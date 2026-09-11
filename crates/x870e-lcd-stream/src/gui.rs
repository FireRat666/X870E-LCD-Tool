//! Interactive desktop GUI for custom live streaming to the ASUS ROG X870E LCD.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, ComboBox, Pos2, Rect, RichText, Sense, Slider, Stroke, StrokeKind, Vec2};
use x870e_lcd_core::{HardwareMonitor, LcdDevice, DisplayMode, FRAME_RAW_SIZE, FRAME_WIDTH, FRAME_HEIGHT};

use crate::media::{self, MediaConfig, Rotation, ScaleMode};
use crate::renderer::{self, DashboardData, DashboardSection, ThemeColor};
use crate::streamer::StreamerStats;
use crate::video::VideoPlayer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamMode {
    Dashboard,
    Media,
    Pattern,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    None,
    Image,
    Video,
}

pub struct StreamGuiApp {
    mode: StreamMode,
    dashboard_data: DashboardData,
    selected_pattern: String,

    // Media streaming state
    media_path: Option<PathBuf>,
    media_kind: MediaKind,
    image_data: Option<image::DynamicImage>,
    video_player: Arc<Mutex<VideoPlayer>>,
    media_config: MediaConfig,
    source_thumb_tex: Option<egui::TextureHandle>,

    pacing_ms: u64,

    // Streaming state
    is_streaming: bool,
    frames_sent: u32,
    current_fps: f32,
    stats: StreamerStats,
    active_frame: Arc<Mutex<Vec<u8>>>,
    stream_thread: Option<thread::JoinHandle<()>>,

    // Preview
    preview_texture: Option<egui::TextureHandle>,
    hw_mon: HardwareMonitor,
    last_hw_update: Instant,
    status_message: String,
}

impl StreamGuiApp {
    /// Creates and initializes the GUI application state with default dashboard telemetry.
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let mut hw_mon = HardwareMonitor::new();
        let snap = hw_mon.refresh();

        let mut data = DashboardData::default();
        data.cpu_name = snap.cpu_name.clone();
        data.cpu_usage = snap.cpu_usage_pct;
        data.cpu_temp = snap.cpu_temp_c.unwrap_or(45.0);
        data.cpu_freq_mhz = snap.cpu_freq_mhz as f32;

        data.gpu_name = snap.gpu_name;
        data.gpu_usage = snap.gpu_usage_pct.unwrap_or(0.0);
        data.gpu_temp = snap.gpu_temp_c.unwrap_or(50.0);
        data.gpu_vram_used_gb = snap.gpu_mem_used_gb.unwrap_or(2.0);
        data.gpu_vram_total_gb = snap.gpu_mem_total_gb.unwrap_or(16.0);

        data.mem_used_gb = snap.ram_used_gb;
        data.mem_total_gb = snap.ram_total_gb;
        data.net_rx_kbps = snap.net_rx_kbps;
        data.net_tx_kbps = snap.net_tx_kbps;

        let initial_buf = vec![0u8; FRAME_RAW_SIZE];

