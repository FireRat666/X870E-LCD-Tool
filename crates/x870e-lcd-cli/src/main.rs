use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use tracing_subscriber::EnvFilter;

use x870e_lcd_core::{
    load_and_prepare_jpeg, DisplayMode, FitMode, HardwareMonitor, HwLayout, LcdDevice,
    SensorMetric,
};

#[derive(Parser)]
#[command(name = "x870e-lcd")]
#[command(about = "Linux management CLI for ASUS ROG Crosshair X870E Extreme 5\" LCD Panel")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Check if the LCD panel is detected and reachable
    Info,

    /// Set display operational mode
    Mode {
        #[arg(value_enum)]
        mode: CliMode,

        /// Preset index for default wallpaper (0..5), custom slot (0..3+), or animation preset (0..1)
        #[arg(short, long, default_value_t = 0)]
        index: u8,

        /// Hardware monitor layout (single, dual, triple, multi)
        #[arg(short, long, value_enum, default_value_t = CliHwLayout::Triple)]
        layout: CliHwLayout,

        /// Hardware monitor theme style (1..4)
        #[arg(short, long, default_value_t = 2)]
        theme: u8,
    },

    /// Set backlight brightness level (0 - 100)
    Brightness {
        #[arg(value_parser = clap::value_parser!(u8).range(0..=100))]
        level: u8,

        /// Keep default wallpaper visible on standby power when PC sleeps or is soft-off
        #[arg(long)]
        standby_wallpaper: Option<bool>,
    },

    /// Configure whether default wallpaper remains on during sleep/hibernate/soft-off
    StandbyWallpaper {
        #[arg(action = clap::ArgAction::Set)]
        enabled: bool,
    },

    /// Erase custom flashed image from SPI flash slot
    Erase {
        /// Custom flash slot to erase (0, 1, 2, 3...)
        #[arg(short, long, default_value_t = 0)]
        slot: u8,
    },

    /// Upload and display a custom image (PNG, JPEG, WebP, BMP, GIF)
    SetImage {
        /// Path to image file
        path: PathBuf,

        /// Fit mode for scaling to 720x1280
        #[arg(short, long, value_enum, default_value_t = CliFitMode::Cover)]
        fit: CliFitMode,

        /// JPEG quality (1 - 100)
        #[arg(short, long, default_value_t = 90)]
        quality: u8,

        /// Custom storage slot on panel (0, 1, 2, 3...)
        #[arg(short, long, default_value_t = 0)]
        slot: u8,
    },

    /// Run hardware telemetry daemon, updating CPU/GPU/fan stats
    Monitor {
        /// Update interval in seconds
        #[arg(short, long, default_value_t = 1.0)]
        interval: f32,

        /// Layout style
        #[arg(short, long, value_enum, default_value_t = CliHwLayout::Triple)]
        layout: CliHwLayout,

        /// Theme style (1..4, Multi-info requires Theme 3)
        #[arg(short, long, default_value_t = 3)]
        theme: u8,

        /// Temperature threshold in Celsius for visual warning alert (e.g. 75, 80, 85, 90, 95, 100)
        #[arg(long)]
        temp_warning: Option<u32>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliMode {
    Off,
    Monitor,
    Animation,
    DefaultWallpaper,
    CustomSlot,
    Image, // Alias for CustomSlot
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliFitMode {
    Cover,
    Fit,
    Stretch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliHwLayout {
    Single,
    Dual,
    Triple,
    Multi,
}

impl From<CliFitMode> for FitMode {
    fn from(mode: CliFitMode) -> Self {
        match mode {
            CliFitMode::Cover => FitMode::Cover,
            CliFitMode::Fit => FitMode::Fit,
            CliFitMode::Stretch => FitMode::Stretch,
        }
    }
}

impl From<CliHwLayout> for HwLayout {
    fn from(layout: CliHwLayout) -> Self {
        match layout {
            CliHwLayout::Single => HwLayout::Single,
            CliHwLayout::Dual => HwLayout::Dual,
            CliHwLayout::Triple => HwLayout::Triple,
            CliHwLayout::Multi => HwLayout::Multi,
        }
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Info => {
            println!("Connecting to ASUS LCD Panel (0b05:1c83)...");
            let _dev = LcdDevice::open().context("Failed to connect to LCD panel. Ensure udev rules are installed.")?;
            println!("✓ Found and connected to ASUS Motherboard LCD Panel!");
            println!("  Resolution: 720 x 1280 (Portrait)");
            println!("  Interfaces: Interface 0 (Bulk EP2 OUT) & Interface 1 (HID Report 0xEC)");
        }

        Commands::Mode { mode, index, layout, mut theme } => {
            let dev = LcdDevice::open()?;
            let hw_layout: HwLayout = layout.into();
            theme = if hw_layout == HwLayout::Multi { 3 } else { theme.clamp(1, 4) };
            match mode {
                CliMode::Off => dev.set_mode(DisplayMode::Off)?,
                CliMode::Monitor => dev.init_hw_monitor(hw_layout, theme)?,
                CliMode::Animation => {
                    let clamped = index.min(1);
                    dev.set_mode(DisplayMode::Animation(clamped))?;
                }
                CliMode::DefaultWallpaper => {
                    let clamped = index.min(5);
                    dev.set_mode(DisplayMode::DefaultWallpaper(clamped))?;
                }
                CliMode::CustomSlot | CliMode::Image => {
                    dev.set_mode(DisplayMode::CustomSlot(index))?;
                }
            }
            println!("✓ Display mode set successfully!");
        }

        Commands::Brightness { level, standby_wallpaper } => {
            let dev = LcdDevice::open()?;
            if let Some(standby) = standby_wallpaper {
                dev.set_brightness_config(level, standby)?;
                println!("✓ Backlight brightness set to {}% (Standby Wallpaper: {})", level, if standby { "ON" } else { "OFF" });
            } else {
                dev.set_brightness(level)?;
                println!("✓ Backlight brightness set to {}%", level);
            }
        }

        Commands::StandbyWallpaper { enabled } => {
            let dev = LcdDevice::open()?;
            dev.set_brightness_config(30, enabled)?;
            println!("✓ Sleep/Standby wallpaper configured to: {}", if enabled { "ENABLED" } else { "DISABLED" });
        }

        Commands::Erase { slot } => {
            println!("Erasing custom image from motherboard SPI flash slot {}...", slot);
            let dev = LcdDevice::open()?;
            dev.delete_custom_image_slot(slot)?;
            println!("✓ Custom image slot {} erased! Screen reverted to factory default wallpaper.", slot);
        }

        Commands::SetImage { path, fit, quality, slot } => {
            if !path.exists() {
                anyhow::bail!("Image file not found: {:?}", path);
            }
            println!("Loading and processing {:?} (Fit: {:?}, Quality: {})...", path, fit, quality);
            let jpeg_data = load_and_prepare_jpeg(&path, fit.into(), quality)
                .context("Failed to process image")?;
            println!("Encoded YUV 4:2:0 JPEG with standard JFIF components: {} bytes. Uploading to custom slot {}...", jpeg_data.len(), slot);

            let dev = LcdDevice::open()?;
            dev.upload_and_display_jpeg(&jpeg_data, slot)?;
            println!("✓ Image successfully uploaded and active in slot {}!", slot);
        }

        Commands::Monitor { interval, layout, mut theme, temp_warning } => {
            let hw_layout: HwLayout = layout.into();
            theme = if hw_layout == HwLayout::Multi { 3 } else { theme.clamp(1, 4) };
            println!("Initializing hardware monitor mode ({:?}, theme {})...", hw_layout, theme);
            let dev = LcdDevice::open()?;
            dev.init_hw_monitor(hw_layout, theme)?;

            if let Some(threshold) = temp_warning {
                println!("Temperature warning alert enabled at threshold: {}°C", threshold);
            }

            let mut hwmon = HardwareMonitor::new();
            println!("Starting telemetry update loop (every {:.1}s). Press Ctrl+C to stop.", interval);

            let delay = Duration::from_secs_f32(interval.max(0.2));
            loop {
                let snap = hwmon.refresh();
                
                // Check if any monitored temperature exceeds warning threshold
                let is_warning = if let Some(thresh) = temp_warning {
                    snap.cpu_temp_c.map(|t| t as u32 >= thresh).unwrap_or(false)
                        || snap.gpu_temp_c.map(|t| t as u32 >= thresh).unwrap_or(false)
                } else {
                    false
                };

                match hw_layout {
                    HwLayout::Single => {
                        let (lbl, val) = if is_warning {
                            ("TEMP WARN", format!("{:.1}\u{2103} !", snap.cpu_temp_c.unwrap_or(0.0)))
                        } else {
                            SensorMetric::CpuTemp.format(&snap)
                        };
                        let _ = dev.update_telemetry_slot(0, &lbl, &val);
                    }
                    HwLayout::Dual => {
                        let (l0, v0) = if is_warning {
                            ("CPU WARN", format!("{:.1}\u{2103} !", snap.cpu_temp_c.unwrap_or(0.0)))
                        } else {
                            SensorMetric::CpuTemp.format(&snap)
                        };
                        let (l1, v1) = SensorMetric::GpuTemp.format(&snap);
                        let _ = dev.update_telemetry_slot(0, &l0, &v0);
                        let _ = dev.update_telemetry_slot(1, &l1, &v1);
                    }
                    HwLayout::Triple => {
                        let (l0, v0) = if is_warning {
                            ("CPU WARN", format!("{:.1}\u{2103} !", snap.cpu_temp_c.unwrap_or(0.0)))
                        } else {
                            SensorMetric::CpuTemp.format(&snap)
                        };
                        let (l1, v1) = SensorMetric::GpuTemp.format(&snap);
                        let (l2, v2) = SensorMetric::RamUsage.format(&snap);
                        let _ = dev.update_telemetry_slot(0, &l0, &v0);
                        let _ = dev.update_telemetry_slot(1, &l1, &v1);
                        let _ = dev.update_telemetry_slot(2, &l2, &v2);
                    }
                    HwLayout::Multi => {
                        let (l0, v0) = if is_warning {
                            ("TEMP ALERT", "OVERHEAT".to_string())
                        } else {
                            SensorMetric::CpuModel.format(&snap)
                        };
                        let (l1, v1) = SensorMetric::GpuTemp.format(&snap);
                        let (l2, v2) = SensorMetric::GpuUsage.format(&snap);
                        let (l3, v3) = SensorMetric::CpuTemp.format(&snap);
                        let (l4, v4) = SensorMetric::RamUsage.format(&snap);
                        let _ = dev.update_telemetry_slot(0, &l0, &v0);
                        let _ = dev.update_telemetry_slot(1, &l1, &v1);
                        let _ = dev.update_telemetry_slot(2, &l2, &v2);
                        let _ = dev.update_telemetry_slot(3, &l3, &v3);
                        let _ = dev.update_telemetry_slot(4, &l4, &v4);
                    }
                }

                sleep(delay);
            }
        }
    }

    Ok(())
}
