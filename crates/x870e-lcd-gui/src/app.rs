use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui::{self, Color32, ComboBox, RichText, Slider, Vec2};
use image::DynamicImage;

use x870e_lcd_core::{
    load_and_prepare_jpeg, process_image, DisplayMode, FitMode, HardwareMonitor, HwLayout,
    LcdDevice, SensorMetric, TelemetrySnapshot,
};

#[derive(PartialEq, Eq, Clone, Copy)]
enum ActiveTab {
    Display,
    Telemetry,
    ImageUpload,
}

enum UploadEvent {
    Progress(String),
    Success(String, String), // (status message, mode name)
    Error(String),
}

pub struct LcdGuiApp {
    device: Option<Arc<Mutex<LcdDevice>>>,
    connection_error: Option<String>,

    active_tab: ActiveTab,
    brightness: u8,
    standby_wallpaper: bool,
    display_on: bool,
    current_mode: String,

    // Temperature Warning Setting (1:1 with ASUS MB Manager)
    temp_warning_enabled: bool,
    temp_warning_threshold: u32, // 75, 80, 85, 90, 95, 100

    // Animation Mode
    selected_animation: u8,

    // Default Wallpapers (Presets 0..=5)
    selected_default_wallpaper: u8,

    // Image Upload & Custom Flash Slots
    selected_custom_slot: u8,
    selected_image_path: Option<PathBuf>,
    preview_texture: Option<egui::TextureHandle>,
    raw_preview_img: Option<DynamicImage>,
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

        let (device, connection_error) = match LcdDevice::open() {
            Ok(dev) => (Some(Arc::new(Mutex::new(dev))), None),
            Err(e) => (None, Some(e.to_string())),
        };

        let mut hwmon = HardwareMonitor::new();
        let snapshot = hwmon.refresh();
        let (upload_tx, upload_rx) = channel();

        Self {
            device,
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
            selected_custom_slot: 0,
            selected_image_path: None,
            preview_texture: None,
            raw_preview_img: None,
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
                self.device = Some(Arc::new(Mutex::new(dev)));
                self.connection_error = None;
            }
            Err(e) => {
                self.device = None;
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
}

impl eframe::App for LcdGuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll asynchronous upload and erase events from worker thread
        while let Ok(event) = self.upload_rx.try_recv() {
            match event {
                UploadEvent::Progress(msg) => {
                    self.upload_status = Some((msg, false));
                }
                UploadEvent::Success(msg, mode_name) => {
                    self.is_flashing = false;
                    self.upload_status = Some((msg, false));
                    self.current_mode = mode_name;
                }
                UploadEvent::Error(msg) => {
                    self.is_flashing = false;
                    self.upload_status = Some((msg, true));
                }
            }
        }