        Self {
            mode: StreamMode::Dashboard,
            dashboard_data: data,
            selected_pattern: "vertical-split".to_string(),
            media_path: None,
            media_kind: MediaKind::None,
            image_data: None,
            video_player: Arc::new(Mutex::new(VideoPlayer::new())),
            media_config: MediaConfig::default(),
            source_thumb_tex: None,
            pacing_ms: 250,
            is_streaming: false,
            frames_sent: 0,
            current_fps: 0.0,
            stats: StreamerStats::new(),
            active_frame: Arc::new(Mutex::new(initial_buf)),
            stream_thread: None,
            preview_texture: None,
            hw_mon,
            last_hw_update: Instant::now(),
            status_message: "Ready to stream".to_string(),
        }
    }

    /// Periodically queries hardware sensors to refresh dashboard telemetry.
    fn update_telemetry(&mut self) {
        if self.last_hw_update.elapsed() >= Duration::from_millis(500) {
            let snap = self.hw_mon.refresh();
            self.dashboard_data.cpu_usage = snap.cpu_usage_pct;
            if let Some(t) = snap.cpu_temp_c {
                self.dashboard_data.cpu_temp = t;
            }
            if snap.cpu_freq_mhz > 0 {
                self.dashboard_data.cpu_freq_mhz = snap.cpu_freq_mhz as f32;
            }

            self.dashboard_data.gpu_name = snap.gpu_name;
            if let Some(u) = snap.gpu_usage_pct {
                self.dashboard_data.gpu_usage = u;
            }
            if let Some(t) = snap.gpu_temp_c {
                self.dashboard_data.gpu_temp = t;
            }
            if let Some(v) = snap.gpu_mem_used_gb {
                self.dashboard_data.gpu_vram_used_gb = v;
            }
            if let Some(tot) = snap.gpu_mem_total_gb {
                self.dashboard_data.gpu_vram_total_gb = tot;
            }

            self.dashboard_data.mem_used_gb = snap.ram_used_gb;
            self.dashboard_data.mem_total_gb = snap.ram_total_gb;

            self.dashboard_data.net_rx_kbps = snap.net_rx_kbps;
            self.dashboard_data.net_tx_kbps = snap.net_tx_kbps;

            self.last_hw_update = Instant::now();
        }
    }

    /// Generates the next BGRA8888 frame according to current mode and media settings.
    fn generate_current_frame(&mut self, buffer: &mut [u8]) {
        match self.mode {
            StreamMode::Dashboard => {
                self.update_telemetry();
                renderer::render_dashboard(&self.dashboard_data, buffer);
            }
            StreamMode::Pattern => {
                renderer::render_pattern(&self.selected_pattern, self.frames_sent, buffer);
            }
            StreamMode::Media => {
                match self.media_kind {
                    MediaKind::Image => {
                        if let Some(ref img) = self.image_data {
                            media::process_image_to_bgra(img, &self.media_config, buffer);
                        } else {
                            renderer::fill_rect_bgra(buffer, 0, 0, FRAME_WIDTH as usize, FRAME_HEIGHT as usize, [0, 0, 0, 0xff]);
                        }
                    }
                    MediaKind::Video => {
                        if let Ok(mut player) = self.video_player.lock() {
                            let _ = player.read_frame(buffer);
                        }
                    }
                    MediaKind::None => {
                        renderer::fill_rect_bgra(buffer, 0, 0, FRAME_WIDTH as usize, FRAME_HEIGHT as usize, [0, 0, 0, 0xff]);
                    }
                }
            }
        }
    }

    /// Loads an image or video media file and configures decoder / preview textures.
    fn load_media_file(&mut self, path: PathBuf, ctx: &egui::Context) {
        let ext = path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_lowercase();

        let is_video = matches!(ext.as_str(), "mp4" | "mkv" | "webm" | "avi" | "mov" | "flv" | "gif");

        if is_video {
            if let Ok(mut player) = self.video_player.lock() {
                if let Err(e) = player.load(&path, self.media_config.clone()) {
                    self.status_message = format!("Video Load Error: {}", e);
                    return;
                }
            }
            self.media_kind = MediaKind::Video;
            self.image_data = None;
            self.source_thumb_tex = None;
            self.media_path = Some(path);
            self.status_message = "Video loaded successfully".to_string();
        } else {
            // Stop any playing video
            if let Ok(mut player) = self.video_player.lock() {
                player.stop();
            }

            match image::open(&path) {
                Ok(img) => {
                    // Generate thumbnail texture for the framing preview
                    let thumb = img.thumbnail(320, 320);
                    let rgba = thumb.to_rgba8();
                    let color_img = egui::ColorImage::from_rgba_unmultiplied(
                        [thumb.width() as usize, thumb.height() as usize],
                        &rgba,
                    );
                    self.source_thumb_tex = Some(ctx.load_texture("source_thumb", color_img, egui::TextureOptions::LINEAR));
                    self.image_data = Some(img);
                    self.media_kind = MediaKind::Image;
                    self.media_path = Some(path);
                    self.status_message = "Image loaded successfully".to_string();
                }
                Err(e) => {
                    self.status_message = format!("Image Load Error: {}", e);
                }
            }
        }
    }

    /// Synchronizes zoom, pan, and rotation configuration with the running video player.
    fn sync_video_config(&mut self) {
        if self.media_kind == MediaKind::Video {
            if let Ok(mut player) = self.video_player.lock() {
                let _ = player.set_config(self.media_config.clone());
            }
        }
    }

    /// Connects to the LCD panel and starts background frame streaming.
    fn start_streaming(&mut self) {
        if self.is_streaming {
            return;
        }

        let device = match LcdDevice::open() {
            Ok(d) => d,
            Err(e) => {
                self.status_message = format!("Device Error: {}", e);
                return;
            }
        };

        if let Err(e) = device.enter_frame_stream() {
            self.status_message = format!("Failed to set stream mode: {}", e);
            return;
        }

        self.stats = StreamerStats::new();
        self.stats.running.store(true, Ordering::SeqCst);
        self.is_streaming = true;
        self.status_message = "Live Streaming Active (Starting...)".to_string();

        let stats = self.stats.clone();
        let frame_shared = self.active_frame.clone();
        let pacing = self.pacing_ms;

        let handle = thread::spawn(move || {
            let mut local_buf = vec![0u8; FRAME_RAW_SIZE];
            while stats.running.load(Ordering::SeqCst) {
                let start = Instant::now();

                // Copy latest frame
                if let Ok(guard) = frame_shared.lock() {
                    local_buf.copy_from_slice(&guard);
                }

                // Send frame over USB
                if let Err(e) = device.send_stream_frame(&local_buf) {
                    tracing::error!("Stream error: {}", e);
                    break;
                }

                stats.frames_sent.fetch_add(1, Ordering::Relaxed);

                // Inter-frame pacing delay
                if pacing > 0 {
                    thread::sleep(Duration::from_millis(pacing));
                }

                // Update FPS metrics
                let frame_duration = start.elapsed().as_secs_f32();
                let instant_fps = if frame_duration > 0.0 { 1.0 / frame_duration } else { 0.0 };
                if let Ok(mut lock) = stats.current_fps.lock() {
                    *lock = if *lock == 0.0 {
                        instant_fps
                    } else {
                        *lock * 0.7 + instant_fps * 0.3
                    };
                }
            }

            let _ = device.set_mode(DisplayMode::DefaultWallpaper(0));
            stats.running.store(false, Ordering::SeqCst);
        });

        self.stream_thread = Some(handle);
    }

    /// Stops active background frame streaming and joins the streaming thread.
    fn stop_streaming(&mut self) {
        if !self.is_streaming {
            return;
        }

        self.stats.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.stream_thread.take() {
            let _ = handle.join();
        }

        self.is_streaming = false;
        self.status_message = "Streaming stopped (Panel reverted to default wallpaper)".to_string();
    }
}

