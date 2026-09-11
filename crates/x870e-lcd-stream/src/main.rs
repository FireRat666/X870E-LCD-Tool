//! ASUS ROG Crosshair X870E LCD Live Streamer (CLI & GUI).

mod font;
mod gui;
mod media;
mod renderer;
mod streamer;
mod video;

use std::io::{self, Read};
use std::path::PathBuf;
use std::time::Duration;
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use tracing::info;
use x870e_lcd_core::{FRAME_RAW_SIZE, HardwareMonitor};

use crate::gui::StreamGuiApp;
use crate::renderer::{DashboardData, ThemeColor};

#[derive(Parser)]
#[command(name = "x870e-lcd-stream")]
#[command(about = "Live video, telemetry dashboard, and media streaming utility for ASUS ROG X870E LCD")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Launch the interactive graphical streaming control panel (default if no command given)
    Gui,

    /// Stream live system telemetry dashboard (clock, CPU, GPU, RAM)
    Dashboard {
        /// Header title text
        #[arg(short, long, default_value = "ROG CROSSHAIR X870E")]
        title: String,

        /// Dashboard theme color (rog, cyber, matrix, amber, purple)
        #[arg(long, default_value = "rog")]
        theme: String,

        /// Hardware settling delay between frames in milliseconds (default: 250ms = ~2.0 FPS)
        #[arg(short, long, default_value_t = 250)]
        pacing_ms: u64,
    },

    /// Stream video file (MP4, MKV, WebM, GIF, etc.) directly to the ROG LCD panel
    Video {
        /// Path to the video file
        path: PathBuf,

        /// Hardware settling delay between frames in milliseconds (default: 200ms)
        #[arg(short, long, default_value_t = 200)]
        pacing_ms: u64,

        /// Scaling mode: fill (crop to 9:16), fit (letterbox), stretch
        #[arg(long, default_value = "fill")]
        scale: String,

        /// Rotation in degrees: 0, 90, 180, 270
        #[arg(long, default_value_t = 0)]
        rotation: u32,
    },

    /// Stream test pattern (vertical-split, colorbars, gradient)
    Pattern {
        /// Pattern name
        #[arg(default_value = "vertical-split")]
        pattern: String,

        /// Hardware settling delay in milliseconds
        #[arg(short, long, default_value_t = 250)]
        pacing_ms: u64,
    },

    /// Stream a static image
    Image {
        /// Path to the image file
        path: PathBuf,

        /// Crop to fill display instead of letterboxing
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
        fill: bool,
    },

    /// Read raw 720x1280 BGRA8888 frames from standard input (e.g. piped from ffmpeg)
    Pipe {
        /// Hardware settling delay in milliseconds (default: 250ms)
        #[arg(short, long, default_value_t = 250)]
        pacing_ms: u64,
    },
}

/// Main entry point for the x870e-lcd-stream application.
fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match cli.command {
        None | Some(Commands::Gui) => launch_gui(),
        Some(Commands::Dashboard { title, theme, pacing_ms }) => run_cli_dashboard(title, &theme, pacing_ms),
        Some(Commands::Video { path, pacing_ms, scale, rotation }) => run_cli_video(&path, pacing_ms, &scale, rotation),
        Some(Commands::Pattern { pattern, pacing_ms }) => run_cli_pattern(&pattern, pacing_ms),
        Some(Commands::Image { path, fill }) => run_cli_image(&path, fill),
        Some(Commands::Pipe { pacing_ms }) => run_cli_pipe(pacing_ms),
    }
}

/// Launches the interactive desktop GUI for the LCD live streamer.
fn launch_gui() -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("ROG X870E LCD Live Streamer")
            .with_inner_size(eframe::egui::Vec2::new(960.0, 780.0))
            .with_min_inner_size(eframe::egui::Vec2::new(760.0, 600.0)),
        ..Default::default()
    };

    eframe::run_native(
        "ROG X870E LCD Live Streamer",
        options,
        Box::new(|cc| Ok(Box::new(StreamGuiApp::new(cc)))),
    )
    .map_err(|e| anyhow::anyhow!("GUI error: {}", e))
}

