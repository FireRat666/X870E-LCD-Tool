//! 720x1280 BGRA8888 software canvas renderer for dashboard, patterns, and media frames.

use image::{imageops::FilterType, DynamicImage, GenericImageView};
use crate::font::{draw_text_bgra, FONT_WIDTH};

pub const WIDTH: usize = 720;
pub const HEIGHT: usize = 1280;
#[allow(dead_code)]
pub const BUFFER_SIZE: usize = WIDTH * HEIGHT * 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeColor {
    RogRed,
    CyberCyan,
    MatrixGreen,
    AmberGold,
    NeonPurple,
    Custom([u8; 3]),
}

impl ThemeColor {
    /// Returns the primary accent color as [B, G, R, A].
    pub fn accent_bgra(&self) -> [u8; 4] {
        match self {
            Self::RogRed => [0x22, 0x22, 0xee, 0xff],      // Bright Red
            Self::CyberCyan => [0xee, 0xee, 0x00, 0xff],    // Cyan
            Self::MatrixGreen => [0x33, 0xee, 0x33, 0xff],  // Neon Green
            Self::AmberGold => [0x00, 0xb0, 0xff, 0xff],    // Amber Gold
            Self::NeonPurple => [0xee, 0x33, 0xaa, 0xff],   // Purple
            Self::Custom([r, g, b]) => [*b, *g, *r, 0xff],  // Custom RGB -> BGRA
        }
    }

    /// Returns the primary accent color as [R, G, B].
    pub fn rgb(&self) -> [u8; 3] {
        match self {
            Self::RogRed => [0xee, 0x22, 0x22],
            Self::CyberCyan => [0x00, 0xee, 0xee],
            Self::MatrixGreen => [0x33, 0xee, 0x33],
            Self::AmberGold => [0xff, 0xb0, 0x00],
            Self::NeonPurple => [0xaa, 0x33, 0xee],
            Self::Custom(rgb) => *rgb,
        }
    }

    /// Returns the hex code string for this theme color, e.g. "#EE2222".
    pub fn hex(&self) -> String {
        let [r, g, b] = self.rgb();
        format!("#{:02X}{:02X}{:02X}", r, g, b)
    }

    /// Returns the human-readable display label for this theme color.
    pub fn label(&self) -> String {
        match self {
            Self::RogRed => "ROG Red".to_string(),
            Self::CyberCyan => "Cyber Cyan".to_string(),
            Self::MatrixGreen => "Matrix Green".to_string(),
            Self::AmberGold => "Amber Gold".to_string(),
            Self::NeonPurple => "Neon Purple".to_string(),
            Self::Custom(rgb) => format!("Custom (#{r:02X}{g:02X}{b:02X})", r = rgb[0], g = rgb[1], b = rgb[2]),
        }
    }

    /// Parses a theme color from a preset name, hex string, or RGB triplet.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        match s.to_lowercase().as_str() {
            "red" | "rog" | "rogred" => Some(Self::RogRed),
            "cyan" | "cyber" | "cybercyan" => Some(Self::CyberCyan),
            "green" | "matrix" | "matrixgreen" => Some(Self::MatrixGreen),
            "amber" | "gold" | "ambergold" => Some(Self::AmberGold),
            "purple" | "neon" | "neonpurple" => Some(Self::NeonPurple),
            _ => {
                let hex = s.strip_prefix('#').unwrap_or(s);
                if hex.is_ascii() && hex.len() == 6 {
                    if let (Ok(r), Ok(g), Ok(b)) = (
                        u8::from_str_radix(&hex[0..2], 16),
                        u8::from_str_radix(&hex[2..4], 16),
                        u8::from_str_radix(&hex[4..6], 16),
                    ) {
                        return Some(Self::Custom([r, g, b]));
                    }
                }
                let rgb_str = s.strip_prefix("rgb(").and_then(|t| t.strip_suffix(')')).unwrap_or(s);
                let parts: Vec<&str> = rgb_str.split(',').map(|p| p.trim()).collect();
                if parts.len() == 3 {
                    if let (Ok(r), Ok(g), Ok(b)) = (
                        parts[0].parse::<u8>(),
                        parts[1].parse::<u8>(),
                        parts[2].parse::<u8>(),
                    ) {
                        return Some(Self::Custom([r, g, b]));
                    }
                }
                None
            }
        }
    }

    /// Returns the background canvas color as [B, G, R, A].
    pub fn bg_bgra(&self) -> [u8; 4] {
        [0x12, 0x10, 0x10, 0xff] // Deep dark background
    }

    /// Returns the card surface color as [B, G, R, A].
    pub fn card_bgra(&self) -> [u8; 4] {
        [0x24, 0x20, 0x20, 0xff] // Card background
    }
}

