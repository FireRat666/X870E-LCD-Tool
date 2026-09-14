use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui::{self, Color32, ComboBox, RichText, Slider, Vec2};
use image::DynamicImage;

use x870e_lcd_core::{
    calculate_crop_rect, crop_and_scale, encode_to_jpeg, load_and_prepare_jpeg, process_image,
    DisplayMode, FitMode, HardwareMonitor, HwLayout, LcdDevice, LcdModel, SensorMetric,
    SlotCatalog, SlotEntry, TelemetrySnapshot,
};

#[derive(PartialEq, Eq, Clone, Copy)]
enum ActiveTab {
    Display,
    Telemetry,
    ImageUpload,
}

enum UploadEvent {
    Progress(String),
    ImageFlashed { slot: u8, mode_name: String, msg: String },
    SlotErased { slot: u8, msg: String },
    Error(String),
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum CropDragHandle {
    None,
    Inside,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

pub struct CropModalState {
    pub image_path: PathBuf,
    pub file_name: String,
    pub original_image: DynamicImage,
    pub source_texture: egui::TextureHandle,
    pub preview_texture: Option<egui::TextureHandle>,
    pub target_slot: u8,
    pub fit_mode: FitMode,
    pub norm_center: (f32, f32),
    pub crop_scale: f32,
    pub jpeg_quality: u8,
    pub active_drag: CropDragHandle,
    pub drag_start_mouse: egui::Pos2,
    pub drag_start_center: (f32, f32),
    pub drag_start_scale: f32,
    pub preview_dirty: bool,
    pub estimated_kb: usize,
}

impl CropModalState {
    pub fn generate_final_rgb(&self) -> image::RgbImage {
        match self.fit_mode {
            FitMode::Cover => {
                crop_and_scale(&self.original_image, self.norm_center, self.crop_scale)
            }
            FitMode::Fit => {
                process_image(&self.original_image, FitMode::Fit)
            }
            FitMode::Stretch => {
                process_image(&self.original_image, FitMode::Stretch)
            }
        }
    }

    pub fn update_preview(&mut self, ctx: &egui::Context) {
        let final_rgb = self.generate_final_rgb();
        if let Ok(jpeg_bytes) = encode_to_jpeg(&final_rgb, self.jpeg_quality) {
            self.estimated_kb = (jpeg_bytes.len() + 512) / 1024;
        }
        let size = [final_rgb.width() as _, final_rgb.height() as _];
        let col_img = egui::ColorImage::from_rgb(size, final_rgb.as_raw());
        self.preview_texture = Some(ctx.load_texture("crop_modal_preview", col_img, Default::default()));
        self.preview_dirty = false;
    }
}

fn draw_dashed_line(
    painter: &egui::Painter,
    start: egui::Pos2,
    end: egui::Pos2,
    dash_len: f32,
    gap_len: f32,
    stroke: egui::Stroke,
) {
    let delta = end - start;
    let len = delta.length();
    if len <= 0.0 {
        return;
    }
    let dir = delta / len;
    let step = dash_len + gap_len;
    let mut dist = 0.0;
    while dist < len {
        let seg_end = (dist + dash_len).min(len);
        painter.line_segment([start + dir * dist, start + dir * seg_end], stroke);
        dist += step;
    }
}

fn draw_dotted_rect(
    painter: &egui::Painter,
    rect: egui::Rect,
    stroke_width: f32,
    color: Color32,
    under_color: Color32,
) {
    painter.rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(stroke_width, under_color),
        egui::StrokeKind::Middle,
    );

    let dash = 6.0;
    let gap = 5.0;
    let stroke = egui::Stroke::new(stroke_width, color);

    draw_dashed_line(painter, rect.left_top(), rect.right_top(), dash, gap, stroke);
    draw_dashed_line(painter, rect.right_top(), rect.right_bottom(), dash, gap, stroke);
    draw_dashed_line(painter, rect.right_bottom(), rect.left_bottom(), dash, gap, stroke);
    draw_dashed_line(painter, rect.left_bottom(), rect.left_top(), dash, gap, stroke);
}

pub struct LcdGuiApp {
    device: Option<Arc<Mutex<LcdDevice>>>,
    connected_model: Option<LcdModel>,
    connection_error: Option<String>,

    active_tab: ActiveTab,
    brightness: u8,
    standby_wallpaper: bool,
    display_on: bool,
    current_mode: String,

    // Temperature Warning Setting
    temp_warning_enabled: bool,
    temp_warning_threshold: u32, // 75, 80, 85, 90, 95, 100

    // Animation Mode
    selected_animation: u8,

    // Default Wallpapers (Presets 0..=5)
    selected_default_wallpaper: u8,

    // Image Upload, Catalog & Custom Flash Slots
    catalog: SlotCatalog,
    thumbnail_textures: HashMap<u8, Option<egui::TextureHandle>>,
    advanced_mode: bool,
    delete_confirm: Option<u8>,

    selected_custom_slot: u8,
    selected_image_path: Option<PathBuf>,
    preview_texture: Option<egui::TextureHandle>,
    raw_preview_img: Option<DynamicImage>,
    crop_modal: Option<CropModalState>,
    fit_mode: FitMode,
    jpeg_quality: u8,
    upload_status: Option<(String, bool)>, // (message, is_error)
    is_flashing: bool,
    upload_tx: Sender<UploadEvent>,
    upload_rx: Receiver<UploadEvent>,

    // Telemetry State
    hw_layout: HwLayout,
    hw_theme: u8,
    slot_metrics: [SensorMetric; 5],
    hwmon: HardwareMonitor,
    latest_snapshot: TelemetrySnapshot,
    telemetry_streaming: bool,
    last_telemetry_tick: std::time::Instant,
}

impl LcdGuiApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // ROG Dark Theme
        let mut visuals = egui::Visuals::dark();
        visuals.override_text_color = Some(Color32::from_rgb(235, 235, 235));
        visuals.widgets.active.bg_fill = Color32::from_rgb(220, 20, 60); // ROG Red
        visuals.widgets.hovered.bg_fill = Color32::from_rgb(180, 20, 50);
        visuals.selection.bg_fill = Color32::from_rgb(220, 20, 60);
        cc.egui_ctx.set_visuals(visuals);

        let (device, connected_model, connection_error) = match LcdDevice::open() {
            Ok(dev) => {
                let model = dev.model();
                (Some(Arc::new(Mutex::new(dev))), Some(model), None)
            }
            Err(e) => (None, None, Some(e.to_string())),
        };

        let mut hwmon = HardwareMonitor::new();
        let snapshot = hwmon.refresh();
        let (upload_tx, upload_rx) = channel();
        let catalog = SlotCatalog::load();

