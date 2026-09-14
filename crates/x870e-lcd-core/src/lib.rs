//! Core library for ASUS ROG Crosshair X870E Extreme 5-inch Motherboard LCD Panel.

pub mod protocol;
pub mod device;
pub mod hwmon;
pub mod image;
pub mod catalog;

pub use device::{LcdDevice, LcdError};
pub use protocol::{
    DisplayMode, HwLayout, LcdModel, LCD_PRODUCT_ID, LCD_PRODUCT_ID_GLACIAL,
    LCD_PRODUCT_ID_EXTREME, SUPPORTED_PRODUCT_IDS, FRAME_BYTES_PER_PIXEL, FRAME_HEIGHT,
    FRAME_RAW_SIZE, FRAME_WIDTH, PANEL_HEIGHT, PANEL_WIDTH,
};
pub use hwmon::{HardwareMonitor, TelemetrySnapshot, SensorMetric};
pub use image::{
    calculate_crop_rect, crop_and_scale, encode_to_jpeg, load_and_prepare_jpeg, process_image,
    FitMode,
};
pub use catalog::{config_dir, thumbnails_dir, SlotCatalog, SlotEntry};