/// Timezone configuration for the dashboard clock display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimezoneConfig {
    /// Local time according to the operating system's configured timezone.
    #[default]
    Local,
    /// Coordinated Universal Time (UTC).
    Utc,
    /// Custom fixed UTC offset in minutes (e.g. +600 for AEST UTC+10).
    Custom(i32),
}

impl TimezoneConfig {
    /// Returns the formatted time and date strings according to the configured timezone.
    pub fn now_strings(&self, use_24h: bool) -> (String, String) {
        match self {
            Self::Local => {
                let local = chrono::Local::now();
                let time_fmt = if use_24h { "%H:%M:%S" } else { "%I:%M:%S %p" };
                (local.format(time_fmt).to_string(), local.format("%A, %b %d, %Y").to_string())
            }
            Self::Utc => {
                let utc = chrono::Utc::now();
                let time_fmt = if use_24h { "%H:%M:%S UTC" } else { "%I:%M:%S %p UTC" };
                (utc.format(time_fmt).to_string(), utc.format("%A, %b %d, %Y").to_string())
            }
            Self::Custom(offset_minutes) => {
                let secs = offset_minutes * 60;
                if let Some(offset) = chrono::FixedOffset::east_opt(secs) {
                    let dt = chrono::Utc::now().with_timezone(&offset);
                    let sign = if *offset_minutes >= 0 { '+' } else { '-' };
                    let abs_m = offset_minutes.abs();
                    let tz_str = format!("UTC{}{:02}:{:02}", sign, abs_m / 60, abs_m % 60);
                    let time_fmt = if use_24h { "%H:%M:%S" } else { "%I:%M:%S %p" };
                    (format!("{} {}", dt.format(time_fmt), tz_str), dt.format("%A, %b %d, %Y").to_string())
                } else {
                    let utc = chrono::Utc::now();
                    (utc.format("%H:%M:%S UTC").to_string(), utc.format("%A, %b %d, %Y").to_string())
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DashboardSection {
    #[default]
    None,
    Clock,
    Cpu,
    Gpu,
    Ram,
    Network,
}

impl DashboardSection {
    /// Returns the human-readable display label for this dashboard slot option.
    pub fn label(&self) -> &'static str {
        match self {
            Self::None => "(Empty / Disabled)",
            Self::Clock => "🕒 Clock & Date",
            Self::Cpu => "💻 CPU (Usage, Temp, Freq)",
            Self::Gpu => "🎮 GPU (Usage, Temp, VRAM)",
            Self::Ram => "🧠 RAM (Memory Usage)",
            Self::Network => "🌐 Network (Up/Down Bandwidth)",
        }
    }
}

/// Layout configuration and system telemetry payload rendered to the dashboard canvas.
#[derive(Debug, Clone)]
pub struct DashboardData {
    pub title: String,
    pub theme: ThemeColor,
    pub fps: f32,
    pub pacing_ms: u64,
    pub show_header: bool,
    pub show_footer: bool,

    // Modular section slots
    pub slot1: DashboardSection,
    pub slot2: DashboardSection,
    pub slot3: DashboardSection,
    pub slot4: DashboardSection,
    pub slot5: DashboardSection,

    // Telemetry data
    pub cpu_name: String,
    pub cpu_usage: f32,
    pub cpu_temp: f32,
    pub cpu_freq_mhz: f32,

    pub gpu_name: String,
    pub gpu_usage: f32,
    pub gpu_temp: f32,
    pub gpu_vram_used_gb: f32,
    pub gpu_vram_total_gb: f32,

    pub mem_used_gb: f32,
    pub mem_total_gb: f32,

    pub net_rx_kbps: f32,
    pub net_tx_kbps: f32,

    pub timezone: TimezoneConfig,
    pub use_24h_clock: bool,
}

impl Default for DashboardData {
    /// Returns default dashboard layout metrics and sample telemetry state.
    fn default() -> Self {
        Self {
            title: "ROG CROSSHAIR X870E".to_string(),
            theme: ThemeColor::RogRed,
            fps: 0.0,
            pacing_ms: 250,
            show_header: true,
            show_footer: true,
            timezone: TimezoneConfig::Local,
            use_24h_clock: true,
            slot1: DashboardSection::Clock,
            slot2: DashboardSection::Cpu,
            slot3: DashboardSection::Gpu,
            slot4: DashboardSection::Ram,
            slot5: DashboardSection::Network,

            cpu_name: "AMD Ryzen".to_string(),
            cpu_usage: 0.0,
            cpu_temp: 45.0,
            cpu_freq_mhz: 4800.0,

            gpu_name: "RTX GPU".to_string(),
            gpu_usage: 0.0,
            gpu_temp: 50.0,
            gpu_vram_used_gb: 2.0,
            gpu_vram_total_gb: 16.0,

            mem_used_gb: 8.0,
            mem_total_gb: 32.0,

            net_rx_kbps: 0.0,
            net_tx_kbps: 0.0,
        }
    }
}

/// Renders right-aligned text ending at right_x on a BGRA frame buffer.
pub fn draw_text_right_aligned_bgra(
    buffer: &mut [u8],
    stride_width: usize,
    stride_height: usize,
    right_x: usize,
    y: usize,
    text: &str,
    color: [u8; 4],
    scale: usize,
) {
    // Each character advances by (FONT_WIDTH + 1) * scale
    let char_step = (FONT_WIDTH + 1) * scale;
    let text_w = text.chars().count() * char_step;
    let start_x = right_x.saturating_sub(text_w);
    draw_text_bgra(buffer, stride_width, stride_height, start_x, y, text, color, scale);
}

/// Fills a solid rectangle with color [B, G, R, A] into a 720x1280 BGRA buffer.
pub fn fill_rect_bgra(
    buffer: &mut [u8],
    rx: usize,
    ry: usize,
    rw: usize,
    rh: usize,
    color: [u8; 4],
) {
    let max_y = (ry + rh).min(HEIGHT);
    let max_x = (rx + rw).min(WIDTH);
    for y in ry..max_y {
        for x in rx..max_x {
            let offset = (y * WIDTH + x) * 4;
            if offset + 3 < buffer.len() {
                buffer[offset] = color[0];
                buffer[offset + 1] = color[1];
                buffer[offset + 2] = color[2];
                buffer[offset + 3] = color[3];
            }
        }
    }
}

/// Draws an unfilled rectangular border with the specified thickness and color.
pub fn draw_border_bgra(
    buffer: &mut [u8],
    rx: usize,
    ry: usize,
    rw: usize,
    rh: usize,
    thickness: usize,
    color: [u8; 4],
) {
    fill_rect_bgra(buffer, rx, ry, rw, thickness, color);
    fill_rect_bgra(buffer, rx, ry + rh.saturating_sub(thickness), rw, thickness, color);
    fill_rect_bgra(buffer, rx, ry, thickness, rh, color);
    fill_rect_bgra(buffer, rx + rw.saturating_sub(thickness), ry, thickness, rh, color);
}

/// Draws a horizontal progress / gauge bar with filled percentage.
pub fn draw_bar_bgra(
    buffer: &mut [u8],
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    fraction: f32,
    fg: [u8; 4],
    bg: [u8; 4],
) {
    fill_rect_bgra(buffer, x, y, w, h, bg);
    let filled_w = ((w as f32) * fraction.clamp(0.0, 1.0)) as usize;
    if filled_w > 0 {
        fill_rect_bgra(buffer, x, y, filled_w, h, fg);
    }
    draw_border_bgra(buffer, x, y, w, h, 1, [0x60, 0x60, 0x60, 0xff]);
}

/// Renders a modern system dashboard directly to the 720x1280 BGRA canvas.
pub fn render_dashboard(data: &DashboardData, buffer: &mut [u8]) {
    let accent = data.theme.accent_bgra();
    let bg = data.theme.bg_bgra();
    let card = data.theme.card_bgra();
    let white = [0xff, 0xff, 0xff, 0xff];
    let gray = [0x99, 0x99, 0x99, 0xff];

    // Background fill
    fill_rect_bgra(buffer, 0, 0, WIDTH, HEIGHT, bg);

    let card_x = 36;
    let card_w = WIDTH - 72; // 648px wide
    // Right text boundary with safe 28px padding from the 684px card border
    let right_x = card_x + card_w - 28;

    let mut current_y = if data.show_header {
        fill_rect_bgra(buffer, 0, 0, WIDTH, 90, card);
        fill_rect_bgra(buffer, 0, 86, WIDTH, 4, accent);
        draw_text_bgra(buffer, WIDTH, HEIGHT, card_x, 32, &data.title, accent, 3);
        110
    } else {
        30
    };

    let slots = [data.slot1, data.slot2, data.slot3, data.slot4, data.slot5];

    for slot in slots {
        match slot {
            DashboardSection::None => {}

            DashboardSection::Clock => {
                let h = 145;
                fill_rect_bgra(buffer, card_x, current_y, card_w, h, card);
                draw_border_bgra(buffer, card_x, current_y, card_w, h, 2, [0x40, 0x38, 0x38, 0xff]);

                let (time_str, date_str) = data.timezone.now_strings(data.use_24h_clock);
                let time_scale = if time_str.len() > 14 { 3 } else { 4 };
                let time_y = if time_scale == 3 { current_y + 32 } else { current_y + 26 };

                draw_text_bgra(buffer, WIDTH, HEIGHT, card_x + 24, time_y, &time_str, white, time_scale);
                draw_text_bgra(buffer, WIDTH, HEIGHT, card_x + 24, current_y + 94, &date_str, accent, 2);

                current_y += h + 20;
            }

            DashboardSection::Cpu => {
                let h = 205;
                fill_rect_bgra(buffer, card_x, current_y, card_w, h, card);
                draw_border_bgra(buffer, card_x, current_y, card_w, h, 2, [0x40, 0x38, 0x38, 0xff]);
                fill_rect_bgra(buffer, card_x, current_y, card_w, 6, accent);

                draw_text_bgra(buffer, WIDTH, HEIGHT, card_x + 24, current_y + 20, "PROCESSOR (CPU)", accent, 3);

                let cpu_short: String = data.cpu_name.chars().take(22).collect();
                draw_text_bgra(buffer, WIDTH, HEIGHT, card_x + 24, current_y + 58, &cpu_short, white, 2);

                let load_str = format!("LOAD: {:.1}%", data.cpu_usage);
                let temp_str = format!("TEMP: {:.1} C", data.cpu_temp);
                draw_text_bgra(buffer, WIDTH, HEIGHT, card_x + 24, current_y + 95, &load_str, white, 2);
                draw_text_right_aligned_bgra(buffer, WIDTH, HEIGHT, right_x, current_y + 95, &temp_str, accent, 2);

                draw_bar_bgra(
                    buffer,
                    card_x + 24,
                    current_y + 130,
                    card_w - 48,
                    24,
                    data.cpu_usage / 100.0,
                    accent,
                    [0x18, 0x14, 0x14, 0xff],
                );

                let freq_str = format!("Clock: {:.2} GHz", data.cpu_freq_mhz / 1000.0);
                draw_text_bgra(buffer, WIDTH, HEIGHT, card_x + 24, current_y + 168, &freq_str, gray, 2);

                current_y += h + 20;
            }

            DashboardSection::Gpu => {
                let h = 205;
                fill_rect_bgra(buffer, card_x, current_y, card_w, h, card);
                draw_border_bgra(buffer, card_x, current_y, card_w, h, 2, [0x40, 0x38, 0x38, 0xff]);
                fill_rect_bgra(buffer, card_x, current_y, card_w, 6, accent);

                draw_text_bgra(buffer, WIDTH, HEIGHT, card_x + 24, current_y + 20, "GRAPHICS (GPU)", accent, 3);

                let gpu_short: String = data.gpu_name.chars().take(22).collect();
                draw_text_bgra(buffer, WIDTH, HEIGHT, card_x + 24, current_y + 58, &gpu_short, white, 2);

                let load_str = format!("LOAD: {:.0}%", data.gpu_usage);
                let temp_str = format!("TEMP: {:.0} C", data.gpu_temp);
                draw_text_bgra(buffer, WIDTH, HEIGHT, card_x + 24, current_y + 95, &load_str, white, 2);
                draw_text_right_aligned_bgra(buffer, WIDTH, HEIGHT, right_x, current_y + 95, &temp_str, accent, 2);

                draw_bar_bgra(
                    buffer,
                    card_x + 24,
                    current_y + 130,
                    card_w - 48,
                    24,
                    data.gpu_usage / 100.0,
                    accent,
                    [0x18, 0x14, 0x14, 0xff],
                );

                let vram_str = format!("VRAM: {:.1} GB / {:.1} GB", data.gpu_vram_used_gb, data.gpu_vram_total_gb);
                draw_text_bgra(buffer, WIDTH, HEIGHT, card_x + 24, current_y + 168, &vram_str, gray, 2);

                current_y += h + 20;
            }

            DashboardSection::Ram => {
                let h = 180;
                fill_rect_bgra(buffer, card_x, current_y, card_w, h, card);
                draw_border_bgra(buffer, card_x, current_y, card_w, h, 2, [0x40, 0x38, 0x38, 0xff]);
                fill_rect_bgra(buffer, card_x, current_y, card_w, 6, accent);

                draw_text_bgra(buffer, WIDTH, HEIGHT, card_x + 24, current_y + 20, "MEMORY (RAM)", accent, 3);

                let mem_pct = if data.mem_total_gb > 0.0 {
                    (data.mem_used_gb / data.mem_total_gb) * 100.0
                } else {
                    0.0
                };
                let mem_str = format!("{:.1} GB / {:.1} GB", data.mem_used_gb, data.mem_total_gb);
                let mem_pct_str = format!("{:.1}%", mem_pct);
                draw_text_bgra(buffer, WIDTH, HEIGHT, card_x + 24, current_y + 68, &mem_str, white, 2);
                draw_text_right_aligned_bgra(buffer, WIDTH, HEIGHT, right_x, current_y + 68, &mem_pct_str, accent, 2);

                draw_bar_bgra(
                    buffer,
                    card_x + 24,
                    current_y + 110,
                    card_w - 48,
                    24,
                    mem_pct / 100.0,
                    accent,
                    [0x18, 0x14, 0x14, 0xff],
                );

                current_y += h + 20;
            }

            DashboardSection::Network => {
                let h = 180;
                fill_rect_bgra(buffer, card_x, current_y, card_w, h, card);
                draw_border_bgra(buffer, card_x, current_y, card_w, h, 2, [0x40, 0x38, 0x38, 0xff]);
                fill_rect_bgra(buffer, card_x, current_y, card_w, 6, accent);

                draw_text_bgra(buffer, WIDTH, HEIGHT, card_x + 24, current_y + 20, "NETWORK BANDWIDTH", accent, 3);

                let down_str = if data.net_rx_kbps >= 1024.0 {
                    format!("DOWN: {:.1} MB/s", data.net_rx_kbps / 1024.0)
                } else {
                    format!("DOWN: {:.0} KB/s", data.net_rx_kbps)
                };

                let up_str = if data.net_tx_kbps >= 1024.0 {
                    format!("UP: {:.1} MB/s", data.net_tx_kbps / 1024.0)
                } else {
                    format!("UP: {:.0} KB/s", data.net_tx_kbps)
                };

                draw_text_bgra(buffer, WIDTH, HEIGHT, card_x + 24, current_y + 68, &down_str, white, 2);
                draw_text_right_aligned_bgra(buffer, WIDTH, HEIGHT, right_x, current_y + 68, &up_str, accent, 2);

                let combined_speed = (data.net_rx_kbps + data.net_tx_kbps) / 10240.0; // 0 to 10 MB/s bar
                draw_bar_bgra(
                    buffer,
                    card_x + 24,
                    current_y + 110,
                    card_w - 48,
                    24,
                    combined_speed.min(1.0),
                    accent,
                    [0x18, 0x14, 0x14, 0xff],
                );

                current_y += h + 20;
            }
        }
    }

    // Footer info card
    if data.show_footer {
        let footer_h = 80;
        fill_rect_bgra(buffer, card_x, current_y, card_w, footer_h, card);
        draw_border_bgra(buffer, card_x, current_y, card_w, footer_h, 1, [0x40, 0x38, 0x38, 0xff]);

        draw_text_bgra(buffer, WIDTH, HEIGHT, card_x + 24, current_y + 18, "HARDWARE STREAM ACTIVE", accent, 2);

        let status_str = if data.fps > 0.0 {
            format!("{:.1} FPS (Pacing: {}ms)", data.fps, data.pacing_ms)
        } else {
            format!("Pacing: {}ms", data.pacing_ms)
        };
        // Clean single line without any "720x1280 Tear-Free" text
        draw_text_bgra(buffer, WIDTH, HEIGHT, card_x + 24, current_y + 48, &status_str, white, 2);
    }

    // Bottom Decorative Bar
    fill_rect_bgra(buffer, 0, HEIGHT - 16, WIDTH, 16, accent);
}

/// Renders test patterns for tearing and color verification.
pub fn render_pattern(pattern_name: &str, frame_count: u32, buffer: &mut [u8]) {
    match pattern_name {
        "vertical-split" => {
            let cols = 3;
            let col_w = WIDTH / cols;
            let colors: [[u8; 4]; 3] = [
                if frame_count % 2 == 0 { [0x00, 0x00, 0xff, 0xff] } else { [0x00, 0xff, 0x00, 0xff] },
                if frame_count % 2 == 0 { [0x00, 0xff, 0x00, 0xff] } else { [0xff, 0x00, 0x00, 0xff] },
                if frame_count % 2 == 0 { [0xff, 0x00, 0x00, 0xff] } else { [0x00, 0x00, 0xff, 0xff] },
            ];
            for c in 0..cols {
                fill_rect_bgra(buffer, c * col_w, 0, col_w, HEIGHT, colors[c]);
            }
            let label = format!("FRAME {}", frame_count);
            draw_text_bgra(buffer, WIDTH, HEIGHT, 240, 600, &label, [0xff, 0xff, 0xff, 0xff], 4);
        }
        "colorbars" => {
            let colors: [[u8; 4]; 7] = [
                [0xff, 0xff, 0xff, 0xff], // White
                [0x00, 0xff, 0xff, 0xff], // Yellow
                [0xff, 0xff, 0x00, 0xff], // Cyan
                [0x00, 0xff, 0x00, 0xff], // Green
                [0xff, 0x00, 0xff, 0xff], // Magenta
                [0x00, 0x00, 0xff, 0xff], // Red
                [0xff, 0x00, 0x00, 0xff], // Blue
            ];
            let col_w = WIDTH / 7;
            for i in 0..7 {
                fill_rect_bgra(buffer, i * col_w, 0, col_w, HEIGHT - 180, colors[i]);
            }
            fill_rect_bgra(buffer, 0, HEIGHT - 180, WIDTH, 180, [0x20, 0x20, 0x20, 0xff]);
            let label = format!("SMPTE COLOR TEST | FRAME {}", frame_count);
            draw_text_bgra(buffer, WIDTH, HEIGHT, 60, HEIGHT - 100, &label, [0xff, 0xff, 0xff, 0xff], 3);
        }
        "gradient" => {
            for y in 0..HEIGHT {
                let shift = (frame_count * 4) as usize;
                let r = ((y + shift) % 256) as u8;
                let g = ((y * 2 + shift) % 256) as u8;
                let b = (255 - r) as u8;
                for x in 0..WIDTH {
                    let offset = (y * WIDTH + x) * 4;
                    buffer[offset] = b;
                    buffer[offset + 1] = g;
                    buffer[offset + 2] = r;
                    buffer[offset + 3] = 0xff;
                }
            }
            draw_text_bgra(buffer, WIDTH, HEIGHT, 160, 600, "RAINBOW GRADIENT", [0xff, 0xff, 0xff, 0xff], 4);
        }
        _ => {
            fill_rect_bgra(buffer, 0, 0, WIDTH, HEIGHT, [0x00, 0x00, 0x00, 0xff]);
            draw_text_bgra(buffer, WIDTH, HEIGHT, 100, 600, "CUSTOM STREAM FRAME", [0x00, 0x00, 0xff, 0xff], 4);
        }
    }
}

/// Converts a DynamicImage to 720x1280 native BGRA8888 buffer, handling scaling & letterboxing.
pub fn image_to_bgra(img: &DynamicImage, buffer: &mut [u8], crop_fill: bool) {
    let (orig_w, orig_h) = img.dimensions();
    if orig_w == 0 || orig_h == 0 {
        return;
    }

    fill_rect_bgra(buffer, 0, 0, WIDTH, HEIGHT, [0x00, 0x00, 0x00, 0xff]);

    let resized = if crop_fill {
        // Crop & Fill: scale to cover entire 720x1280
        let scale = ((WIDTH as f32) / orig_w as f32).max((HEIGHT as f32) / orig_h as f32);
        let nw = ((orig_w as f32) * scale).round() as u32;
        let nh = ((orig_h as f32) * scale).round() as u32;
        let scaled = img.resize_exact(nw, nh, FilterType::Triangle);
        let x = (nw.saturating_sub(WIDTH as u32)) / 2;
        let y = (nh.saturating_sub(HEIGHT as u32)) / 2;
        scaled.crop_imm(x, y, WIDTH as u32, HEIGHT as u32)
    } else {
        // Fit & Letterbox: preserve aspect ratio
        img.resize(WIDTH as u32, HEIGHT as u32, FilterType::Triangle)
    };

    let (rw, rh) = resized.dimensions();
    let offset_x = (WIDTH.saturating_sub(rw as usize)) / 2;
    let offset_y = (HEIGHT.saturating_sub(rh as usize)) / 2;

    let rgba = resized.to_rgba8();
    for y in 0..rh as usize {
        for x in 0..rw as usize {
            let px = rgba.get_pixel(x as u32, y as u32);
            let dst_x = offset_x + x;
            let dst_y = offset_y + y;
            if dst_x < WIDTH && dst_y < HEIGHT {
                let off = (dst_y * WIDTH + dst_x) * 4;
                buffer[off] = px[2];     // B
                buffer[off + 1] = px[1]; // G
                buffer[off + 2] = px[0]; // R
                buffer[off + 3] = px[3]; // A
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_theme_color_presets() {
        assert_eq!(ThemeColor::parse("red"), Some(ThemeColor::RogRed));
        assert_eq!(ThemeColor::parse("cyan"), Some(ThemeColor::CyberCyan));
        assert_eq!(ThemeColor::parse("green"), Some(ThemeColor::MatrixGreen));
        assert_eq!(ThemeColor::parse("amber"), Some(ThemeColor::AmberGold));
        assert_eq!(ThemeColor::parse("purple"), Some(ThemeColor::NeonPurple));
    }

    #[test]
    fn test_theme_color_hex_and_rgb_parsing() {
        assert_eq!(ThemeColor::parse("#FF3C3C"), Some(ThemeColor::Custom([255, 60, 60])));
        assert_eq!(ThemeColor::parse("00FF00"), Some(ThemeColor::Custom([0, 255, 0])));
        assert_eq!(ThemeColor::parse("255, 128, 64"), Some(ThemeColor::Custom([255, 128, 64])));
        assert_eq!(ThemeColor::parse("rgb(10, 20, 30)"), Some(ThemeColor::Custom([10, 20, 30])));
        assert_eq!(ThemeColor::parse("invalid"), None);
        assert_eq!(ThemeColor::parse("aébcd"), None);
    }

    #[test]
    fn test_theme_color_properties() {
        let custom = ThemeColor::Custom([255, 128, 64]);
        assert_eq!(custom.rgb(), [255, 128, 64]);
        assert_eq!(custom.hex(), "#FF8040");
        assert_eq!(custom.accent_bgra(), [64, 128, 255, 255]);
    }

    #[test]
    fn test_timezone_config_strings() {
        let (utc_time, utc_date) = TimezoneConfig::Utc.now_strings(true);
        assert!(utc_time.contains("UTC"));
        assert!(!utc_date.is_empty());

        let (local_time, local_date) = TimezoneConfig::Local.now_strings(false);
        assert!(!local_time.is_empty());
        assert!(!local_date.is_empty());

        let (custom_time, custom_date) = TimezoneConfig::Custom(600).now_strings(true);
        assert!(custom_time.contains("UTC+10:00"));
        assert!(!custom_date.is_empty());
    }
}