        Self {
            device,
            connected_model,
            connection_error,
            active_tab: ActiveTab::Display,
            brightness: 100,
            standby_wallpaper: false,
            display_on: true,
            current_mode: "Hardware Monitor".to_string(),
            temp_warning_enabled: false,
            temp_warning_threshold: 75,
            selected_animation: 0,
            selected_default_wallpaper: 0,
            catalog,
            thumbnail_textures: HashMap::new(),
            advanced_mode: false,
            delete_confirm: None,
            selected_custom_slot: 0,
            selected_image_path: None,
            preview_texture: None,
            raw_preview_img: None,
            crop_modal: None,
            fit_mode: FitMode::Cover,
            jpeg_quality: 92,
            upload_status: None,
            is_flashing: false,
            upload_tx,
            upload_rx,
            hw_layout: HwLayout::Triple,
            hw_theme: 2,
            slot_metrics: [
                SensorMetric::CpuTemp,
                SensorMetric::GpuTemp,
                SensorMetric::RamUsage,
                SensorMetric::CpuUsage,
                SensorMetric::CpuFreq,
            ],
            hwmon,
            latest_snapshot: snapshot,
            telemetry_streaming: false,
            last_telemetry_tick: std::time::Instant::now(),
        }
    }

    fn try_reconnect(&mut self) {
        match LcdDevice::open() {
            Ok(dev) => {
                self.connected_model = Some(dev.model());
                self.device = Some(Arc::new(Mutex::new(dev)));
                self.connection_error = None;
            }
            Err(e) => {
                self.device = None;
                self.connected_model = None;
                self.connection_error = Some(e.to_string());
            }
        }
    }

    fn set_brightness(&mut self, level: u8) {
        self.brightness = level;
        if let Some(dev) = &self.device {
            if let Ok(dev) = dev.lock() {
                let _ = dev.set_brightness_config(level, self.standby_wallpaper);
            }
        }
    }

    fn set_standby_wallpaper(&mut self, enabled: bool) {
        self.standby_wallpaper = enabled;
        if let Some(dev) = &self.device {
            if let Ok(dev) = dev.lock() {
                let _ = dev.set_brightness_config(self.brightness, enabled);
            }
        }
    }

    fn set_mode(&mut self, mode: DisplayMode, mode_name: &str) {
        self.current_mode = mode_name.to_string();
        if let Some(dev) = &self.device {
            if let Ok(dev) = dev.lock() {
                let _ = dev.set_mode(mode);
            }
        }
    }

    fn apply_hw_layout(&mut self) {
        if let Some(dev) = &self.device {
            if let Ok(dev) = dev.lock() {
                let _ = dev.init_hw_monitor(self.hw_layout, self.hw_theme);
            }
        }
    }

    #[allow(dead_code)]
    fn load_image(&mut self, path: PathBuf, ctx: &egui::Context) {
        if let Ok(img) = image::open(&path) {
            let processed = process_image(&img, self.fit_mode);
            let size = [processed.width() as _, processed.height() as _];
            let color_image = egui::ColorImage::from_rgb(size, processed.as_raw());
            self.preview_texture = Some(ctx.load_texture("lcd_preview", color_image, Default::default()));
            self.raw_preview_img = Some(img);
            self.selected_image_path = Some(path);
            self.upload_status = None;
        }
    }

    #[allow(dead_code)]
    fn load_image_from_bytes(&mut self, bytes: &[u8], name: &str, ctx: &egui::Context) {
        if let Ok(img) = image::load_from_memory(bytes) {
            let processed = process_image(&img, self.fit_mode);
            let size = [processed.width() as _, processed.height() as _];
            let color_image = egui::ColorImage::from_rgb(size, processed.as_raw());
            self.preview_texture = Some(ctx.load_texture("lcd_preview", color_image, Default::default()));
            self.raw_preview_img = Some(img);
            let scratch = std::env::temp_dir().join(format!("asus_lcd_drop_{name}"));
            let _ = std::fs::write(&scratch, bytes);
            self.selected_image_path = Some(scratch);
            self.upload_status = None;
        }
    }

    fn refresh_preview_texture(&mut self, ctx: &egui::Context) {
        if let Some(img) = &self.raw_preview_img {
            let processed = process_image(img, self.fit_mode);
            let size = [processed.width() as _, processed.height() as _];
            let color_image = egui::ColorImage::from_rgb(size, processed.as_raw());
            self.preview_texture = Some(ctx.load_texture("lcd_preview", color_image, Default::default()));
        }
    }

    fn get_or_load_thumbnail(&mut self, slot: u8, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        if let Some(opt_tex) = self.thumbnail_textures.get(&slot) {
            return opt_tex.clone();
        }
        let thumb_path = self.catalog.thumbnail_path(slot);
        let loaded = if thumb_path.exists() {
            if let Ok(img) = image::open(&thumb_path) {
                let rgb = img.to_rgb8();
                let size = [rgb.width() as _, rgb.height() as _];
                let color_image = egui::ColorImage::from_rgb(size, rgb.as_raw());
                Some(ctx.load_texture(format!("thumb_{slot}"), color_image, Default::default()))
            } else {
                None
            }
        } else {
            None
        };
        self.thumbnail_textures.insert(slot, loaded.clone());
        loaded
    }

    fn flash_custom_image(&mut self, path: PathBuf, target_slot: u8, fit: FitMode, quality: u8) {
        let tx = self.upload_tx.clone();
        let device = self.device.clone();
        self.is_flashing = true;
        self.upload_status = Some((format!("Encoding hardware-compliant JPEG for Slot {target_slot}..."), false));

        std::thread::spawn(move || {
            let _ = tx.send(UploadEvent::Progress("Encoding compliant JPEG...".to_string()));
            match load_and_prepare_jpeg(&path, fit, quality) {
                Ok(jpeg) => {
                    let _ = tx.send(UploadEvent::Progress(format!("Uploading {} bytes to Slot {} SPI flash...", jpeg.len(), target_slot)));
                    if let Some(dev) = device {
                        if let Ok(dev) = dev.lock() {
                            match dev.upload_and_display_jpeg(&jpeg, target_slot) {
                                Ok(_) => {
                                    if let Ok(dyn_img) = image::open(&path) {
                                        let filename = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| format!("Slot {target_slot}"));
                                        let mut cat = SlotCatalog::load();
                                        let entry = SlotEntry::new(target_slot, &filename, jpeg.len());
                                        let _ = cat.add_slot(entry, &dyn_img);
                                    }
                                    let _ = tx.send(UploadEvent::ImageFlashed {
                                        slot: target_slot,
                                        mode_name: format!("Custom Slot {target_slot}"),
                                        msg: format!("✓ Image successfully flashed to Slot {} and active on screen!", target_slot),
                                    });
                                }
                                Err(e) => {
                                    let _ = tx.send(UploadEvent::Error(format!("Upload error: {e}")));
                                }
                            }
                        } else {
                            let _ = tx.send(UploadEvent::Error("Failed to lock device mutex".to_string()));
                        }
                    } else {
                        let _ = tx.send(UploadEvent::Error("Device disconnected".to_string()));
                    }
                }
                Err(e) => {
                    let _ = tx.send(UploadEvent::Error(format!("Image preparation error: {e}")));
                }
            }
        });
    }

    fn erase_custom_slot(&mut self, slot: u8) {
        let tx = self.upload_tx.clone();
        let device = self.device.clone();
        self.is_flashing = true;
        self.upload_status = Some((format!("Erasing Slot {} from SPI flash...", slot), false));

        std::thread::spawn(move || {
            if let Some(dev) = device {
                if let Ok(dev) = dev.lock() {
                    match dev.delete_custom_image_slot(slot) {
                        Ok(_) => {
                            let mut cat = SlotCatalog::load();
                            let _ = cat.remove_slot(slot);
                            let _ = tx.send(UploadEvent::SlotErased {
                                slot,
                                msg: format!("✓ Slot {} erased from SPI flash and catalog.", slot),
                            });
                        }
                        Err(e) => {
                            let _ = tx.send(UploadEvent::Error(format!("Erase error: {e}")));
                        }
                    }
                } else {
                    let _ = tx.send(UploadEvent::Error("Failed to lock device".to_string()));
                }
            } else {
                let _ = tx.send(UploadEvent::Error("Device disconnected".to_string()));
            }
        });
    }

    fn open_crop_modal(&mut self, path: PathBuf, target_slot: u8, ctx: &egui::Context) {
        if let Ok(dyn_img) = image::open(&path) {
            let file_name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| format!("Slot {target_slot}"));
            let rgb = dyn_img.to_rgb8();
            let size = [rgb.width() as _, rgb.height() as _];
            let col_img = egui::ColorImage::from_rgb(size, rgb.as_raw());
            let source_texture = ctx.load_texture("crop_source", col_img, Default::default());

            let mut state = CropModalState {
                image_path: path.clone(),
                file_name,
                original_image: dyn_img,
                source_texture,
                preview_texture: None,
                target_slot,
                fit_mode: FitMode::Cover,
                norm_center: (0.5, 0.5),
                crop_scale: 1.0,
                jpeg_quality: self.jpeg_quality,
                active_drag: CropDragHandle::None,
                drag_start_mouse: egui::Pos2::ZERO,
                drag_start_center: (0.5, 0.5),
                drag_start_scale: 1.0,
                preview_dirty: true,
                estimated_kb: 0,
            };
            state.update_preview(ctx);
            self.selected_image_path = Some(path);
            self.crop_modal = Some(state);
        }
    }

    fn open_crop_modal_from_bytes(&mut self, bytes: &[u8], name: &str, target_slot: u8, ctx: &egui::Context) {
        let scratch = std::env::temp_dir().join(format!("asus_lcd_drop_{name}"));
        if std::fs::write(&scratch, bytes).is_ok() {
            self.open_crop_modal(scratch, target_slot, ctx);
        }
    }

    fn flash_prepared_image(&mut self, rgb: image::RgbImage, target_slot: u8, quality: u8, title: String) {
        let tx = self.upload_tx.clone();
        let device = self.device.clone();
        self.is_flashing = true;
        self.selected_custom_slot = target_slot;
        self.upload_status = Some((format!("Encoding hardware-compliant JPEG for Slot {target_slot}..."), false));
        self.raw_preview_img = Some(DynamicImage::ImageRgb8(rgb.clone()));

        std::thread::spawn(move || {
            let _ = tx.send(UploadEvent::Progress("Encoding compliant JPEG...".to_string()));
            match encode_to_jpeg(&rgb, quality) {
                Ok(jpeg) => {
                    let _ = tx.send(UploadEvent::Progress(format!("Uploading {} bytes to Slot {} SPI flash...", jpeg.len(), target_slot)));
                    if let Some(dev) = device {
                        if let Ok(dev) = dev.lock() {
                            match dev.upload_and_display_jpeg(&jpeg, target_slot) {
                                Ok(_) => {
                                    let dyn_img = DynamicImage::ImageRgb8(rgb);
                                    let mut cat = SlotCatalog::load();
                                    let entry = SlotEntry::new(target_slot, &title, jpeg.len());
                                    let _ = cat.add_slot(entry, &dyn_img);

                                    let _ = tx.send(UploadEvent::ImageFlashed {
                                        slot: target_slot,
                                        mode_name: format!("Custom Slot {target_slot}"),
                                        msg: format!("✓ Image successfully flashed to Slot {} and active on screen!", target_slot),
                                    });
                                }
                                Err(e) => {
                                    let _ = tx.send(UploadEvent::Error(format!("Upload error: {e}")));
                                }
                            }
                        } else {
                            let _ = tx.send(UploadEvent::Error("Failed to lock device mutex".to_string()));
                        }
                    } else {
                        let _ = tx.send(UploadEvent::Error("Device disconnected".to_string()));
                    }
                }
                Err(e) => {
                    let _ = tx.send(UploadEvent::Error(format!("JPEG encoding error: {e}")));
                }
            }
        });
    }

    fn render_crop_modal(&mut self, ctx: &egui::Context) {
        let mut close_modal = false;
        let mut flash_action = None;

        if let Some(state) = &mut self.crop_modal {
            let modal_title = format!("🖼 Crop & Adjust Custom Image (Target: Slot {})", state.target_slot);
            let mut is_open = true;

            egui::Window::new(modal_title)
                .open(&mut is_open)
                .collapsible(false)
                .resizable(true)
                .default_size(Vec2::new(920.0, 600.0))
                .min_size(Vec2::new(760.0, 520.0))
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().id_salt("crop_modal_scroll").show(ui, |ui| {
                        ui.horizontal(|ui| {
                            // Left Column: Interactive Framing Canvas
                            ui.vertical(|ui| {
                                ui.label(RichText::new("Interactive Crop Framing").strong());
                                ui.label(RichText::new("Drag inside dotted box to move • Drag corners or scroll wheel to zoom").size(11.0).color(Color32::from_rgb(170, 170, 185)));
                                ui.add_space(4.0);

                                let max_canvas_w = 460.0;
                                let max_canvas_h = 320.0;
                                let img_w = state.original_image.width() as f32;
                                let img_h = state.original_image.height() as f32;
                                let img_ratio = img_w / img_h;

                                let (display_w, display_h) = if img_ratio >= (max_canvas_w / max_canvas_h) {
                                    (max_canvas_w, max_canvas_w / img_ratio)
                                } else {
                                    (max_canvas_h * img_ratio, max_canvas_h)
                                };

                                let (canvas_rect, response) = ui.allocate_exact_size(Vec2::new(display_w, display_h), egui::Sense::click_and_drag());

                                // Draw source image
                                ui.painter().image(
                                    state.source_texture.id(),
                                    canvas_rect,
                                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                    Color32::WHITE,
                                );

                                // Crop Box
                                let (norm_min_x, norm_min_y, norm_max_x, norm_max_y) = calculate_crop_rect(
                                    state.original_image.width(),
                                    state.original_image.height(),
                                    state.norm_center,
                                    state.crop_scale,
                                );

                                let crop_screen_rect = egui::Rect::from_min_max(
                                    canvas_rect.min + egui::vec2(norm_min_x * display_w, norm_min_y * display_h),
                                    canvas_rect.min + egui::vec2(norm_max_x * display_w, norm_max_y * display_h),
                                );

                                if state.fit_mode == FitMode::Cover {
                                    // Dimmed outside regions
                                    let dim = Color32::from_rgba_unmultiplied(0, 0, 0, 170);
                                    ui.painter().rect_filled(egui::Rect::from_min_max(canvas_rect.left_top(), egui::pos2(canvas_rect.right(), crop_screen_rect.top())), 0.0, dim);
                                    ui.painter().rect_filled(egui::Rect::from_min_max(egui::pos2(canvas_rect.left(), crop_screen_rect.bottom()), canvas_rect.right_bottom()), 0.0, dim);
                                    ui.painter().rect_filled(egui::Rect::from_min_max(egui::pos2(canvas_rect.left(), crop_screen_rect.top()), egui::pos2(crop_screen_rect.left(), crop_screen_rect.bottom())), 0.0, dim);
                                    ui.painter().rect_filled(egui::Rect::from_min_max(egui::pos2(crop_screen_rect.right(), crop_screen_rect.top()), egui::pos2(canvas_rect.right(), crop_screen_rect.bottom())), 0.0, dim);

                                    // High contrast dashed border
                                    draw_dotted_rect(ui.painter(), crop_screen_rect, 2.0_f32, Color32::WHITE, Color32::from_rgb(20, 20, 20));

                                    // Corner handles
                                    let handle_len = 14.0_f32;
                                    let corner_stroke = egui::Stroke::new(3.0_f32, Color32::from_rgb(220, 20, 60));
                                    let c = crop_screen_rect;
                                    ui.painter().line_segment([c.left_top(), c.left_top() + egui::vec2(handle_len, 0.0)], corner_stroke);
                                    ui.painter().line_segment([c.left_top(), c.left_top() + egui::vec2(0.0, handle_len)], corner_stroke);
                                    ui.painter().line_segment([c.right_top(), c.right_top() + egui::vec2(-handle_len, 0.0)], corner_stroke);
                                    ui.painter().line_segment([c.right_top(), c.right_top() + egui::vec2(0.0, handle_len)], corner_stroke);
                                    ui.painter().line_segment([c.left_bottom(), c.left_bottom() + egui::vec2(handle_len, 0.0)], corner_stroke);
                                    ui.painter().line_segment([c.left_bottom(), c.left_bottom() + egui::vec2(0.0, -handle_len)], corner_stroke);
                                    ui.painter().line_segment([c.right_bottom(), c.right_bottom() + egui::vec2(-handle_len, 0.0)], corner_stroke);
                                    ui.painter().line_segment([c.right_bottom(), c.right_bottom() + egui::vec2(0.0, -handle_len)], corner_stroke);

                                    // Center crosshair
                                    let center = crop_screen_rect.center();
                                    let cross_stroke = egui::Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 120));
                                    ui.painter().line_segment([center - egui::vec2(8.0, 0.0), center + egui::vec2(8.0, 0.0)], cross_stroke);
                                    ui.painter().line_segment([center - egui::vec2(0.0, 8.0), center + egui::vec2(0.0, 8.0)], cross_stroke);
                                } else {
                                    let mode_label = match state.fit_mode {
                                        FitMode::Fit => "Fit Mode: Entire image letterboxed to 720x1280",
                                        FitMode::Stretch => "Stretch Mode: Entire image stretched to 720x1280",
                                        _ => "",
                                    };
                                    ui.painter().rect_stroke(canvas_rect, 0.0, egui::Stroke::new(2.0_f32, Color32::from_rgb(100, 180, 255)), egui::StrokeKind::Outside);
                                    ui.painter().text(canvas_rect.center(), egui::Align2::CENTER_CENTER, mode_label, egui::FontId::proportional(13.0), Color32::WHITE);
                                }

                                ui.painter().rect_stroke(canvas_rect, 2.0_f32, egui::Stroke::new(1.0_f32, Color32::from_rgb(60, 60, 75)), egui::StrokeKind::Outside);

                                // Cursor & mouse input
                                if state.fit_mode == FitMode::Cover {
                                    let pointer_pos = ctx.input(|i| i.pointer.latest_pos());
                                    let corner_thresh = 16.0;

                                    if let Some(pos) = pointer_pos {
                                        if canvas_rect.contains(pos) {
                                            let near_tl = pos.distance(crop_screen_rect.left_top()) < corner_thresh;
                                            let near_tr = pos.distance(crop_screen_rect.right_top()) < corner_thresh;
                                            let near_bl = pos.distance(crop_screen_rect.left_bottom()) < corner_thresh;
                                            let near_br = pos.distance(crop_screen_rect.right_bottom()) < corner_thresh;

                                            if near_tl || near_br {
                                                ctx.set_cursor_icon(egui::CursorIcon::ResizeNorthWest);
                                            } else if near_tr || near_bl {
                                                ctx.set_cursor_icon(egui::CursorIcon::ResizeNorthEast);
                                            } else if crop_screen_rect.contains(pos) {
                                                ctx.set_cursor_icon(if response.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
                                            }
                                        }
                                    }

                                    // Scroll wheel zoom
                                    let scroll_delta = ctx.input(|i| i.raw_scroll_delta);
                                    if response.hovered() && scroll_delta.y != 0.0 {
                                        let change = scroll_delta.y * 0.002;
                                        state.crop_scale = (state.crop_scale + change).clamp(0.08, 1.0);
                                        state.preview_dirty = true;
                                    }

                                    if response.drag_started() {
                                        if let Some(pos) = pointer_pos {
                                            let near_tl = pos.distance(crop_screen_rect.left_top()) < corner_thresh;
                                            let near_tr = pos.distance(crop_screen_rect.right_top()) < corner_thresh;
                                            let near_bl = pos.distance(crop_screen_rect.left_bottom()) < corner_thresh;
                                            let near_br = pos.distance(crop_screen_rect.right_bottom()) < corner_thresh;

                                            if near_tl {
                                                state.active_drag = CropDragHandle::TopLeft;
                                            } else if near_tr {
                                                state.active_drag = CropDragHandle::TopRight;
                                            } else if near_bl {
                                                state.active_drag = CropDragHandle::BottomLeft;
                                            } else if near_br {
                                                state.active_drag = CropDragHandle::BottomRight;
                                            } else if crop_screen_rect.contains(pos) {
                                                state.active_drag = CropDragHandle::Inside;
                                            } else {
                                                state.active_drag = CropDragHandle::None;
                                            }
                                            state.drag_start_mouse = pos;
                                            state.drag_start_center = state.norm_center;
                                            state.drag_start_scale = state.crop_scale;
                                        }
                                    }

                                    if response.dragged() {
                                        match state.active_drag {
                                            CropDragHandle::Inside => {
                                                let delta = response.drag_delta();
                                                let norm_dx = delta.x / display_w;
                                                let norm_dy = delta.y / display_h;
                                                state.norm_center.0 += norm_dx;
                                                state.norm_center.1 += norm_dy;
                                                state.preview_dirty = true;
                                            }
                                            CropDragHandle::TopLeft | CropDragHandle::TopRight | CropDragHandle::BottomLeft | CropDragHandle::BottomRight => {
                                                let delta = response.drag_delta();
                                                let scale_dir = match state.active_drag {
                                                    CropDragHandle::BottomRight => (delta.x + delta.y) * 0.5,
                                                    CropDragHandle::TopLeft => (-delta.x - delta.y) * 0.5,
                                                    CropDragHandle::TopRight => (delta.x - delta.y) * 0.5,
                                                    CropDragHandle::BottomLeft => (-delta.x + delta.y) * 0.5,
                                                    _ => 0.0,
                                                };
                                                let scale_change = scale_dir / display_w.min(display_h);
                                                state.crop_scale = (state.crop_scale + scale_change).clamp(0.08, 1.0);
                                                state.preview_dirty = true;
                                            }
                                            CropDragHandle::None => {}
                                        }
                                    }

                                    if response.drag_stopped() {
                                        state.active_drag = CropDragHandle::None;
                                    }
                                }

                                ui.add_space(6.0);
                                ui.horizontal(|ui| {
                                    if ui.button("⛶ Center Crop").clicked() {
                                        state.norm_center = (0.5, 0.5);
                                        state.preview_dirty = true;
                                    }
                                    if ui.button("🔍 Max Fit (100%)").clicked() {
                                        state.crop_scale = 1.0;
                                        state.preview_dirty = true;
                                    }
                                    if state.fit_mode == FitMode::Cover {
                                        ui.add_space(8.0);
                                        ui.label(RichText::new("Zoom:").size(11.0));
                                        if ui.add(Slider::new(&mut state.crop_scale, 0.1..=1.0).show_value(false)).changed() {
                                            state.preview_dirty = true;
                                        }
                                        ui.label(RichText::new(format!("{:.0}%", state.crop_scale * 100.0)).monospace().size(11.0));
                                    }
                                });
                            });

                            ui.add_space(16.0);
                            ui.separator();
                            ui.add_space(16.0);

                            // Right Column: Live 720x1280 LCD Preview
                            ui.vertical(|ui| {
                                ui.label(RichText::new("Live 720x1280 LCD Preview").strong());
                                ui.label(RichText::new("Direct hardware preview (5\" 720x1280 panel)").size(11.0).color(Color32::from_rgb(170, 170, 185)));
                                ui.add_space(4.0);

                                let prev_w = 180.0;
                                let prev_h = prev_w / (720.0 / 1280.0);
                                let (prev_rect, _resp) = ui.allocate_exact_size(Vec2::new(prev_w, prev_h), egui::Sense::hover());
                                ui.painter().rect_filled(prev_rect, 4.0, Color32::BLACK);

                                if let Some(prev_tex) = &state.preview_texture {
                                    ui.painter().image(prev_tex.id(), prev_rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                                }
                                ui.painter().rect_stroke(prev_rect, 4.0, egui::Stroke::new(2.0_f32, Color32::from_rgb(220, 20, 60)), egui::StrokeKind::Outside);

                                ui.add_space(6.0);
                                ui.label(RichText::new(format!("Destination: Slot {}", state.target_slot)).strong().color(Color32::from_rgb(100, 200, 255)));
                                ui.label(RichText::new(format!("File: {}", state.file_name)).size(11.0).color(Color32::from_rgb(200, 200, 215)));
                                ui.label(RichText::new(format!("Source: {}", state.image_path.display())).size(9.0).italics().color(Color32::from_rgb(140, 140, 155)));
                                ui.label(RichText::new(format!("Estimated Size: ~{} KB", state.estimated_kb)).monospace().size(11.0).color(Color32::from_rgb(180, 220, 180)));
                                ui.label(RichText::new("Encoder: JFIF YUV 4:2:0 STM32H7").size(10.0).italics().color(Color32::from_rgb(150, 150, 160)));
                            });
                        });

                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(8.0);

                        // Mode and Compression controls
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Mode:").strong());
                            let prev_fit = state.fit_mode;
                            ui.radio_value(&mut state.fit_mode, FitMode::Cover, "Cover (Crop & Pan)");
                            ui.radio_value(&mut state.fit_mode, FitMode::Fit, "Fit (Letterbox)");
                            ui.radio_value(&mut state.fit_mode, FitMode::Stretch, "Stretch");
                            if prev_fit != state.fit_mode {
                                state.preview_dirty = true;
                            }

                            ui.add_space(20.0);
                            ui.label(RichText::new("JPEG Quality:").strong());
                            let prev_q = state.jpeg_quality;
                            if ui.add(Slider::new(&mut state.jpeg_quality, 50..=100).text("%").suffix("%")).changed() {
                                if prev_q != state.jpeg_quality {
                                    state.preview_dirty = true;
                                }
                            }
                        });

                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Flash to Slot:").strong());
                            ui.add(egui::DragValue::new(&mut state.target_slot).range(0..=255).prefix("Slot "));
                            let is_occ = self.catalog.is_occupied(state.target_slot);
                            if is_occ {
                                ui.label(RichText::new("(Will overwrite existing image in this slot)").color(Color32::from_rgb(255, 180, 80)).size(11.0));
                            } else {
                                ui.label(RichText::new("(Free slot)").color(Color32::from_rgb(100, 220, 120)).size(11.0));
                            }
                        });

                        ui.add_space(12.0);

                        // Action Buttons
                        ui.horizontal(|ui| {
                            if ui.button(RichText::new("Cancel").size(14.0)).clicked() {
                                close_modal = true;
                            }

                            ui.add_space(12.0);
                            let can_flash = !self.is_flashing && self.device.is_some();
                            let flash_btn_text = if self.is_flashing {
                                "⏳ Flashing in progress...".to_string()
                            } else {
                                format!("⚡ Flash Image to Motherboard LCD (Slot {})", state.target_slot)
                            };

                            let flash_btn = egui::Button::new(RichText::new(flash_btn_text).color(Color32::WHITE).strong().size(14.0))
                                .fill(Color32::from_rgb(220, 20, 60));

                            if ui.add_enabled(can_flash, flash_btn).clicked() {
                                flash_action = Some((state.generate_final_rgb(), state.target_slot, state.jpeg_quality, state.file_name.clone()));
                            }
                        });

                        if state.preview_dirty {
                            state.update_preview(ctx);
                        }
                    });
                });

            if !is_open {
                close_modal = true;
            }
        }

        if let Some((rgb, slot, quality, title)) = flash_action {
            let size = [rgb.width() as _, rgb.height() as _];
            let col_img = egui::ColorImage::from_rgb(size, rgb.as_raw());
            self.preview_texture = Some(ctx.load_texture("lcd_preview", col_img, Default::default()));
            self.crop_modal = None;
            self.flash_prepared_image(rgb, slot, quality, title);
        } else if close_modal {
            self.crop_modal = None;
        }
    }

    /// Renders the top application status bar and notice banners.
    fn render_header(&mut self, ctx: &egui::Context) {
    egui::TopBottomPanel::top("header")
        .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 8)).fill(Color32::from_rgb(16, 16, 22)))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                // ROG-red accent bar on the left edge
                let accent_rect = egui::Rect::from_min_size(
                    ui.cursor().min,
                    Vec2::new(3.0, 28.0),
                );
                ui.painter().rect_filled(accent_rect, 1.0, Color32::from_rgb(220, 20, 60));
                ui.add_space(10.0);

                ui.heading(RichText::new("ROG X870E LCD TOOL").color(Color32::from_rgb(220, 20, 60)).strong().size(17.0));
                ui.add_space(6.0);
                ui.label(RichText::new("ROG Crosshair X870E Series  |  5\" LCD Panel Manager").color(Color32::from_rgb(140, 140, 155)).size(12.0).italics());

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(model) = self.connected_model {
                        ui.label(RichText::new(format!("●  Connected ({})", model.display_name())).color(Color32::from_rgb(50, 205, 50)).strong().size(12.0));
                    } else {
                        if ui.add(egui::Button::new(RichText::new("⟳  Reconnect").size(12.0)).min_size(Vec2::new(90.0, 24.0))).clicked() {
                            self.try_reconnect();
                        }
                        ui.add_space(6.0);
                        ui.label(RichText::new("●  Disconnected").color(Color32::from_rgb(220, 20, 60)).strong().size(12.0));
                    }
                });
            });
        });


    if let Some(err) = &self.connection_error {
        egui::TopBottomPanel::top("error_banner").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("⚠ Notice:").color(Color32::from_rgb(255, 180, 0)).strong());
                ui.label(format!("{err}"));
            });
            ui.add_space(4.0);
        });
    }

    // Left Panel: 720x1280 LCD Preview Canvas

    }

    /// Renders the left sidebar displaying the LCD preview canvas and live status.
    fn render_preview_panel(&mut self, ctx: &egui::Context) {
    egui::SidePanel::left("preview_panel")
        .resizable(false)
        .exact_width(240.0)
        .frame(egui::Frame::new().fill(Color32::from_rgb(18, 18, 26)).inner_margin(egui::Margin::symmetric(12, 10)))
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().id_salt("preview_scroll").show(ui, |ui| {
                // Sidebar title
                ui.label(RichText::new("LCD PREVIEW").size(11.0).monospace().color(Color32::from_rgb(130, 130, 150)));
                ui.add_space(6.0);

                let aspect_ratio = 720.0 / 1280.0;
                let preview_width = 210.0;
                let preview_height = preview_width / aspect_ratio;

                let (rect, _response) = ui.allocate_exact_size(Vec2::new(preview_width, preview_height), egui::Sense::hover());
                ui.painter().rect_filled(rect, 6.0, Color32::from_rgb(10, 10, 16));

                if let Some(texture) = &self.preview_texture {
                    ui.painter().image(texture.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                } else {
                    let center = rect.center();
                    ui.painter().text(
                        egui::pos2(center.x, center.y - 40.0),
                        egui::Align2::CENTER_CENTER,
                        "5\" ROG DISPLAY",
                        egui::FontId::proportional(14.0),
                        Color32::from_rgb(140, 140, 160),
                    );
                    ui.painter().text(
                        egui::pos2(center.x, center.y - 10.0),
                        egui::Align2::CENTER_CENTER,
                        &self.current_mode,
                        egui::FontId::proportional(13.0),
                        Color32::from_rgb(220, 20, 60),
                    );

                    // Show active metrics on preview
                    if self.current_mode.contains("Monitor") || self.telemetry_streaming {
                        let count = self.hw_layout.slot_count();
                        let is_warning = self.temp_warning_enabled && (
                            self.latest_snapshot.cpu_temp_c.map(|t| t as u32 >= self.temp_warning_threshold).unwrap_or(false)
                            || self.latest_snapshot.gpu_temp_c.map(|t| t as u32 >= self.temp_warning_threshold).unwrap_or(false)
                        );

                        if is_warning {
                            ui.painter().text(
                                egui::pos2(center.x, center.y + 12.0),
                                egui::Align2::CENTER_CENTER,
                                format!("⚠ OVERHEAT ALERT (>={}°C)", self.temp_warning_threshold),
                                egui::FontId::proportional(11.0),
                                Color32::from_rgb(255, 60, 60),
                            );
                        }

                        for i in 0..count {
                            let (lbl, val) = if is_warning && i == 0 {
                                ("TEMP WARN", format!("{:.1}\u{2103} !", self.latest_snapshot.cpu_temp_c.unwrap_or(0.0)))
                            } else {
                                self.slot_metrics[i].format(&self.latest_snapshot)
                            };
                            ui.painter().text(
                                egui::pos2(center.x, center.y + 35.0 + (i as f32 * 20.0)),
                                egui::Align2::CENTER_CENTER,
                                format!("{lbl}: {val}"),
                                egui::FontId::monospace(11.0),
                                if is_warning && i == 0 { Color32::from_rgb(255, 100, 100) } else { Color32::from_rgb(200, 200, 220) },
                            );
                        }
                    }
                }

                // ROG-red corner accents on preview frame
                let corner_len = 12.0;
                let corner_color = Color32::from_rgb(220, 20, 60);
                let cw = 2.0_f32;
                ui.painter().line_segment([rect.min, egui::pos2(rect.min.x + corner_len, rect.min.y)], egui::Stroke::new(cw, corner_color));
                ui.painter().line_segment([rect.min, egui::pos2(rect.min.x, rect.min.y + corner_len)], egui::Stroke::new(cw, corner_color));
                ui.painter().line_segment([rect.right_top(), egui::pos2(rect.right_top().x - corner_len, rect.right_top().y)], egui::Stroke::new(cw, corner_color));
                ui.painter().line_segment([rect.right_top(), egui::pos2(rect.right_top().x, rect.right_top().y + corner_len)], egui::Stroke::new(cw, corner_color));
                ui.painter().line_segment([rect.left_bottom(), egui::pos2(rect.left_bottom().x + corner_len, rect.left_bottom().y)], egui::Stroke::new(cw, corner_color));
                ui.painter().line_segment([rect.left_bottom(), egui::pos2(rect.left_bottom().x, rect.left_bottom().y - corner_len)], egui::Stroke::new(cw, corner_color));
                ui.painter().line_segment([rect.max, egui::pos2(rect.max.x - corner_len, rect.max.y)], egui::Stroke::new(cw, corner_color));
                ui.painter().line_segment([rect.max, egui::pos2(rect.max.x, rect.max.y - corner_len)], egui::Stroke::new(cw, corner_color));
                ui.painter().rect_stroke(rect, 6.0, egui::Stroke::new(1.0_f32, Color32::from_rgb(45, 45, 60)), egui::StrokeKind::Outside);

                ui.add_space(8.0);

                // Active mode badge
                let mode_text = self.current_mode.clone();
                let mode_frame = egui::Frame::new()
                    .fill(Color32::from_rgb(28, 18, 24))
                    .stroke(egui::Stroke::new(1.0_f32, Color32::from_rgb(90, 20, 38)))
                    .inner_margin(egui::Margin::symmetric(8, 4))
                    .corner_radius(4.0);
                mode_frame.show(ui, |ui| {
                    ui.set_min_width(preview_width - 4.0);
                    ui.label(RichText::new(format!("▶  {mode_text}")).size(11.0).color(Color32::from_rgb(220, 80, 100)));
                });

                ui.add_space(6.0);

                // Connection status
                if self.device.is_some() {
                    ui.label(RichText::new("●  USB Connected").size(11.0).color(Color32::from_rgb(50, 200, 80)));
                } else {
                    ui.label(RichText::new("●  Not Connected").size(11.0).color(Color32::from_rgb(180, 50, 50)));
                }
                ui.label(RichText::new("720 × 1280 px  (Portrait)").italics().size(10.0).color(Color32::from_rgb(100, 100, 120)));
                ui.add_space(8.0);
            });
        });

    // Main Panel: Control Tabs

    }

    /// Renders the Display and Settings tab content.
    fn render_display_tab(&mut self, ui: &mut egui::Ui) {
        let section_frame = section_frame();

        // ── Section: Power & Backlight ────────────────────────────
        section_frame.show(ui, |ui| {
            ui.label(RichText::new("POWER & BACKLIGHT").size(11.0).monospace().color(Color32::from_rgb(120, 120, 145)));
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                ui.label(RichText::new("Display Power:").size(13.0));
                ui.add_space(8.0);
                let (btn_text, btn_color) = if self.display_on {
                    ("  ■  Turn Screen OFF  ", Color32::from_rgb(180, 40, 40))
                } else {
                    ("  ▶  Turn Screen ON  ", Color32::from_rgb(30, 140, 60))
                };
                if ui.add(egui::Button::new(RichText::new(btn_text).size(12.0).strong()).fill(btn_color).corner_radius(4.0).min_size(Vec2::new(0.0, 26.0))).clicked() {
                    self.display_on = !self.display_on;
                    if self.display_on {
                        self.apply_hw_layout();
                        self.current_mode = "Hardware Monitor".to_string();
                    } else {
                        self.set_mode(DisplayMode::Off, "Off");
                    }
                }
            });

            ui.add_space(10.0);
            ui.label(RichText::new("Backlight Brightness").size(13.0));
            ui.add_space(4.0);
            let mut b = self.brightness;
            let slider_width = (ui.available_width() - 16.0).min(500.0).max(200.0);
            if ui.add_sized(Vec2::new(slider_width, 20.0), Slider::new(&mut b, 0..=100).suffix("%").show_value(true)).changed() {
                self.set_brightness(b);
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                for pct in [25u8, 50, 75, 100] {
                    if ui.add(egui::Button::new(RichText::new(format!("{pct}%")).size(11.0)).min_size(Vec2::new(42.0, 22.0))).clicked() {
                        self.set_brightness(pct);
                    }
                }
            });
        });

        ui.add_space(8.0);

        // ── Section: Sleep & Standby ──────────────────────────────
        section_frame.show(ui, |ui| {
            ui.label(RichText::new("SLEEP & STANDBY").size(11.0).monospace().color(Color32::from_rgb(120, 120, 145)));
            ui.add_space(6.0);
            let mut standby = self.standby_wallpaper;
            if ui.checkbox(&mut standby, RichText::new("Keep wallpaper displayed during sleep / hibernate / soft-off states").size(13.0)).changed() {
                self.set_standby_wallpaper(standby);
            }
            ui.add_space(4.0);
            ui.label(RichText::new("Hardware-level setting (USB Cmd 0x5c, byte 16). When enabled, keeps the default wallpaper lit on 5V standby power while the PC sleeps. When disabled, the display turns off.").italics().size(11.0).color(Color32::from_rgb(140, 140, 158)));
        });

        ui.add_space(8.0);

        // ── Section: Temperature Alert ────────────────────────────
        section_frame.show(ui, |ui| {
            ui.label(RichText::new("TEMPERATURE WARNING ALERT").size(11.0).monospace().color(Color32::from_rgb(120, 120, 145)));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.temp_warning_enabled, RichText::new("Enable overheat warning").size(13.0));
                ui.add_space(16.0);
                ui.label(RichText::new("Alert at:").size(12.0).color(Color32::from_rgb(180, 180, 200)));
                ComboBox::from_id_salt("temp_warning_thresh")
                    .selected_text(format!("{} °C", self.temp_warning_threshold))
                    .width(90.0)
                    .show_ui(ui, |ui| {
                        for &t in &[75u32, 80, 85, 90, 95, 100] {
                            ui.selectable_value(&mut self.temp_warning_threshold, t, format!("{t} °C"));
                        }
                    });
            });
            if let Some(t) = self.latest_snapshot.cpu_temp_c {
                ui.add_space(4.0);
                let temp_color = if t as u32 >= self.temp_warning_threshold {
                    Color32::from_rgb(255, 80, 80)
                } else {
                    Color32::from_rgb(100, 200, 120)
                };
                ui.label(RichText::new(format!("Current CPU: {t:.1}°C   |   Alert at ≥ {}°C", self.temp_warning_threshold)).size(11.0).color(temp_color));
            }
        });

        ui.add_space(8.0);

        // ── Section: Switch Display Mode ──────────────────────────
        section_frame.show(ui, |ui| {
            ui.label(RichText::new("SWITCH DISPLAY MODE").size(11.0).monospace().color(Color32::from_rgb(120, 120, 145)));
            ui.add_space(8.0);

            // Hardware Monitor Mode
            ui.horizontal(|ui| {
                ui.label(RichText::new("Hardware Monitor:").size(12.0).color(Color32::from_rgb(180, 180, 200)));
                ui.add_space(8.0);
                let prev_layout = self.hw_layout;
                ComboBox::from_id_salt("disp_layout_select")
                    .selected_text(match self.hw_layout {
                        HwLayout::Single => "Single Gauge",
                        HwLayout::Dual   => "Dual Info",
                        HwLayout::Triple => "Triple Info",
                        HwLayout::Multi  => "Multi Info",
                    })
                    .width(120.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.hw_layout, HwLayout::Single, "Single Gauge (1 Metric)");
                        ui.selectable_value(&mut self.hw_layout, HwLayout::Dual,   "Dual Info (2 Metrics)");
                        ui.selectable_value(&mut self.hw_layout, HwLayout::Triple, "Triple Info (3 Metrics)");
                        ui.selectable_value(&mut self.hw_layout, HwLayout::Multi,  "Multi Info (5 Metrics)");
                    });
                if self.hw_layout == HwLayout::Multi && prev_layout != HwLayout::Multi {
                    self.hw_theme = 3;
                }
                if self.hw_layout == HwLayout::Multi {
                    self.hw_theme = 3;
                    ui.label(RichText::new("Theme 3 (forced)").size(11.0).color(Color32::from_rgb(140, 140, 160)));
                } else {
                    ComboBox::from_id_salt("disp_theme_select")
                        .selected_text(match self.hw_theme {
                            1 => "Theme 1 — Classic ROG",
                            2 => "Theme 2 — Cyber Blue",
                            3 => "Theme 3 — Multi-Info",
                            4 => "Theme 4 — Space Orbit",
                            _ => "Theme ?",
                        })
                        .width(165.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.hw_theme, 1, "Theme 1 — Classic ROG Red");
                            ui.selectable_value(&mut self.hw_theme, 2, "Theme 2 — Cyber Blue HUD");
                            ui.selectable_value(&mut self.hw_theme, 3, "Theme 3 — Multi-Info Gauge");
                            ui.selectable_value(&mut self.hw_theme, 4, "Theme 4 — Space Orbit Ring");
                        });
                }
                if ui.add(egui::Button::new(RichText::new("Apply").size(12.0)).min_size(Vec2::new(60.0, 24.0))).clicked() {
                    self.apply_hw_layout();
                    self.current_mode = "Hardware Monitor".to_string();
                }
            });

            ui.add_space(6.0);

            // Factory Wallpapers
            ui.horizontal(|ui| {
                ui.label(RichText::new("Factory Wallpaper (ROM):").size(12.0).color(Color32::from_rgb(180, 180, 200)));
                ui.add_space(8.0);
                ComboBox::from_id_salt("default_wp_select")
                    .selected_text(format!("Preset {}", self.selected_default_wallpaper))
                    .width(130.0)
                    .show_ui(ui, |ui| {
                        for p in 0u8..=5 {
                            ui.selectable_value(&mut self.selected_default_wallpaper, p, format!("Preset {p}"));
                        }
                    });
                if ui.add(egui::Button::new(RichText::new("Apply").size(12.0)).min_size(Vec2::new(60.0, 24.0))).clicked() {
                    let p = self.selected_default_wallpaper;
                    self.set_mode(DisplayMode::DefaultWallpaper(p), &format!("Default Wallpaper {p}"));
                }
            });

            ui.add_space(6.0);

            // Built-in Animations
            ui.horizontal(|ui| {
                ui.label(RichText::new("Built-in Animation:").size(12.0).color(Color32::from_rgb(180, 180, 200)));
                ui.add_space(8.0);
                ComboBox::from_id_salt("anim_select")
                    .selected_text(format!("Animation {}", self.selected_animation))
                    .width(130.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.selected_animation, 0, "Animation 0 (ROG Starlight)");
                        ui.selectable_value(&mut self.selected_animation, 1, "Animation 1 (ROG Glitch Neon)");
                    });
                if ui.add(egui::Button::new(RichText::new("Apply").size(12.0)).min_size(Vec2::new(60.0, 24.0))).clicked() {
                    self.set_mode(DisplayMode::Animation(self.selected_animation), &format!("Animation {}", self.selected_animation));
                }
            });

            ui.add_space(6.0);

            // Custom Flash Slot
            ui.horizontal(|ui| {
                ui.label(RichText::new("Custom Flash Slot:").size(12.0).color(Color32::from_rgb(180, 180, 200)));
                ui.add_space(8.0);
                ComboBox::from_id_salt("disp_custom_slot_select")
                    .selected_text(format!("Slot {}", self.selected_custom_slot))
                    .width(130.0)
                    .show_ui(ui, |ui| {
                        for s in 0u8..=255 {
                            let tag = if self.catalog.is_occupied(s) { " [Stored]" } else { "" };
                            ui.selectable_value(&mut self.selected_custom_slot, s, format!("Slot {s}{tag}"));
                        }
                    });
                ui.add(egui::DragValue::new(&mut self.selected_custom_slot).range(0..=255u8).prefix("Slot "));
                if ui.add(egui::Button::new(RichText::new("Apply").size(12.0)).min_size(Vec2::new(60.0, 24.0))).clicked() {
                    let slot = self.selected_custom_slot;
                    self.set_mode(DisplayMode::CustomSlot(slot), &format!("Custom Slot {slot}"));
                }
            });
        });
    }

    /// Renders the Hardware Telemetry tab content.
    fn render_telemetry_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Hardware Monitor Configuration");
        ui.label("Configure gauge layout, visual theme (Styles 1..4), and sensor assignments.");
        ui.add_space(8.0);

        // Layout & Theme Selector
        ui.horizontal(|ui| {
            ui.label("Layout Mode:");
            let prev_layout = self.hw_layout;
            ComboBox::from_id_salt("layout_select")
                .selected_text(match self.hw_layout {
                    HwLayout::Single => "Single Gauge (1 Metric)",
                    HwLayout::Dual => "Dual Info (2 Metrics)",
                    HwLayout::Triple => "Triple Info (3 Metrics)",
                    HwLayout::Multi => "Multi Info (5 Metrics)",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.hw_layout, HwLayout::Single, "Single Gauge (1 Metric)");
                    ui.selectable_value(&mut self.hw_layout, HwLayout::Dual, "Dual Info (2 Metrics)");
                    ui.selectable_value(&mut self.hw_layout, HwLayout::Triple, "Triple Info (3 Metrics)");
                    ui.selectable_value(&mut self.hw_layout, HwLayout::Multi, "Multi Info (5 Metrics)");
                });

            if self.hw_layout == HwLayout::Multi && prev_layout != HwLayout::Multi {
                self.hw_theme = 3;
            }

            ui.add_space(10.0);
            ui.label("Visual Theme:");
            if self.hw_layout == HwLayout::Multi {
                self.hw_theme = 3;
                ui.label(RichText::new("Theme Style 3 (Multi-Info Gauge)").color(Color32::from_rgb(100, 200, 255)).strong());
            } else {
                ComboBox::from_id_salt("theme_select")
                    .selected_text(match self.hw_theme {
                        1 => "Theme Style 1 (Classic ROG Red)".to_string(),
                        2 => "Theme Style 2 (Cyber Blue HUD)".to_string(),
                        3 => "Theme Style 3 (Multi-Info Gauge)".to_string(),
                        4 => "Theme Style 4 (Space Orbit Ring)".to_string(),
                        _ => format!("Theme Style {}", self.hw_theme),
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.hw_theme, 1, "Theme Style 1 (Classic ROG Red)");
                        ui.selectable_value(&mut self.hw_theme, 2, "Theme Style 2 (Cyber Blue HUD)");
                        ui.selectable_value(&mut self.hw_theme, 3, "Theme Style 3 (Multi-Info Gauge)");
                        ui.selectable_value(&mut self.hw_theme, 4, "Theme Style 4 (Space Orbit Ring)");
                    });
            }

            if ui.button("Apply Layout & Theme").clicked() {
                self.apply_hw_layout();
            }
        });

        ui.add_space(10.0);
        ui.heading("Sensor Slot Assignments");
        let slot_count = self.hw_layout.slot_count();
        for slot in 0..slot_count {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Slot {}:", slot)).strong());
                ComboBox::from_id_salt(format!("slot_metric_{slot}"))
                    .selected_text(self.slot_metrics[slot].display_name())
                    .show_ui(ui, |ui| {
                        for m in SensorMetric::all() {
                            ui.selectable_value(&mut self.slot_metrics[slot], *m, m.display_name());
                        }
                    });
                
                // Show current live value preview
                let (lbl, val) = self.slot_metrics[slot].format(&self.latest_snapshot);
                ui.label(RichText::new(format!("Preview: [{lbl}] {val}")).monospace().color(Color32::from_rgb(180, 180, 200)));
            });
        }

        ui.add_space(14.0);
        let stream_btn_text = if self.telemetry_streaming {
            RichText::new("⏹ Stop Live Screen Streaming").color(Color32::from_rgb(255, 80, 80)).strong()
        } else {
            RichText::new("▶ Start Live Streaming to Motherboard LCD").color(Color32::from_rgb(50, 220, 50)).strong()
        };

        if ui.button(stream_btn_text).clicked() {
            self.telemetry_streaming = !self.telemetry_streaming;
            if self.telemetry_streaming {
                self.apply_hw_layout();
                self.current_mode = "Hardware Monitor".to_string();
            }
        }
    }

    /// Renders the Wallpapers and Custom Image Upload tab content.
    fn render_image_upload_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // Modal delete confirmation dialog
        if let Some(slot_to_delete) = self.delete_confirm {
            egui::Window::new("⚠️ Confirm Delete Image")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.add_space(4.0);
                    let title = self.catalog.slots.get(&slot_to_delete).map(|e| e.title.as_str()).unwrap_or("Custom Image");
                    ui.label(RichText::new(format!("Are you sure you want to permanently erase Slot {} (\"{title}\") from the motherboard's SPI flash memory?", slot_to_delete)).strong());
                    ui.add_space(4.0);
                    ui.label(RichText::new("This will erase the image from hardware SPI storage and remove it from your catalog.").size(11.0).color(Color32::from_rgb(200, 160, 100)));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        let can_erase = !self.is_flashing && self.device.is_some();
                        if ui.add_enabled(can_erase, egui::Button::new(RichText::new(format!("🗑 Yes, Erase Slot {slot_to_delete}")).color(Color32::from_rgb(255, 90, 90)).strong())).clicked() {
                            self.delete_confirm = None;
                            self.erase_custom_slot(slot_to_delete);
                        }
                        if ui.button("Cancel").clicked() {
                            self.delete_confirm = None;
                        }
                    });
                    ui.add_space(4.0);
                });
        }

        // Header: Mode Switcher & Capacity Stats
        ui.horizontal(|ui| {
            ui.heading("Wallpapers & Custom Images");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.checkbox(&mut self.advanced_mode, "⚙ Advanced Slot Mode");
                let occupied = self.catalog.occupied_count();
                let cap_text = format!("Storage: {} / 255 Custom Slots Used", occupied);
                ui.label(RichText::new(cap_text).monospace().color(Color32::from_rgb(180, 180, 200)));
            });
        });
        ui.label("Display factory built-in wallpapers or upload custom images to the motherboard's 64MB SPI flash.");
        ui.add_space(10.0);

        // 1. Factory Built-in Wallpapers (ROM Presets 0..=5)
        ui.label(RichText::new("Factory Default Wallpapers (ROM)").strong());
        ui.add_space(4.0);
        let presets: [u8; 6] = [0, 1, 2, 3, 4, 5];

        egui::ScrollArea::horizontal().id_salt("default_wallpapers_scroll").show(ui, |ui| {
            ui.horizontal(|ui| {
                for p in presets {
                    let is_active = self.current_mode == format!("Default Wallpaper {p}");
                    let card_w = 90.0;
                    let card_h = 160.0;
                    let (rect, response) = ui.allocate_exact_size(Vec2::new(card_w, card_h), egui::Sense::click());

                    let bg_color = if is_active {
                        Color32::from_rgb(45, 20, 28)
                    } else if response.hovered() {
                        Color32::from_rgb(38, 38, 50)
                    } else {
                        Color32::from_rgb(26, 26, 36)
                    };
                    let border_color = if is_active {
                        Color32::from_rgb(220, 20, 60)
                    } else if response.hovered() {
                        Color32::from_rgb(140, 140, 160)
                    } else {
                        Color32::from_rgb(60, 60, 75)
                    };
                    let stroke_w = if is_active { 2.0_f32 } else { 1.0_f32 };

                    ui.painter().rect_filled(rect, 6.0, bg_color);
                    ui.painter().rect_stroke(rect, 6.0, egui::Stroke::new(stroke_w, border_color), egui::StrokeKind::Outside);

                    let center = rect.center();
                    ui.painter().text(
                        egui::pos2(center.x, rect.min.y + 14.0),
                        egui::Align2::CENTER_CENTER,
                        format!("PRESET {p}"),
                        egui::FontId::monospace(10.0),
                        Color32::from_rgb(100, 180, 255),
                    );

                    ui.painter().text(
                        egui::pos2(center.x, center.y),
                        egui::Align2::CENTER_CENTER,
                        "ROG",
                        egui::FontId::proportional(24.0),
                        if is_active { Color32::from_rgb(255, 60, 90) } else { Color32::from_rgb(170, 170, 190) },
                    );

                    if response.clicked() {
                        self.selected_default_wallpaper = p;
                        self.set_mode(DisplayMode::DefaultWallpaper(p), &format!("Default Wallpaper {p}"));
                    }
                    response.on_hover_text(format!("ROM Preset {p} — click to display on LCD"));
                    ui.add_space(4.0);
                }
            });
        });

        ui.add_space(14.0);

        // 2. Custom Flashed Images (SPI Flash Storage)
        ui.horizontal(|ui| {
            ui.label(RichText::new("Custom Images (Motherboard SPI Storage)").strong());
            let free_slot = self.catalog.next_free_slot(256);
            if let Some(next) = free_slot {
                ui.label(RichText::new(format!("(Next Free: Slot {next})")).size(11.0).color(Color32::from_rgb(100, 200, 120)));
            } else {
                ui.label(RichText::new("(All 255 Slots Full)").size(11.0).color(Color32::from_rgb(255, 80, 80)));
            }
        });
        ui.add_space(4.0);

        let mut slot_to_delete = None;
        let mut action_add_image = false;

        egui::ScrollArea::horizontal().id_salt("custom_images_scroll").show(ui, |ui| {
            ui.horizontal(|ui| {
                // 2a. Installed Custom Image Cards
                let slots: Vec<(u8, x870e_lcd_core::SlotEntry)> = self.catalog.slots.iter().map(|(k, v)| (*k, v.clone())).collect();
                for (slot, entry) in slots {
                    let is_active = self.current_mode == format!("Custom Slot {slot}");
                    let card_w = 90.0;
                    let card_h = 160.0;
                    let (rect, response) = ui.allocate_exact_size(Vec2::new(card_w, card_h), egui::Sense::click());

                    let bg_color = if is_active {
                        Color32::from_rgb(45, 20, 28)
                    } else if response.hovered() {
                        Color32::from_rgb(38, 38, 50)
                    } else {
                        Color32::from_rgb(26, 26, 36)
                    };
                    let border_color = if is_active {
                        Color32::from_rgb(220, 20, 60)
                    } else if response.hovered() {
                        Color32::from_rgb(140, 140, 160)
                    } else {
                        Color32::from_rgb(60, 60, 75)
                    };
                    let stroke_w = if is_active { 2.0_f32 } else { 1.0_f32 };

                    ui.painter().rect_filled(rect, 6.0, bg_color);

                    // Thumbnail fills most of the card, leaving 30px footer for labels
                    if let Some(tex) = self.get_or_load_thumbnail(slot, ctx) {
                        let thumb_rect = egui::Rect::from_min_max(
                            egui::pos2(rect.min.x + 5.0, rect.min.y + 5.0),
                            egui::pos2(rect.max.x - 5.0, rect.max.y - 30.0),
                        );
                        ui.painter().image(tex.id(), thumb_rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                    } else {
                        let center = rect.center();
                        ui.painter().text(
                            egui::pos2(center.x, center.y - 15.0),
                            egui::Align2::CENTER_CENTER,
                            "🖼",
                            egui::FontId::proportional(28.0),
                            Color32::from_rgb(140, 140, 160),
                        );
                    }

                    ui.painter().rect_stroke(rect, 6.0, egui::Stroke::new(stroke_w, border_color), egui::StrokeKind::Outside);

                    // Title & Slot label at bottom (30px footer)
                    let display_title = if entry.title.len() > 11 {
                        format!("{}…", &entry.title[..10])
                    } else {
                        entry.title.clone()
                    };
                    ui.painter().text(
                        egui::pos2(rect.center().x, rect.max.y - 19.0),
                        egui::Align2::CENTER_CENTER,
                        display_title,
                        egui::FontId::proportional(11.0),
                        Color32::WHITE,
                    );
                    ui.painter().text(
                        egui::pos2(rect.center().x, rect.max.y - 7.0),
                        egui::Align2::CENTER_CENTER,
                        format!("Slot {slot}  •  {:.0}KB", entry.file_size_bytes as f64 / 1024.0),
                        egui::FontId::monospace(9.0),
                        Color32::from_rgb(150, 150, 165),
                    );

                    // Hover delete button "X" in top right corner
                    let btn_size = 20.0;
                    let btn_rect = egui::Rect::from_min_size(
                        egui::pos2(rect.max.x - btn_size - 4.0, rect.min.y + 4.0),
                        Vec2::new(btn_size, btn_size),
                    );
                    let delete_btn_resp = ui.allocate_rect(btn_rect, egui::Sense::click());
                    let btn_hovered = delete_btn_resp.hovered();
                    let btn_bg = if btn_hovered {
                        Color32::from_rgb(220, 20, 50)
                    } else {
                        Color32::from_rgba_premultiplied(40, 40, 50, 200)
                    };
                    ui.painter().rect_filled(btn_rect, 4.0, btn_bg);
                    ui.painter().text(
                        btn_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "✕",
                        egui::FontId::proportional(11.0),
                        Color32::WHITE,
                    );

                    if delete_btn_resp.clicked() {
                        slot_to_delete = Some(slot);
                    } else if response.clicked() {
                        self.selected_custom_slot = slot;
                        self.set_mode(DisplayMode::CustomSlot(slot), &format!("Custom Slot {slot}"));
                    }

                    delete_btn_resp.on_hover_text(format!("Erase custom image Slot {} from SPI flash memory", slot));
                    response.on_hover_text(format!("Click to display \"{}\" (Custom Slot {}) on LCD", entry.title, slot));

                    ui.add_space(6.0);
                }

                // 2b. The "+ Add Image" Card (Puts image in next free slot)
                let free_slot_opt = self.catalog.next_free_slot(256);
                let add_card_w = 90.0;
                let add_card_h = 160.0;
                let (add_rect, add_resp) = ui.allocate_exact_size(Vec2::new(add_card_w, add_card_h), egui::Sense::click());

                let add_bg = if add_resp.hovered() {
                    Color32::from_rgb(35, 45, 40)
                } else {
                    Color32::from_rgb(24, 30, 28)
                };
                let add_border = if add_resp.hovered() {
                    Color32::from_rgb(50, 205, 50)
                } else {
                    Color32::from_rgb(45, 90, 60)
                };
                ui.painter().rect_filled(add_rect, 6.0, add_bg);
                ui.painter().rect_stroke(add_rect, 6.0, egui::Stroke::new(1.0_f32, add_border), egui::StrokeKind::Outside);

                let add_center = add_rect.center();
                ui.painter().text(
                    egui::pos2(add_center.x, add_center.y - 22.0),
                    egui::Align2::CENTER_CENTER,
                    "➕",
                    egui::FontId::proportional(30.0),
                    Color32::from_rgb(50, 220, 80),
                );
                ui.painter().text(
                    egui::pos2(add_center.x, add_center.y + 10.0),
                    egui::Align2::CENTER_CENTER,
                    "Add Image",
                    egui::FontId::proportional(12.0),
                    Color32::WHITE,
                );
                let subtext = match free_slot_opt {
                    Some(s) => format!("Next: Slot {s}"),
                    None => "Full (255/255)".to_string(),
                };
                ui.painter().text(
                    egui::pos2(add_center.x, add_center.y + 26.0),
                    egui::Align2::CENTER_CENTER,
                    subtext,
                    egui::FontId::monospace(9.0),
                    Color32::from_rgb(140, 200, 160),
                );

                if add_resp.clicked() && free_slot_opt.is_some() && !self.is_flashing {
                    action_add_image = true;
                }
                add_resp.on_hover_text("Browse an image file to automatically flash into the next free SPI slot");
            });
        });


        if let Some(slot) = slot_to_delete {
            self.delete_confirm = Some(slot);
        }

        if action_add_image {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp", "gif"])
                .pick_file()
            {
                let slot = self.catalog.next_free_slot(256).unwrap_or(self.selected_custom_slot);
                self.selected_custom_slot = slot;
                self.open_crop_modal(path, slot, ctx);
            }
        }

        // 3. Advanced Mode Controls (visible when advanced_mode is enabled)
        if self.advanced_mode {
            ui.add_space(14.0);
            ui.separator();
            ui.add_space(8.0);
            ui.heading("⚙ Advanced Slot Management & Manual Controls");
            ui.label("Directly target specific slots (0..=255), force overwrite, adjust JPEG quality and scaling modes.");
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                ui.label("Target Destination Slot:");
                ui.add(egui::DragValue::new(&mut self.selected_custom_slot).range(0..=255).prefix("Slot "));
                ComboBox::from_id_salt("adv_upload_slot_select")
                    .selected_text(format!("Custom Slot {} (SPI Flash)", self.selected_custom_slot))
                    .show_ui(ui, |ui| {
                        for s in 0..=255 {
                            let tag = if self.catalog.is_occupied(s) { " [Occupied]" } else { " [Free]" };
                            ui.selectable_value(&mut self.selected_custom_slot, s, format!("Custom Slot {s}{tag}"));
                        }
                    });

                ui.add_space(15.0);
                let can_erase = !self.is_flashing && self.device.is_some();
                if ui.add_enabled(can_erase, egui::Button::new(RichText::new(format!("🗑 Erase Slot {}", self.selected_custom_slot)).color(Color32::from_rgb(255, 100, 100)))).clicked() {
                    self.delete_confirm = Some(self.selected_custom_slot);
                }
            });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("📂 Browse Image File...").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp", "gif"])
                        .pick_file()
                    {
                        self.open_crop_modal(path, self.selected_custom_slot, ctx);
                    }
                }
                if let Some(path) = &self.selected_image_path {
                    ui.label(format!("File: {}", path.file_name().unwrap_or_default().to_string_lossy()));
                    if ui.button("✂ Crop & Adjust Framing...").clicked() {
                        self.open_crop_modal(path.clone(), self.selected_custom_slot, ctx);
                    }
                }
            });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Scaling Mode:");
                let prev_fit = self.fit_mode;
                ui.radio_value(&mut self.fit_mode, FitMode::Cover, "Cover");
                ui.radio_value(&mut self.fit_mode, FitMode::Fit, "Fit");
                ui.radio_value(&mut self.fit_mode, FitMode::Stretch, "Stretch");
                if prev_fit != self.fit_mode {
                    self.refresh_preview_texture(ctx);
                }

                ui.add_space(16.0);
                ui.add(Slider::new(&mut self.jpeg_quality, 50..=100).text("JPEG Quality"));
            });

            ui.add_space(10.0);
            let can_upload = !self.is_flashing && self.selected_image_path.is_some() && self.device.is_some();
            let flash_btn_text = if self.is_flashing {
                format!("⏳ Flashing to Slot {} (SPI Lockstep)...", self.selected_custom_slot)
            } else {
                format!("⚡ Flash Image to Motherboard LCD (Slot {})", self.selected_custom_slot)
            };

            if ui.add_enabled(can_upload, egui::Button::new(RichText::new(flash_btn_text).strong())).clicked() {
                if let Some(path) = &self.selected_image_path {
                    self.flash_custom_image(path.clone(), self.selected_custom_slot, self.fit_mode, self.jpeg_quality);
                }
            }
        }

        if let Some((msg, is_err)) = &self.upload_status {
            ui.add_space(10.0);
            let color = if *is_err { Color32::from_rgb(220, 20, 60) } else { Color32::from_rgb(50, 205, 50) };
            ui.label(RichText::new(msg).color(color).strong());
        }
    }
}


