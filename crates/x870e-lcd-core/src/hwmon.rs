//! Hardware telemetry monitoring (CPU, GPU, RAM, Fan) for Linux.

use std::fs;
use std::path::Path;
use std::process::Command;
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

#[derive(Debug, Clone, Default)]
pub struct TelemetrySnapshot {
    pub cpu_name: String,
    pub cpu_temp_c: Option<f32>,
    pub cpu_usage_pct: f32,
    pub cpu_freq_mhz: u64,
    pub gpu_name: String,
    pub gpu_temp_c: Option<f32>,
    pub gpu_usage_pct: Option<f32>,
    pub gpu_mem_used_gb: Option<f32>,
    pub gpu_mem_total_gb: Option<f32>,
    pub ram_used_gb: f32,
    pub ram_total_gb: f32,
    pub ram_usage_pct: f32,
    pub net_rx_kbps: f32,
    pub net_tx_kbps: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SensorMetric {
    #[default]
    CpuTemp,
    CpuUsage,
    CpuFreq,
    GpuTemp,
    GpuUsage,
    RamUsed,
    RamUsage,
    CpuModel,
}

impl SensorMetric {
    /// Returns all available hardware telemetry metrics.
    pub fn all() -> &'static [SensorMetric] {
        &[
            SensorMetric::CpuTemp,
            SensorMetric::CpuUsage,
            SensorMetric::CpuFreq,
            SensorMetric::GpuTemp,
            SensorMetric::GpuUsage,
            SensorMetric::RamUsed,
            SensorMetric::RamUsage,
            SensorMetric::CpuModel,
        ]
    }

    /// Returns the human-readable display name for this metric.
    pub fn display_name(&self) -> &'static str {
        match self {
            SensorMetric::CpuTemp => "CPU Temperature (°C)",
            SensorMetric::CpuUsage => "CPU Utilization (%)",
            SensorMetric::CpuFreq => "CPU Frequency (GHz)",
            SensorMetric::GpuTemp => "GPU Temperature (°C)",
            SensorMetric::GpuUsage => "GPU Utilization (%)",
            SensorMetric::RamUsed => "RAM Used (GB)",
            SensorMetric::RamUsage => "RAM Usage (%)",
            SensorMetric::CpuModel => "CPU Model Name",
        }
    }

    /// Formats the metric into (Label, Value) suitable for Cmd 0x53
    pub fn format(&self, snap: &TelemetrySnapshot) -> (&'static str, String) {
        match self {
            SensorMetric::CpuTemp => {
                let val = snap.cpu_temp_c
                    .map(|t| format!("{:.1}\u{2103}", t))
                    .unwrap_or_else(|| "N/A".to_string());
                ("CPU", val)
            }
            SensorMetric::CpuUsage => ("CPU", format!("{:.0}%", snap.cpu_usage_pct)),
            SensorMetric::CpuFreq => {
                let val = if snap.cpu_freq_mhz > 0 {
                    format!("{:.2}\u{3393}", snap.cpu_freq_mhz as f32 / 1000.0)
                } else {
                    "N/A".to_string()
                };
                ("CPU", val)
            }
            SensorMetric::GpuTemp => {
                let val = snap.gpu_temp_c
                    .map(|t| format!("{:.1}\u{2103}", t))
                    .unwrap_or_else(|| "N/A".to_string());
                ("GPU", val)
            }
            SensorMetric::GpuUsage => {
                let val = snap.gpu_usage_pct
                    .map(|u| format!("{:.0}%", u))
                    .unwrap_or_else(|| "N/A".to_string());
                ("GPU", val)
            }
            SensorMetric::RamUsed => ("RAM", format!("{:.1}GB", snap.ram_used_gb)),
            SensorMetric::RamUsage => ("RAM", format!("{:.0}%", snap.ram_usage_pct)),
            SensorMetric::CpuModel => {
                let short_name = snap.cpu_name
                    .replace("AMD Ryzen ", "Ryzen ")
                    .replace(" 16-Core Processor", "")
                    .replace(" Processor", "");
                ("Processor", short_name)
            }
        }
    }
}

use std::time::Instant;

pub struct HardwareMonitor {
    sys: System,
    last_net_time: Instant,
    last_rx_bytes: u64,
    last_tx_bytes: u64,
}