impl eframe::App for StreamGuiApp {
    /// Renders the GUI interface and processes egui events for each frame.
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Dynamic repaint based on media/pacing
        let repaint_ms = if self.pacing_ms > 0 { self.pacing_ms.min(250) } else { 100 };
        ctx.request_repaint_after(Duration::from_millis(repaint_ms));

        if self.is_streaming {
            self.frames_sent = self.stats.frames_sent.load(Ordering::Relaxed);
            self.current_fps = self.stats.get_fps();
            if !self.stats.running.load(Ordering::SeqCst) {
                self.is_streaming = false;
                self.status_message = "Stream stopped or disconnected".to_string();
            } else if self.current_fps > 0.0 {
                self.status_message = format!("Live Streaming Active ({:.1} FPS)", self.current_fps);
            } else {
                self.status_message = "Live Streaming Active (Starting...)".to_string();
            }
        }

        self.dashboard_data.fps = self.current_fps;
        self.dashboard_data.pacing_ms = self.pacing_ms;

        // Generate current frame into active buffer
        let mut local_buf = vec![0u8; FRAME_RAW_SIZE];
        self.generate_current_frame(&mut local_buf);

        if let Ok(mut buf) = self.active_frame.lock() {
            buf.copy_from_slice(&local_buf);
        }

