//! Media configuration, scaling modes, rotation, and frame processing for 720x1280 LCD.

use image::{imageops::FilterType, DynamicImage, GenericImageView, Rgba};
use crate::renderer::{fill_rect_bgra, HEIGHT, WIDTH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleMode {
    Fill,       // Crop to fill 9:16 (no black bars, maintains aspect ratio)
    Fit,        // Letterbox / Pillarbox (black bars, complete content visible)
    Stretch,    // Non-uniform stretch to 720x1280
    CustomCrop, // User interactive pan / zoom framing
}

impl ScaleMode {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Fill => "Fill (Crop to 9:16)",
            Self::Fit => "Fit (Letterbox)",
            Self::Stretch => "Stretch (Full Frame)",
            Self::CustomCrop => "Custom Pan & Zoom",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    Rot0,
    Rot90,
    Rot180,
    Rot270,
}

impl Rotation {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Rot0 => "0° (Default)",
            Self::Rot90 => "90° Clockwise",
            Self::Rot180 => "180° Inverted",
            Self::Rot270 => "270° (90° CCW)",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MediaConfig {
    pub scale_mode: ScaleMode,
    pub rotation: Rotation,
    pub zoom: f32,   // 1.0 = base, 0.5 to 4.0
    pub pan_x: f32,  // -1.0 to 1.0
    pub pan_y: f32,  // -1.0 to 1.0
    pub loop_media: bool,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            scale_mode: ScaleMode::Fill,
            rotation: Rotation::Rot0,
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            loop_media: true,
        }
    }
}

/// Applies rotation, scaling, and panning to `img` and writes a 720x1280 BGRA frame into `buffer`.
pub fn process_image_to_bgra(img: &DynamicImage, config: &MediaConfig, buffer: &mut [u8]) {
    // 1. Rotate
    let rotated: DynamicImage = match config.rotation {
        Rotation::Rot0 => img.clone(),
        Rotation::Rot90 => img.rotate90(),
        Rotation::Rot180 => img.rotate180(),
        Rotation::Rot270 => img.rotate270(),
    };

    let (src_w, src_h) = rotated.dimensions();
    if src_w == 0 || src_h == 0 {
        fill_rect_bgra(buffer, 0, 0, WIDTH, HEIGHT, [0, 0, 0, 0xff]);
        return;
    }

    match config.scale_mode {
        ScaleMode::Stretch => {
            let resized = rotated.resize_exact(WIDTH as u32, HEIGHT as u32, FilterType::Triangle);
            copy_image_to_bgra_buffer(&resized, buffer, 0, 0);
        }
        ScaleMode::Fit => {
            fill_rect_bgra(buffer, 0, 0, WIDTH, HEIGHT, [0, 0, 0, 0xff]);
            let w_ratio = WIDTH as f32 / src_w as f32;
            let h_ratio = HEIGHT as f32 / src_h as f32;
            let ratio = w_ratio.min(h_ratio);

            let new_w = ((src_w as f32 * ratio).round() as u32).clamp(1, WIDTH as u32);
            let new_h = ((src_h as f32 * ratio).round() as u32).clamp(1, HEIGHT as u32);

            let resized = rotated.resize_exact(new_w, new_h, FilterType::Triangle);
            let offset_x = (WIDTH as i32 - new_w as i32) / 2;
            let offset_y = (HEIGHT as i32 - new_h as i32) / 2;
            copy_image_to_bgra_buffer(&resized, buffer, offset_x.max(0) as usize, offset_y.max(0) as usize);
        }
        ScaleMode::Fill => {
            let w_ratio = WIDTH as f32 / src_w as f32;
            let h_ratio = HEIGHT as f32 / src_h as f32;
            let ratio = w_ratio.max(h_ratio);

            let crop_w = (WIDTH as f32 / ratio).round() as u32;
            let crop_h = (HEIGHT as f32 / ratio).round() as u32;

            let crop_w = crop_w.min(src_w);
            let crop_h = crop_h.min(src_h);

            let crop_x = (src_w - crop_w) / 2;
            let crop_y = (src_h - crop_h) / 2;

            let cropped = rotated.crop_imm(crop_x, crop_y, crop_w, crop_h);
            let resized = cropped.resize_exact(WIDTH as u32, HEIGHT as u32, FilterType::Triangle);
            copy_image_to_bgra_buffer(&resized, buffer, 0, 0);
        }
        ScaleMode::CustomCrop => {
            let base_w_ratio = WIDTH as f32 / src_w as f32;
            let base_h_ratio = HEIGHT as f32 / src_h as f32;
            let base_ratio = base_w_ratio.max(base_h_ratio);

            let zoom = config.zoom.clamp(0.2, 5.0);
            let effective_ratio = base_ratio * zoom;

            let crop_w = ((WIDTH as f32 / effective_ratio).round() as u32).clamp(10, src_w);
            let crop_h = ((HEIGHT as f32 / effective_ratio).round() as u32).clamp(10, src_h);

            // Compute center with pan offset
            let max_pan_x = (src_w.saturating_sub(crop_w)) as f32 / 2.0;
            let max_pan_y = (src_h.saturating_sub(crop_h)) as f32 / 2.0;

            let center_x = (src_w as f32) / 2.0 + config.pan_x.clamp(-1.0, 1.0) * max_pan_x;
            let center_y = (src_h as f32) / 2.0 + config.pan_y.clamp(-1.0, 1.0) * max_pan_y;

            let crop_x = (center_x - crop_w as f32 / 2.0).round().clamp(0.0, (src_w.saturating_sub(crop_w)) as f32) as u32;
            let crop_y = (center_y - crop_h as f32 / 2.0).round().clamp(0.0, (src_h.saturating_sub(crop_h)) as f32) as u32;

            let cropped = rotated.crop_imm(crop_x, crop_y, crop_w, crop_h);
            let resized = cropped.resize_exact(WIDTH as u32, HEIGHT as u32, FilterType::Triangle);
            copy_image_to_bgra_buffer(&resized, buffer, 0, 0);
        }
    }
}

fn copy_image_to_bgra_buffer(img: &DynamicImage, buffer: &mut [u8], offset_x: usize, offset_y: usize) {
    let (w, h) = img.dimensions();
    let rgba_img = img.to_rgba8();

    for y in 0..h as usize {
        let dst_y = offset_y + y;
        if dst_y >= HEIGHT {
            break;
        }
        for x in 0..w as usize {
            let dst_x = offset_x + x;
            if dst_x >= WIDTH {
                break;
            }
            let Rgba([r, g, b, a]) = rgba_img.get_pixel(x as u32, y as u32);
            let dst_idx = (dst_y * WIDTH + dst_x) * 4;
            if dst_idx + 3 < buffer.len() {
                buffer[dst_idx] = *b;
                buffer[dst_idx + 1] = *g;
                buffer[dst_idx + 2] = *r;
                buffer[dst_idx + 3] = *a;
            }
        }
    }
}
