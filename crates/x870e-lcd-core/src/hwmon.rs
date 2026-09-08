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
    pub ram_used_gb: f32,
    pub ram_total_gb: f32,
    pub ram_usage_pct: f32,
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

pub struct HardwareMonitor {
    sys: System,
}

impl HardwareMonitor {
    pub fn new() -> Self {
        let mut sys = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::everything())
                .with_memory(MemoryRefreshKind::everything()),
        );
        sys.refresh_all();
        Self { sys }
    }

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

        let (gpu_name, gpu_temp_c, gpu_usage_pct) = read_gpu_info();

        let ram_total = self.sys.total_memory() as f32 / (1024.0 * 1024.0 * 1024.0);
        let ram_used = self.sys.used_memory() as f32 / (1024.0 * 1024.0 * 1024.0);
        let ram_usage_pct = if ram_total > 0.0 { (ram_used / ram_total) * 100.0 } else { 0.0 };

        TelemetrySnapshot {
            cpu_name,
            cpu_temp_c,
            cpu_usage_pct,
            cpu_freq_mhz,
            gpu_name,
            gpu_temp_c,
            gpu_usage_pct,
            ram_used_gb: ram_used,
            ram_total_gb: ram_total,
            ram_usage_pct,
        }
    }
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
fn read_gpu_info() -> (String, Option<f32>, Option<f32>) {
    if let Ok(output) = Command::new("nvidia-smi")
        .args(["--query-gpu=name,temperature.gpu,utilization.gpu", "--format=csv,noheader,nounits"])
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            let parts: Vec<&str> = text.trim().split(',').map(|s| s.trim()).collect();
            if parts.len() >= 3 {
                let name = parts[0].replace("NVIDIA GeForce ", "RTX ");
                let temp = parts[1].parse::<f32>().ok();
                let util = parts[2].parse::<f32>().ok();
                return (name, temp, util);
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
                    return ("Radeon GPU".to_string(), temp, None);
                }
            }
        }
    }

    ("GPU".to_string(), None, None)
}
