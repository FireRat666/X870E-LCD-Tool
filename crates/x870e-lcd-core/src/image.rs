//! Image processing, scaling, and JPEG compression for the 720x1280 LCD panel.

use std::path::Path;
use image::{imageops::FilterType, DynamicImage, GenericImageView, Rgb, RgbImage};
use thiserror::Error;

use crate::protocol::{PANEL_HEIGHT, PANEL_WIDTH};

#[derive(Error, Debug)]
pub enum ImageError {
    #[error("Failed to load image: {0}")]
    Load(#[from] image::ImageError),
    #[error("Failed to encode JPEG: {0}")]
    Encode(#[from] jpeg_encoder::EncodingError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FitMode {
    /// Scale to fill the 720x1280 screen, center-cropping any excess (default)
    #[default]
    Cover,
    /// Scale to fit inside 720x1280, letterboxing / pillarboxing with black bars
    Fit,
    /// Force stretch to exact 720x1280
    Stretch,
}

/// Prepares any dynamic image for the 720x1280 panel according to the chosen fit mode.
pub fn process_image(img: &DynamicImage, mode: FitMode) -> RgbImage {
    let (_src_w, _src_h) = img.dimensions();
    let target_w = PANEL_WIDTH;
    let target_h = PANEL_HEIGHT;

    match mode {
        FitMode::Stretch => img.resize_exact(target_w, target_h, FilterType::Lanczos3).to_rgb8(),
        FitMode::Fit => {
            let resized = img.resize(target_w, target_h, FilterType::Lanczos3);
            let mut canvas = RgbImage::from_pixel(target_w, target_h, Rgb([0, 0, 0]));
            let (rw, rh) = resized.dimensions();
            let offset_x = (target_w.saturating_sub(rw)) / 2;
            let offset_y = (target_h.saturating_sub(rh)) / 2;
            image::imageops::overlay(&mut canvas, &resized.to_rgb8(), offset_x as i64, offset_y as i64);
            canvas
        }
        FitMode::Cover => {
            let resized = img.resize_to_fill(target_w, target_h, FilterType::Lanczos3);
            resized.to_rgb8()
        }
    }
}

/// Encodes an RGB image to a baseline JPEG byte buffer using YUV 4:2:0 subsampling
/// matching the motherboard panel's hardware JPEG decoder.
pub fn encode_to_jpeg(img: &RgbImage, quality: u8) -> Result<Vec<u8>, ImageError> {
    let mut buffer = Vec::new();
    let mut encoder = jpeg_encoder::Encoder::new(&mut buffer, quality.min(100));
    // The embedded LCD MCU's hardware decoder expects YUV 4:2:0 macroblocks (2x2 chroma subsampling)
    encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::R_4_2_0);
    encoder.encode(
        img.as_raw(),
        img.width() as u16,
        img.height() as u16,
        jpeg_encoder::ColorType::Rgb,
    )?;
    patch_jfif_component_ids(&mut buffer);
    Ok(buffer)
}

/// Patches JPEG byte stream to ensure standard JFIF component IDs (1=Y, 2=Cb, 3=Cr)
/// in SOF0 and SOS markers, as strictly required by the STM32H7 hardware JPEG decoder.
pub fn patch_jfif_component_ids(jpeg: &mut [u8]) {
    let mut i = 0;
    while i + 1 < jpeg.len() {
        if jpeg[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = jpeg[i + 1];
        // Skip SOI, EOI, RST markers which have no length
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        if i + 3 >= jpeg.len() {
            break;
        }
        let len = u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]) as usize;
        if marker == 0xC0 {
            // SOF0: Baseline DCT
            // Offset 4: precision (8), 5..7: height, 7..9: width, 9: num_components
            if i + 9 < jpeg.len() {
                let num_components = jpeg[i + 9] as usize;
                for c in 0..num_components {
                    let comp_offset = i + 10 + c * 3;
                    if comp_offset < jpeg.len() {
                        // Map component IDs 0, 1, 2 to 1, 2, 3
                        if jpeg[comp_offset] == c as u8 {
                            jpeg[comp_offset] = (c + 1) as u8;
                        }
                    }
                }
            }
        } else if marker == 0xDA {
            // SOS: Start of Scan
            // Offset 4: num_components
            if i + 4 < jpeg.len() {
                let num_components = jpeg[i + 4] as usize;
                for c in 0..num_components {
                    let sel_offset = i + 5 + c * 2;
                    if sel_offset < jpeg.len() {
                        // Map component selectors 0, 1, 2 to 1, 2, 3
                        if jpeg[sel_offset] == c as u8 {
                            jpeg[sel_offset] = (c + 1) as u8;
                        }
                    }
                }
            }
            // Scan data follows SOS, we don't need to scan past this
            break;
        }
        i += 2 + len;
    }
}