impl HardwareMonitor {
    /// Initializes a new hardware monitor with CPU, RAM, and network baseline telemetry.
    pub fn new() -> Self {
        let mut sys = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::everything())
                .with_memory(MemoryRefreshKind::everything()),
        );
        sys.refresh_all();
        let (rx, tx) = read_net_bytes().unwrap_or((0, 0));
        Self {
            sys,
            last_net_time: Instant::now(),
            last_rx_bytes: rx,
            last_tx_bytes: tx,
        }
    }

    /// Refreshes all system metrics and returns a current telemetry snapshot.
    pub fn refresh(&mut self) -> TelemetrySnapshot {
        self.sys.refresh_cpu_all();
        self.sys.refresh_memory();

        let cpu_name = self.sys.cpus().first()
            .map(|c| c.brand().trim().to_string())
            .unwrap_or_else(|| "AMD Ryzen".to_string());

        let cpu_freq_mhz = self.sys.cpus().first()
            .map(|c| c.frequency())
            .unwrap_or(0);

        let cpu_usage_pct = self.sys.global_cpu_usage();
        let cpu_temp_c = read_cpu_temp();

        let (gpu_name, gpu_temp_c, gpu_usage_pct, gpu_mem_used_gb, gpu_mem_total_gb) = read_gpu_info();

        let ram_total = self.sys.total_memory() as f32 / (1024.0 * 1024.0 * 1024.0);
        let ram_used = self.sys.used_memory() as f32 / (1024.0 * 1024.0 * 1024.0);
        let ram_usage_pct = if ram_total > 0.0 { (ram_used / ram_total) * 100.0 } else { 0.0 };

        // Calculate network transfer rates (KB/s)
        let mut net_rx_kbps = 0.0;
        let mut net_tx_kbps = 0.0;
        if let Some((current_rx, current_tx)) = read_net_bytes() {
            let elapsed_secs = self.last_net_time.elapsed().as_secs_f32().max(0.1);
            let rx_diff = current_rx.saturating_sub(self.last_rx_bytes);
            let tx_diff = current_tx.saturating_sub(self.last_tx_bytes);
            net_rx_kbps = (rx_diff as f32 / elapsed_secs) / 1024.0;
            net_tx_kbps = (tx_diff as f32 / elapsed_secs) / 1024.0;

            self.last_rx_bytes = current_rx;
            self.last_tx_bytes = current_tx;
            self.last_net_time = Instant::now();
        }

        TelemetrySnapshot {
            cpu_name,
            cpu_temp_c,
            cpu_usage_pct,
            cpu_freq_mhz,
            gpu_name,
            gpu_temp_c,
            gpu_usage_pct,
            gpu_mem_used_gb,
            gpu_mem_total_gb,
            ram_used_gb: ram_used,
            ram_total_gb: ram_total,
            ram_usage_pct,
            net_rx_kbps,
            net_tx_kbps,
        }
    }
}

/// Reads non-loopback network bytes from /proc/net/dev
fn read_net_bytes() -> Option<(u64, u64)> {
    let content = fs::read_to_string("/proc/net/dev").ok()?;
    let mut total_rx = 0u64;
    let mut total_tx = 0u64;
    for line in content.lines().skip(2) {
        let mut parts = line.split_whitespace();
        if let Some(iface) = parts.next() {
            if iface.starts_with("lo:") {
                continue;
            }
            if let Some(rx_str) = parts.next() {
                if let Ok(rx) = rx_str.parse::<u64>() {
                    total_rx = total_rx.saturating_add(rx);
                }
            }
            if let Some(tx_str) = parts.nth(7) {
                if let Ok(tx) = tx_str.parse::<u64>() {
                    total_tx = total_tx.saturating_add(tx);
                }
            }
        }
    }
    Some((total_rx, total_tx))
}

/// Reads AMD CPU temperature from k10temp or zenpower in /sys/class/hwmon
fn read_cpu_temp() -> Option<f32> {
    let hwmon_dir = Path::new("/sys/class/hwmon");
    if let Ok(entries) = fs::read_dir(hwmon_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name_file = path.join("name");
            if let Ok(name) = fs::read_to_string(&name_file) {
                let name = name.trim();
                if name == "k10temp" || name == "zenpower" || name == "coretemp" {
                    for temp_file in ["temp1_input", "temp2_input"] {
                        let f = path.join(temp_file);
                        if let Ok(val_str) = fs::read_to_string(&f) {
                            if let Ok(millidegrees) = val_str.trim().parse::<f32>() {
                                return Some(millidegrees / 1000.0);
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// Reads GPU info (tries nvidia-smi first, falls back to amdgpu hwmon)
fn read_gpu_info() -> (String, Option<f32>, Option<f32>, Option<f32>, Option<f32>) {
    if let Ok(output) = Command::new("nvidia-smi")
        .args(["--query-gpu=name,temperature.gpu,utilization.gpu,memory.used,memory.total", "--format=csv,noheader,nounits"])
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            let parts: Vec<&str> = text.trim().split(',').map(|s| s.trim()).collect();
            if parts.len() >= 5 {
                let name = parts[0].replace("NVIDIA GeForce ", "RTX ").replace("NVIDIA ", "");
                let temp = parts[1].parse::<f32>().ok();
                let util = parts[2].parse::<f32>().ok();
                let vram_used = parts[3].parse::<f32>().ok().map(|mb| mb / 1024.0);
                let vram_total = parts[4].parse::<f32>().ok().map(|mb| mb / 1024.0);
                return (name, temp, util, vram_used, vram_total);
            }
        }
    }

    let hwmon_dir = Path::new("/sys/class/hwmon");
    if let Ok(entries) = fs::read_dir(hwmon_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name_file = path.join("name");
            if let Ok(name) = fs::read_to_string(&name_file) {
                if name.trim() == "amdgpu" {
                    let temp = fs::read_to_string(path.join("temp1_input"))
                        .ok()
                        .and_then(|s| s.trim().parse::<f32>().ok())
                        .map(|m| m / 1000.0);
                    return ("Radeon GPU".to_string(), temp, None, None, None);
                }
            }
        }
    }

    ("GPU".to_string(), None, None, None, None)
}
