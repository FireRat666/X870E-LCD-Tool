//! Core library for ASUS ROG Crosshair X870E Extreme 5-inch Motherboard LCD Panel.

pub mod protocol;
pub mod device;
pub mod hwmon;
pub mod image;

pub use device::{LcdDevice, LcdError};
pub use protocol::{DisplayMode, HwLayout};
pub use hwmon::{HardwareMonitor, TelemetrySnapshot, SensorMetric};
pub use image::{process_image, load_and_prepare_jpeg, FitMode};