        // Drag & drop image support
        ctx.input(|i| {
            if !i.raw.dropped_files.is_empty() {
                for file in &i.raw.dropped_files {
                    if let Some(path) = &file.path {
                        self.load_image(path.clone(), ctx);
                        self.active_tab = ActiveTab::ImageUpload;
                        break;
                    } else if let Some(bytes) = &file.bytes {
                        self.load_image_from_bytes(bytes, &file.name, ctx);
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
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading(RichText::new("X870E EXTREME LCD TOOL").color(Color32::from_rgb(220, 20, 60)).strong());
                ui.label(RichText::new("5\" LCD Panel Manager").italics());

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.device.is_some() {
                        ui.label(RichText::new("● Connected").color(Color32::from_rgb(50, 205, 50)).strong());
                    } else {
                        if ui.button("⟳ Reconnect").clicked() {
                            self.try_reconnect();
                        }
                        ui.label(RichText::new("● Disconnected").color(Color32::from_rgb(220, 20, 60)).strong());
                    }
                });
            });
            ui.add_space(6.0);
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
        egui::SidePanel::left("preview_panel")
            .resizable(false)
            .exact_width(240.0)
            .show(ctx, |ui| {
                ui.add_space(8.0);
                ui.label(RichText::new("Live Preview (720x1280)").strong());
                ui.separator();

                let aspect_ratio = 720.0 / 1280.0;
                let preview_width = 200.0;
                let preview_height = preview_width / aspect_ratio;

                let (rect, _response) = ui.allocate_exact_size(Vec2::new(preview_width, preview_height), egui::Sense::hover());
                ui.painter().rect_filled(rect, 4.0, Color32::from_rgb(15, 15, 20));

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

                ui.painter().rect_stroke(rect, 4.0, egui::Stroke::new(2.0_f32, Color32::from_rgb(60, 60, 75)), egui::StrokeKind::Outside);
                ui.add_space(8.0);
                ui.label(RichText::new("Panel: 720 × 1280 (Portrait)").italics().size(11.0).color(Color32::from_rgb(140, 140, 155)));
            });

        // Main Panel: Control Tabs
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.active_tab, ActiveTab::Display, "Display & Settings");
                ui.selectable_value(&mut self.active_tab, ActiveTab::Telemetry, "Hardware Telemetry");
                ui.selectable_value(&mut self.active_tab, ActiveTab::ImageUpload, "Custom Images (Flash)");
            });
            ui.separator();
            ui.add_space(8.0);

            match self.active_tab {
                ActiveTab::Display => {
                    ui.heading("Display & Power Management");
                    ui.add_space(6.0);

                    ui.horizontal(|ui| {
                        ui.label("Display Power:");
                        if ui.button(if self.display_on { "Turn Screen OFF" } else { "Turn Screen ON" }).clicked() {
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
                    ui.label("Backlight Brightness:");
                    let mut b = self.brightness;
                    if ui.add(Slider::new(&mut b, 0..=100).text("%").suffix("%")).changed() {
                        self.set_brightness(b);
                    }

                    ui.horizontal(|ui| {
                        if ui.button("25%").clicked() { self.set_brightness(25); }
                        if ui.button("50%").clicked() { self.set_brightness(50); }
                        if ui.button("75%").clicked() { self.set_brightness(75); }
                        if ui.button("100%").clicked() { self.set_brightness(100); }
                    });

                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(6.0);

                    // 1:1 ASUS MB Manager Feature: Standby Wallpaper
                    ui.heading("Sleep & Standby Behavior (1:1 ASUS Parity)");
                    ui.add_space(4.0);
                    let mut standby = self.standby_wallpaper;
                    if ui.checkbox(&mut standby, "When system is in sleep, hibernate or soft off states (Keep Wallpaper Displayed)").changed() {
                        self.set_standby_wallpaper(standby);
                    }
                    ui.label(RichText::new("Hardware-level setting (Cmd 0x5c byte 16): Keeps default wallpaper lit on 5V standby power when PC sleeps. If disabled (0x00), turns display off.").italics().size(11.0).color(Color32::from_rgb(170, 170, 180)));

                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(6.0);

                    // 1:1 ASUS MB Manager Feature: Temperature Warning
                    ui.heading("Temperature Warning Alert (1:1 ASUS Parity)");
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.temp_warning_enabled, "Enable Temperature Warning");
                        ui.add_space(10.0);
                        ui.label("Threshold:");
                        ComboBox::from_id_salt("temp_warning_thresh")
                            .selected_text(format!("{} °C", self.temp_warning_threshold))
                            .show_ui(ui, |ui| {
                                for &t in &[75u32, 80, 85, 90, 95, 100] {
                                    ui.selectable_value(&mut self.temp_warning_threshold, t, format!("{t} °C"));
                                }
                            });
                    });
                    if let Some(t) = self.latest_snapshot.cpu_temp_c {
                        ui.label(RichText::new(format!("Current CPU Temp: {:.1}°C  |  Alert Trigger: ≥ {}°C", t, self.temp_warning_threshold)).size(11.0).color(Color32::from_rgb(180, 180, 190)));
                    }

                    ui.add_space(14.0);
                    ui.separator();
                    ui.add_space(6.0);

                    ui.heading("Switch Display Mode");
                    ui.add_space(8.0);

                    // 1. Hardware Monitor Mode
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new("▶ Switch to Hardware Monitor Mode").strong()).clicked() {
                            self.apply_hw_layout();
                            self.current_mode = "Hardware Monitor".to_string();
                        }
                    });

                    ui.add_space(8.0);
                    // 2. Built-in Factory Default Wallpapers (ROM Presets 0..=5)
                    ui.horizontal(|ui| {
                        ui.label("Default Wallpapers (ROM):");
                        ComboBox::from_id_salt("default_wp_select")
                            .selected_text(format!("Wallpaper Preset {}", self.selected_default_wallpaper))
                            .show_ui(ui, |ui| {
                                for p in 0..=5 {
                                    ui.selectable_value(&mut self.selected_default_wallpaper, p, format!("Wallpaper Preset {p}"));
                                }
                            });
                        if ui.button("Apply Wallpaper").clicked() {
                            let p = self.selected_default_wallpaper;
                            self.set_mode(DisplayMode::DefaultWallpaper(p), &format!("Default Wallpaper {p}"));
                        }
                    });

                    ui.add_space(8.0);
                    // 3. Built-in Animation Selector
                    ui.horizontal(|ui| {
                        ui.label("Built-in Animations:");
                        ComboBox::from_id_salt("anim_select")
                            .selected_text(match self.selected_animation {
                                0 => "Animation Preset 0 (ROG Starlight)".to_string(),
                                1 => "Animation Preset 1 (ROG Glitch Neon)".to_string(),
                                _ => format!("Animation Preset {}", self.selected_animation),
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.selected_animation, 0, "Animation Preset 0 (ROG Starlight)");
                                ui.selectable_value(&mut self.selected_animation, 1, "Animation Preset 1 (ROG Glitch Neon)");
                            });
                        if ui.button("Apply Animation").clicked() {
                            self.set_mode(DisplayMode::Animation(self.selected_animation), &format!("Animation {}", self.selected_animation));
                        }
                    });

                    ui.add_space(8.0);
                    // 4. Custom Flash Slots Switcher
                    ui.horizontal(|ui| {
                        ui.label("Custom Flash Slots:");
                        ComboBox::from_id_salt("disp_custom_slot_select")
                            .selected_text(format!("Custom Slot {} (SPI Flash)", self.selected_custom_slot))
                            .show_ui(ui, |ui| {
                                for s in 0..=4 {
                                    ui.selectable_value(&mut self.selected_custom_slot, s, format!("Custom Slot {s} (SPI Flash)"));
                                }
                            });
                        if ui.button("Display Custom Slot").clicked() {
                            let slot = self.selected_custom_slot;
                            self.set_mode(DisplayMode::CustomSlot(slot), &format!("Custom Slot {slot}"));
                        }
                    });
                }

                ActiveTab::Telemetry => {
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

                ActiveTab::ImageUpload => {
                    ui.heading("Flash Custom Image (Motherboard SPI Storage)");
                    ui.label("Upload any image (PNG, JPG, WebP, BMP, GIF). Encoded in hardware-compliant JFIF YUV 4:2:0 JPEG with standard IDs (1, 2, 3).");
                    ui.add_space(10.0);

                    ui.horizontal(|ui| {
                        ui.label("Target Destination Slot:");
                        ComboBox::from_id_salt("upload_slot_select")
                            .selected_text(format!("Custom Slot {} (SPI Flash)", self.selected_custom_slot))
                            .show_ui(ui, |ui| {
                                for s in 0..=4 {
                                    ui.selectable_value(&mut self.selected_custom_slot, s, format!("Custom Slot {s} (SPI Flash)"));
                                }
                            });

                        ui.add_space(15.0);
                        let can_erase = !self.is_flashing && self.device.is_some();
                        if ui.add_enabled(can_erase, egui::Button::new(RichText::new(format!("🗑 Erase Slot {}", self.selected_custom_slot)).color(Color32::from_rgb(255, 100, 100)))).clicked() {
                            let slot = self.selected_custom_slot;
                            let tx = self.upload_tx.clone();
                            let device = self.device.clone();
                            self.is_flashing = true;
                            self.upload_status = Some((format!("Erasing Slot {} from SPI flash...", slot), false));

                            std::thread::spawn(move || {
                                if let Some(dev) = device {
                                    if let Ok(dev) = dev.lock() {
                                        match dev.delete_custom_image_slot(slot) {
                                            Ok(_) => {
                                                let _ = tx.send(UploadEvent::Success(
                                                    format!("✓ Slot {} erased from SPI flash.", slot),
                                                    "Default Wallpaper 0".to_string(),
                                                ));
                                            }
                                            Err(e) => {
                                                let _ = tx.send(UploadEvent::Error(format!("Erase error: {e}")));
                                            }
                                        }
                                    } else {
                                        let _ = tx.send(UploadEvent::Error("Failed to lock device".to_string()));
                                    }
                                }
                            });
                        }
                    });

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("📂 Browse Image File...").clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp", "gif"])
                                .pick_file()
                            {
                                self.load_image(path, ctx);
                            }
                        }
                    });

                    if let Some(path) = &self.selected_image_path {
                        ui.label(format!("Selected: {}", path.file_name().unwrap_or_default().to_string_lossy()));
                    }

                    ui.add_space(8.0);
                    ui.label("Scaling Mode:");
                    let prev_fit = self.fit_mode;
                    ui.radio_value(&mut self.fit_mode, FitMode::Cover, "Cover (Center crop to 720x1280)");
                    ui.radio_value(&mut self.fit_mode, FitMode::Fit, "Fit (Letterbox with black bars)");
                    ui.radio_value(&mut self.fit_mode, FitMode::Stretch, "Stretch (Distort to 720x1280)");
                    if prev_fit != self.fit_mode {
                        self.refresh_preview_texture(ctx);
                    }

                    ui.add_space(6.0);
                    ui.add(Slider::new(&mut self.jpeg_quality, 50..=100).text("JPEG Quality"));

                    ui.add_space(12.0);
                    let can_upload = !self.is_flashing && self.selected_image_path.is_some() && self.device.is_some();
                    let flash_btn_text = if self.is_flashing {
                        format!("⏳ Flashing to Slot {} (SPI Lockstep)...", self.selected_custom_slot)
                    } else {
                        format!("⚡ Flash Image to Motherboard LCD (Slot {})", self.selected_custom_slot)
                    };

                    if ui.add_enabled(can_upload, egui::Button::new(RichText::new(flash_btn_text).strong())).clicked() {
                        if let Some(path) = &self.selected_image_path {
                            let path = path.clone();
                            let fit = self.fit_mode;
                            let quality = self.jpeg_quality;
                            let slot = self.selected_custom_slot;
                            let tx = self.upload_tx.clone();
                            let device = self.device.clone();

                            self.is_flashing = true;
                            self.upload_status = Some((format!("Encoding hardware-compliant JPEG for Slot {slot}..."), false));

                            std::thread::spawn(move || {
                                let _ = tx.send(UploadEvent::Progress("Encoding compliant JPEG...".to_string()));
                                match load_and_prepare_jpeg(&path, fit, quality) {
                                    Ok(jpeg) => {
                                        let _ = tx.send(UploadEvent::Progress(format!("Uploading {} bytes to Slot {} SPI flash...", jpeg.len(), slot)));
                                        if let Some(dev) = device {
                                            if let Ok(dev) = dev.lock() {
                                                match dev.upload_and_display_jpeg(&jpeg, slot) {
                                                    Ok(_) => {
                                                        let _ = tx.send(UploadEvent::Success(
                                                            format!("✓ Image successfully flashed to Slot {} and active on screen!", slot),
                                                            format!("Custom Slot {}", slot),
                                                        ));
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
                    }

                    if let Some((msg, is_err)) = &self.upload_status {
                        ui.add_space(8.0);
                        let color = if *is_err { Color32::from_rgb(220, 20, 60) } else { Color32::from_rgb(50, 205, 50) };
                        ui.label(RichText::new(msg).color(color).strong());
                    }
                }
            }
        });
    }
}