/// Streams real-time hardware telemetry dashboard to the LCD panel via CLI.
fn run_cli_dashboard(title: String, theme_name: &str, pacing_ms: u64) -> Result<()> {
    let theme = match theme_name.to_lowercase().as_str() {
        "cyber" => ThemeColor::CyberCyan,
        "matrix" => ThemeColor::MatrixGreen,
        "amber" => ThemeColor::AmberGold,
        "purple" => ThemeColor::NeonPurple,
        _ => ThemeColor::RogRed,
    };

    let mut hw_mon = HardwareMonitor::new();
    let snap = hw_mon.refresh();

    let mut dash_data = DashboardData::default();
    dash_data.title = title;
    dash_data.theme = theme;
    dash_data.cpu_name = snap.cpu_name;
    dash_data.pacing_ms = pacing_ms;

    println!("Starting live dashboard stream to ROG X870E LCD (pacing: {}ms)...", pacing_ms);
    println!("Press Ctrl+C to stop.");

    let handle = streamer::start_stream_worker(
        move |buf, stats| {
            let snap = hw_mon.refresh();
            dash_data.cpu_usage = snap.cpu_usage_pct;
            if let Some(t) = snap.cpu_temp_c {
                dash_data.cpu_temp = t;
            }
            if snap.cpu_freq_mhz > 0 {
                dash_data.cpu_freq_mhz = snap.cpu_freq_mhz as f32;
            }
            dash_data.mem_used_gb = snap.ram_used_gb;
            dash_data.mem_total_gb = snap.ram_total_gb;

            dash_data.gpu_name = snap.gpu_name;
            if let Some(u) = snap.gpu_usage_pct { dash_data.gpu_usage = u; }
            if let Some(t) = snap.gpu_temp_c { dash_data.gpu_temp = t; }
            if let Some(v) = snap.gpu_mem_used_gb { dash_data.gpu_vram_used_gb = v; }
            if let Some(tot) = snap.gpu_mem_total_gb { dash_data.gpu_vram_total_gb = tot; }

            dash_data.net_rx_kbps = snap.net_rx_kbps;
            dash_data.net_tx_kbps = snap.net_tx_kbps;
            dash_data.fps = stats.get_fps();

            renderer::render_dashboard(&dash_data, buf);
            Ok(true)
        },
        pacing_ms,
    )?;

    // Wait for Ctrl+C or worker termination
    wait_for_stream(handle)
}

/// Waits for a stream to complete or until interrupted by SIGINT (Ctrl+C).
fn wait_for_stream(mut handle: streamer::StreamerHandle) -> Result<()> {
    let running = handle.stats().running.clone();
    let _ = ctrlc::set_handler(move || {
        running.store(false, std::sync::atomic::Ordering::SeqCst);
    });

    while handle.stats().running.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(100));
    }

    handle.stop();
    Ok(())
}

/// Plays and streams a video file to the LCD panel via CLI.
fn run_cli_video(path: &PathBuf, pacing_ms: u64, scale: &str, rotation: u32) -> Result<()> {
    let mut config = media::MediaConfig::default();
    config.scale_mode = match scale.to_lowercase().as_str() {
        "fit" | "letterbox" => media::ScaleMode::Fit,
        "stretch" => media::ScaleMode::Stretch,
        _ => media::ScaleMode::Fill,
    };
    config.rotation = match rotation {
        90 => media::Rotation::Rot90,
        180 => media::Rotation::Rot180,
        270 => media::Rotation::Rot270,
        _ => media::Rotation::Rot0,
    };

    let mut player = video::VideoPlayer::new();
    player.load(path, config)?;

    println!("Streaming video '{}' to ROG X870E LCD (pacing: {}ms)...", path.display(), pacing_ms);
    println!("Press Ctrl+C to stop.");

    let handle = streamer::start_stream_worker(
        move |buf, _stats| {
            let ok = player.read_frame(buf);
            Ok(ok)
        },
        pacing_ms,
    )?;

    wait_for_stream(handle)
}

/// Streams a dynamic test pattern to the LCD panel via CLI.
fn run_cli_pattern(pattern: &str, pacing_ms: u64) -> Result<()> {
    let pattern_owned = pattern.to_string();
    let mut frame_count = 0u32;

    println!("Streaming pattern '{}' to ROG X870E LCD...", pattern);
    println!("Press Ctrl+C to stop.");

    let handle = streamer::start_stream_worker(
        move |buf, _stats| {
            renderer::render_pattern(&pattern_owned, frame_count, buf);
            frame_count = frame_count.wrapping_add(1);
            Ok(true)
        },
        pacing_ms,
    )?;

    wait_for_stream(handle)
}

/// Renders and displays a single static image on the LCD panel via FrameStream mode.
fn run_cli_image(path: &PathBuf, fill: bool) -> Result<()> {
    let img = image::open(path).with_context(|| format!("Failed to open image file: {}", path.display()))?;
    let mut frame_buf = vec![0u8; FRAME_RAW_SIZE];
    renderer::image_to_bgra(&img, &mut frame_buf, fill);

    let device = x870e_lcd_core::LcdDevice::open()?;
    device.enter_frame_stream()?;

    println!("Displaying image from {} on LCD panel...", path.display());
    device.send_stream_frame(&frame_buf)?;
    println!("Image rendered cleanly in FrameStream mode.");

    Ok(())
}

/// Streams raw 720x1280 BGRA frames read from standard input to the LCD panel.
fn run_cli_pipe(pacing_ms: u64) -> Result<()> {
    println!("Streaming raw 720x1280 BGRA frames from stdin (pacing: {}ms)...", pacing_ms);
    let mut stdin = io::stdin();

    let handle = streamer::start_stream_worker(
        move |buf, _stats| {
            match stdin.read_exact(buf) {
                Ok(_) => Ok(true),
                Err(ref e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    info!("End of stdin stream reached.");
                    Ok(false)
                }
                Err(e) => bail!("Stdin read error: {}", e),
            }
        },
        pacing_ms,
    )?;

    wait_for_stream(handle)
}