        // Convert BGRA to RGBA for egui central preview
        let mut rgba = vec![0u8; FRAME_RAW_SIZE];
        for i in (0..FRAME_RAW_SIZE).step_by(4) {
            rgba[i] = local_buf[i + 2];     // R
            rgba[i + 1] = local_buf[i + 1]; // G
            rgba[i + 2] = local_buf[i];     // B
            rgba[i + 3] = 255;              // A
        }

        let color_img = egui::ColorImage::from_rgba_unmultiplied(
            [FRAME_WIDTH as usize, FRAME_HEIGHT as usize],
            &rgba,
        );

        if let Some(tex) = &mut self.preview_texture {
            tex.set(color_img, egui::TextureOptions::LINEAR);
        } else {
            self.preview_texture = Some(ctx.load_texture("lcd_preview", color_img, egui::TextureOptions::LINEAR));
        }

        // Top Control Panel
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading(RichText::new("ROG X870E LCD Live Streamer").color(Color32::from_rgb(255, 60, 60)).strong());
                ui.separator();

                if self.is_streaming {
                    ui.label(RichText::new("● STREAMING").color(Color32::GREEN).strong());
                    if ui.button(RichText::new("⏹ Stop Streaming").color(Color32::WHITE)).clicked() {
                        self.stop_streaming();
                    }
                } else {
                    ui.label(RichText::new("○ IDLE").color(Color32::GRAY));
                    if ui.button(RichText::new("▶ Start Live Stream").color(Color32::GREEN).strong()).clicked() {
                        self.start_streaming();
                    }
                }