/// Returns the standard styled panel frame for tab configuration sections.
fn section_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(Color32::from_rgb(22, 22, 32))
        .stroke(egui::Stroke::new(1.0_f32, Color32::from_rgb(50, 50, 66)))
        .inner_margin(egui::Margin::symmetric(14, 10))
        .corner_radius(6.0)
}

impl eframe::App for LcdGuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll asynchronous upload and erase events from worker thread
        while let Ok(event) = self.upload_rx.try_recv() {
            match event {
                UploadEvent::Progress(msg) => {
                    self.upload_status = Some((msg, false));
                }
                UploadEvent::ImageFlashed { slot, mode_name, msg } => {
                    self.is_flashing = false;
                    self.catalog = SlotCatalog::load();
                    self.thumbnail_textures.remove(&slot);
                    self.upload_status = Some((msg, false));
                    self.current_mode = mode_name;
                }
                UploadEvent::SlotErased { slot, msg } => {
                    self.is_flashing = false;
                    self.catalog = SlotCatalog::load();
                    self.thumbnail_textures.remove(&slot);
                    self.upload_status = Some((msg, false));
                    self.current_mode = "Default Wallpaper 0".to_string();
                }
                UploadEvent::Error(msg) => {
                    self.is_flashing = false;
                    self.upload_status = Some((msg, true));
                }
            }
        }

        // Drag & drop image support (opens interactive crop & adjust modal)
        ctx.input(|i| {
            if !i.raw.dropped_files.is_empty() {
                for file in &i.raw.dropped_files {
                    let target_slot = self.catalog.next_free_slot(256).unwrap_or(self.selected_custom_slot);
                    if let Some(path) = &file.path {
                        self.open_crop_modal(path.clone(), target_slot, ctx);
                        self.active_tab = ActiveTab::ImageUpload;
                        break;
                    } else if let Some(bytes) = &file.bytes {
                        self.open_crop_modal_from_bytes(bytes, &file.name, target_slot, ctx);
                        self.active_tab = ActiveTab::ImageUpload;
                        break;
                    }
                }
            }
        });

        // Periodic telemetry streaming
        if self.telemetry_streaming && self.last_telemetry_tick.elapsed() >= Duration::from_millis(800) {
            self.last_telemetry_tick = std::time::Instant::now();
            self.latest_snapshot = self.hwmon.refresh();

            let is_warning = self.temp_warning_enabled && (
                self.latest_snapshot.cpu_temp_c.map(|t| t as u32 >= self.temp_warning_threshold).unwrap_or(false)
                || self.latest_snapshot.gpu_temp_c.map(|t| t as u32 >= self.temp_warning_threshold).unwrap_or(false)
            );

            if let Some(dev) = &self.device {
                if let Ok(dev) = dev.lock() {
                    let count = self.hw_layout.slot_count();
                    for slot in 0..count {
                        let metric = self.slot_metrics[slot];
                        let (label, val) = if is_warning && slot == 0 {
                            ("TEMP WARN", format!("{:.1}\u{2103} !", self.latest_snapshot.cpu_temp_c.unwrap_or(0.0)))
                        } else {
                            metric.format(&self.latest_snapshot)
                        };
                        let _ = dev.update_telemetry_slot(slot as u8, label, &val);
                    }
                }
            }
            ctx.request_repaint();
        }

        // Top Status Bar
        self.render_header(ctx);
        self.render_preview_panel(ctx);

        // Main Panel: Control Tabs
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("central_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // Styled pill tab bar
                    ui.horizontal(|ui| {
                        let tabs = [
                            (ActiveTab::Display,     "⚙  Display & Settings"),
                            (ActiveTab::Telemetry,   "📊  Hardware Telemetry"),
                            (ActiveTab::ImageUpload, "🖼  Wallpapers & Images"),
                        ];
                        for (tab, label) in tabs {
                            let is_active = self.active_tab == tab;
                            let btn_color = if is_active {
                                Color32::from_rgb(220, 20, 60)
                            } else {
                                Color32::from_rgb(55, 55, 70)
                            };
                            let txt_color = if is_active {
                                Color32::WHITE
                            } else {
                                Color32::from_rgb(180, 180, 200)
                            };
                            let btn = egui::Button::new(RichText::new(label).size(12.0).color(txt_color))
                                .fill(btn_color)
                                .corner_radius(5.0)
                                .min_size(Vec2::new(0.0, 28.0));
                            if ui.add(btn).clicked() {
                                self.active_tab = tab;
                            }
                            ui.add_space(2.0);
                        }
                    });
                    ui.add_space(10.0);

                    match self.active_tab {
                        ActiveTab::Display => self.render_display_tab(ui),
                        ActiveTab::Telemetry => self.render_telemetry_tab(ui),
                        ActiveTab::ImageUpload => self.render_image_upload_tab(ui, ctx),
                    }
                });
        });

        // Render interactive crop & adjust modal when active
        self.render_crop_modal(ctx);
    }
}