/// Computes the normalized crop rectangle [min_x, min_y, max_x, max_y] (all in 0.0..=1.0)
/// representing a 720:1280 aspect ratio region inside an image of dimensions `(img_w, img_h)`.
pub fn calculate_crop_rect(
    img_w: u32,
    img_h: u32,
    norm_center: (f32, f32),
    crop_scale: f32,
) -> (f32, f32, f32, f32) {
    if img_w == 0 || img_h == 0 {
        return (0.0, 0.0, 1.0, 1.0);
    }
    let target_ar = PANEL_WIDTH as f32 / PANEL_HEIGHT as f32; // 720.0 / 1280.0 = 0.5625
    let img_ar = img_w as f32 / img_h as f32;

    let (base_norm_w, base_norm_h) = if img_ar >= target_ar {
        // Image is wider than 9:16 (e.g. landscape or square)
        (target_ar / img_ar, 1.0)
    } else {
        // Image is narrower than 9:16
        (1.0, img_ar / target_ar)
    };

    let scale = crop_scale.clamp(0.05, 1.0);
    let norm_w = (base_norm_w * scale).clamp(0.01, 1.0);
    let norm_h = (base_norm_h * scale).clamp(0.01, 1.0);

    let half_w = norm_w / 2.0;
    let half_h = norm_h / 2.0;

    let cx = norm_center.0.clamp(half_w, 1.0 - half_w);
    let cy = norm_center.1.clamp(half_h, 1.0 - half_h);

    let min_x = (cx - half_w).clamp(0.0, 1.0);
    let min_y = (cy - half_h).clamp(0.0, 1.0);
    let max_x = (cx + half_w).clamp(0.0, 1.0);
    let max_y = (cy + half_h).clamp(0.0, 1.0);

    (min_x, min_y, max_x, max_y)
}

/// Crops a dynamic image using the normalized center and scale, then resizes to the exact 720x1280 hardware dimensions.
pub fn crop_and_scale(
    img: &DynamicImage,
    norm_center: (f32, f32),
    crop_scale: f32,
) -> RgbImage {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return RgbImage::new(PANEL_WIDTH, PANEL_HEIGHT);
    }

    let (min_x, min_y, max_x, max_y) = calculate_crop_rect(w, h, norm_center, crop_scale);

    let px_x = (min_x * w as f32).round().clamp(0.0, (w - 1) as f32) as u32;
    let px_y = (min_y * h as f32).round().clamp(0.0, (h - 1) as f32) as u32;
    let px_w = ((max_x - min_x) * w as f32).round().clamp(1.0, (w - px_x) as f32) as u32;
    let px_h = ((max_y - min_y) * h as f32).round().clamp(1.0, (h - px_y) as f32) as u32;

    let cropped = img.crop_imm(px_x, px_y, px_w, px_h);
    image::imageops::resize(&cropped.to_rgb8(), PANEL_WIDTH, PANEL_HEIGHT, FilterType::Lanczos3)
}

/// Convenience function: Loads any image file from disk and encodes it as a 720x1280 JPEG.
pub fn load_and_prepare_jpeg<P: AsRef<Path>>(path: P, mode: FitMode, quality: u8) -> Result<Vec<u8>, ImageError> {
    let img = image::open(path)?;
    let processed = process_image(&img, mode);
    encode_to_jpeg(&processed, quality)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_and_patch_jfif() {
        let img = RgbImage::new(PANEL_WIDTH, PANEL_HEIGHT);
        let jpeg = encode_to_jpeg(&img, 85).expect("JPEG encoding should succeed");
        
        // Verify SOF0 has components 1, 2, 3
        let sof0_idx = jpeg.windows(2).position(|w| w == [0xFF, 0xC0]).expect("SOF0 should exist");
        assert_eq!(jpeg[sof0_idx + 10], 1, "Component 0 ID should be 1 (Y)");
        assert_eq!(jpeg[sof0_idx + 13], 2, "Component 1 ID should be 2 (Cb)");
        assert_eq!(jpeg[sof0_idx + 16], 3, "Component 2 ID should be 3 (Cr)");

        // Verify SOS has selectors 1, 2, 3
        let sos_idx = jpeg.windows(2).position(|w| w == [0xFF, 0xDA]).expect("SOS should exist");
        assert_eq!(jpeg[sos_idx + 5], 1, "Scan component 0 selector should be 1");
        assert_eq!(jpeg[sos_idx + 7], 2, "Scan component 1 selector should be 2");
        assert_eq!(jpeg[sos_idx + 9], 3, "Scan component 2 selector should be 3");
    }

    #[test]
    fn test_crop_and_scale_dimensions() {
        let img = DynamicImage::ImageRgb8(RgbImage::new(1920, 1080));
        let result = crop_and_scale(&img, (0.5, 0.5), 1.0);
        assert_eq!(result.width(), PANEL_WIDTH);
        assert_eq!(result.height(), PANEL_HEIGHT);

        let zoomed = crop_and_scale(&img, (0.2, 0.8), 0.5);
        assert_eq!(zoomed.width(), PANEL_WIDTH);
        assert_eq!(zoomed.height(), PANEL_HEIGHT);
    }

    #[test]
    fn test_calculate_crop_rect_clamping() {
        let (min_x, min_y, max_x, max_y) = calculate_crop_rect(1920, 1080, (-5.0, 5.0), 1.0);
        assert!(min_x >= 0.0 && min_x <= 1.0);
        assert!(min_y >= 0.0 && min_y <= 1.0);
        assert!(max_x >= 0.0 && max_x <= 1.0);
        assert!(max_y >= 0.0 && max_y <= 1.0);
        assert!(max_x > min_x);
        assert!(max_y > min_y);
    }
}