                ui.separator();
                ui.label(format!("Frames: {} ({:.1} FPS)", self.frames_sent, self.current_fps));
                ui.separator();
                ui.label(&self.status_message);
            });
            ui.add_space(6.0);
        });

        // Left Controls Panel
        egui::SidePanel::left("left_controls")
            .resizable(true)
            .default_width(420.0)
            .show(ctx, |ui| {
                ui.add_space(8.0);
                ui.heading("Source Stream Mode");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.mode, StreamMode::Dashboard, "📊 Dashboard");
                    ui.selectable_value(&mut self.mode, StreamMode::Media, "🎬 Media (Video/Image)");
                    ui.selectable_value(&mut self.mode, StreamMode::Pattern, "🎨 Patterns");
                });

                ui.separator();

                match self.mode {
                    StreamMode::Dashboard => {
                        ui.heading("Dashboard Appearance & Metrics");
                        ui.add_space(4.0);

                        ui.horizontal(|ui| {
                            ui.label("Title:");
                            ui.text_edit_singleline(&mut self.dashboard_data.title);
                        });

                        ui.horizontal(|ui| {
                            ui.label("Theme Color:");
                            ComboBox::from_id_salt("theme_picker")
                                .selected_text(match self.dashboard_data.theme {
                                    ThemeColor::RogRed => "ROG Red",
                                    ThemeColor::CyberCyan => "Cyber Cyan",
                                    ThemeColor::MatrixGreen => "Matrix Green",
                                    ThemeColor::AmberGold => "Amber Gold",
                                    ThemeColor::NeonPurple => "Neon Purple",
                                })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut self.dashboard_data.theme, ThemeColor::RogRed, "ROG Red");
                                    ui.selectable_value(&mut self.dashboard_data.theme, ThemeColor::CyberCyan, "Cyber Cyan");
                                    ui.selectable_value(&mut self.dashboard_data.theme, ThemeColor::MatrixGreen, "Matrix Green");
                                    ui.selectable_value(&mut self.dashboard_data.theme, ThemeColor::AmberGold, "Amber Gold");
                                    ui.selectable_value(&mut self.dashboard_data.theme, ThemeColor::NeonPurple, "Neon Purple");
                                });
                        });

                        ui.add_space(8.0);
                        ui.group(|ui| {
                            ui.label(RichText::new("Modular Dashboard Sections (5 Customizable Slots)").strong());
                            ui.add_space(4.0);

                            let section_options = [
                                DashboardSection::Clock,
                                DashboardSection::Cpu,
                                DashboardSection::Gpu,
                                DashboardSection::Ram,
                                DashboardSection::Network,
                                DashboardSection::None,
                            ];

                            let render_slot = |ui: &mut egui::Ui, id: &str, label: &str, slot: &mut DashboardSection| {
                                ui.horizontal(|ui| {
                                    ui.label(label);
                                    ComboBox::from_id_salt(id)
                                        .selected_text(slot.label())
                                        .show_ui(ui, |ui| {
                                            for opt in &section_options {
                                                ui.selectable_value(slot, *opt, opt.label());
                                            }
                                        });
                                });
                            };

                            render_slot(ui, "slot1_picker", "Slot 1:", &mut self.dashboard_data.slot1);
                            render_slot(ui, "slot2_picker", "Slot 2:", &mut self.dashboard_data.slot2);
                            render_slot(ui, "slot3_picker", "Slot 3:", &mut self.dashboard_data.slot3);
                            render_slot(ui, "slot4_picker", "Slot 4:", &mut self.dashboard_data.slot4);
                            render_slot(ui, "slot5_picker", "Slot 5:", &mut self.dashboard_data.slot5);

                            ui.separator();
                            ui.checkbox(&mut self.dashboard_data.show_header, "Show Top Header Banner");
                            ui.checkbox(&mut self.dashboard_data.show_footer, "Show Bottom Stream Footer");
                        });

                        ui.add_space(8.0);
                        ui.group(|ui| {
                            ui.label(RichText::new("Live Telemetry Snapshot").strong());
                            ui.label(format!("CPU: {} (Load: {:.1}%, Temp: {:.1}°C)", self.dashboard_data.cpu_name, self.dashboard_data.cpu_usage, self.dashboard_data.cpu_temp));
                            ui.label(format!("GPU: {} (Load: {:.0}%, Temp: {:.0}°C, VRAM: {:.1}/{:.1} GB)", self.dashboard_data.gpu_name, self.dashboard_data.gpu_usage, self.dashboard_data.gpu_temp, self.dashboard_data.gpu_vram_used_gb, self.dashboard_data.gpu_vram_total_gb));
                            ui.label(format!("RAM: {:.1} GB / {:.1} GB", self.dashboard_data.mem_used_gb, self.dashboard_data.mem_total_gb));
                            ui.label(format!("NET: ▼ {:.1} MB/s | ▲ {:.1} MB/s", self.dashboard_data.net_rx_kbps / 1024.0, self.dashboard_data.net_tx_kbps / 1024.0));
                        });
                    }

                    StreamMode::Media => {
                        ui.heading("Media Player (Videos & Images)");
                        ui.add_space(4.0);

                        ui.horizontal(|ui| {
                            if ui.button("📂 Select Video or Image...").clicked() {
                                if let Some(path) = rfd::FileDialog::new()
                                    .add_filter("Media Files", &["mp4", "mkv", "webm", "avi", "mov", "flv", "gif", "png", "jpg", "jpeg", "webp", "bmp"])
                                    .pick_file()
                                {
                                    self.load_media_file(path, ctx);
                                }
                            }

                            if let Some(ref path) = self.media_path {
                                let name = path.file_name().unwrap_or_default().to_string_lossy();
                                ui.label(RichText::new(format!("Loaded: {}", name)).strong());
                            }
                        });

                        ui.add_space(6.0);
                        let mut config_changed = false;

                        ui.group(|ui| {
                            ui.label(RichText::new("Display Scaling & Rotation").strong());

                            ui.horizontal(|ui| {
                                ui.label("Scaling Mode:");
                                let prev_mode = self.media_config.scale_mode;
                                ComboBox::from_id_salt("scale_mode_picker")
                                    .selected_text(self.media_config.scale_mode.label())
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(&mut self.media_config.scale_mode, ScaleMode::Fill, ScaleMode::Fill.label());
                                        ui.selectable_value(&mut self.media_config.scale_mode, ScaleMode::Fit, ScaleMode::Fit.label());
                                        ui.selectable_value(&mut self.media_config.scale_mode, ScaleMode::Stretch, ScaleMode::Stretch.label());
                                        ui.selectable_value(&mut self.media_config.scale_mode, ScaleMode::CustomCrop, ScaleMode::CustomCrop.label());
                                    });
                                if self.media_config.scale_mode != prev_mode {
                                    config_changed = true;
                                }
                            });

                            ui.horizontal(|ui| {
                                ui.label("Rotation:");
                                let prev_rot = self.media_config.rotation;
                                ComboBox::from_id_salt("rotation_picker")
                                    .selected_text(self.media_config.rotation.label())
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(&mut self.media_config.rotation, Rotation::Rot0, Rotation::Rot0.label());
                                        ui.selectable_value(&mut self.media_config.rotation, Rotation::Rot90, Rotation::Rot90.label());
                                        ui.selectable_value(&mut self.media_config.rotation, Rotation::Rot180, Rotation::Rot180.label());
                                        ui.selectable_value(&mut self.media_config.rotation, Rotation::Rot270, Rotation::Rot270.label());
                                    });
                                if self.media_config.rotation != prev_rot {
                                    config_changed = true;
                                }
                            });

                            // Custom Crop, Pan, Zoom controls
                            if self.media_config.scale_mode == ScaleMode::CustomCrop {
                                ui.separator();
                                ui.label(RichText::new("Pan & Zoom Controls").strong());

                                ui.horizontal(|ui| {
                                    ui.label("Zoom:");
                                    if ui.add(Slider::new(&mut self.media_config.zoom, 0.5..=4.0).step_by(0.05)).changed() {
                                        config_changed = true;
                                    }
                                    if ui.button("1.0x").clicked() {
                                        self.media_config.zoom = 1.0;
                                        config_changed = true;
                                    }
                                });

                                ui.horizontal(|ui| {
                                    ui.label("Pan X:");
                                    if ui.add(Slider::new(&mut self.media_config.pan_x, -1.0..=1.0).step_by(0.02)).changed() {
                                        config_changed = true;
                                    }
                                    if ui.button("Center").clicked() {
                                        self.media_config.pan_x = 0.0;
                                        config_changed = true;
                                    }
                                });

                                ui.horizontal(|ui| {
                                    ui.label("Pan Y:");
                                    if ui.add(Slider::new(&mut self.media_config.pan_y, -1.0..=1.0).step_by(0.02)).changed() {
                                        config_changed = true;
                                    }
                                    if ui.button("Center").clicked() {
                                        self.media_config.pan_y = 0.0;
                                        config_changed = true;
                                    }
                                });
                            }

                            // Video specific controls
                            if self.media_kind == MediaKind::Video {
                                ui.separator();
                                ui.label(RichText::new("Video Playback").strong());
                                ui.horizontal(|ui| {
                                    let mut is_playing = true;
                                    if let Ok(player) = self.video_player.lock() {
                                        is_playing = player.is_playing;
                                    }

                                    let play_btn_text = if is_playing { "⏸ Pause Video" } else { "▶ Play Video" };
                                    if ui.button(play_btn_text).clicked() {
                                        if let Ok(mut player) = self.video_player.lock() {
                                            player.is_playing = !player.is_playing;
                                        }
                                    }

                                    if ui.button("⏮ Restart Video").clicked() {
                                        if let Some(ref path) = self.media_path.clone() {
                                            if let Ok(mut player) = self.video_player.lock() {
                                                let _ = player.load(path, self.media_config.clone());
                                            }
                                        }
                                    }

                                    if ui.checkbox(&mut self.media_config.loop_media, "Loop Video").changed() {
                                        config_changed = true;
                                    }
                                });
                            }
                        });

                        // Interactive Dotted Crop Framing Box for loaded images
                        if let Some(ref tex) = self.source_thumb_tex {
                            ui.add_space(8.0);
                            ui.group(|ui| {
                                ui.label(RichText::new("Source Framing Box (Drag to Pan)").strong());
                                let tex_size = tex.size_vec2();
                                let display_w = 260.0;
                                let display_h = display_w * (tex_size.y / tex_size.x);
                                let (rect, response) = ui.allocate_exact_size(Vec2::new(display_w, display_h), Sense::drag());

                                // Draw thumbnail image
                                ui.painter().image(tex.id(), rect, Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)), Color32::WHITE);

                                // Allow dragging directly on the thumbnail to pan
                                if response.dragged() {
                                    let delta = response.drag_delta();
                                    self.media_config.pan_x = (self.media_config.pan_x + delta.x / (display_w * 0.5)).clamp(-1.0, 1.0);
                                    self.media_config.pan_y = (self.media_config.pan_y + delta.y / (display_h * 0.5)).clamp(-1.0, 1.0);
                                    self.media_config.scale_mode = ScaleMode::CustomCrop;
                                    config_changed = true;
                                }

                                // Calculate and draw the dotted 9:16 crop rectangle
                                let zoom = if self.media_config.scale_mode == ScaleMode::CustomCrop { self.media_config.zoom } else { 1.0 };
                                let target_ratio = 720.0 / 1280.0; // 9:16
                                let box_h = (display_h / zoom).min(display_h);
                                let box_w = (box_h * target_ratio).min(display_w);

                                let max_pan_x = (display_w - box_w) / 2.0;
                                let max_pan_y = (display_h - box_h) / 2.0;

                                let center_x = rect.min.x + display_w / 2.0 + self.media_config.pan_x * max_pan_x;
                                let center_y = rect.min.y + display_h / 2.0 + self.media_config.pan_y * max_pan_y;

                                let crop_rect = Rect::from_center_size(Pos2::new(center_x, center_y), Vec2::new(box_w, box_h));

                                // Draw crop rect framing outline
                                let stroke = Stroke::new(2.0f32, Color32::from_rgb(255, 60, 60));
                                ui.painter().rect_stroke(crop_rect, 0.0, stroke, StrokeKind::Middle);
                            });
                        }

                        if config_changed {
                            self.sync_video_config();
                        }
                    }

                    StreamMode::Pattern => {
                        ui.heading("Test Pattern Generator");
                        ui.add_space(4.0);
                        ComboBox::from_id_salt("pattern_picker")
                            .selected_text(&self.selected_pattern)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.selected_pattern, "vertical-split".to_string(), "Vertical Split (Tear Test)");
                                ui.selectable_value(&mut self.selected_pattern, "colorbars".to_string(), "SMPTE Color Bars");
                                ui.selectable_value(&mut self.selected_pattern, "gradient".to_string(), "Rainbow Gradient");
                            });
                        ui.add_space(10.0);
                        ui.label("Ideal for validating tearing immunity and display color response.");
                    }
                }

                ui.separator();
                ui.heading("Hardware Pacing");
                ui.add(Slider::new(&mut self.pacing_ms, 10..=1000).text("ms inter-frame delay"));
                ui.label(RichText::new("200ms–250ms guarantees tear-free DMA2D blits. Lower values speed up framerate.").small().color(Color32::GRAY));
            });

        // Center Preview Panel
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Display Preview (720 × 1280)");
            ui.add_space(8.0);

            if let Some(tex) = &self.preview_texture {
                let preview_w = 340.0;
                let preview_h = preview_w * (1280.0 / 720.0); // 9:16 aspect ratio (~604px)
                ui.image((tex.id(), Vec2::new(preview_w, preview_h)));
            }
        });
    }
}
