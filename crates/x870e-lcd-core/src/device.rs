//! Low-level USB & HID device communication for the LCD panel.

use std::thread::sleep;
use std::time::Duration;
use thiserror::Error;
use tracing::{debug, info};

use crate::protocol::{
    self, upload, DisplayMode, HwLayout, LcdModel, ASUS_VENDOR_ID, BULK_CHUNK_SIZE, BULK_EP_OUT,
    LCD_PRODUCT_ID, PACKET_LEN,
};

#[derive(Error, Debug)]
pub enum LcdError {
    #[error("Device not found (VID: 0x{0:04x}, PID: 0x{1:04x})")]
    NotFound(u16, u16),
    #[error("HID API error: {0}")]
    Hid(#[from] hidapi::HidError),
    #[error("USB error: {0}")]
    Usb(#[from] rusb::Error),
    #[error("Bulk transfer timed out or failed: transferred {transferred}/{expected} bytes")]
    BulkTransferFailed {
        transferred: usize,
        expected: usize,
    },
    #[error("Flash operation timed out: {0}")]
    UploadTimeout(&'static str),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct LcdDevice {
    hid: hidapi::HidDevice,
    usb_handle: rusb::DeviceHandle<rusb::GlobalContext>,
    model: LcdModel,
}

impl LcdDevice {
    /// Returns the detected motherboard LCD model.
    pub fn model(&self) -> LcdModel {
        self.model
    }

    /// Discovers and opens both the HID control channel and Bulk USB channel.
    pub fn open() -> Result<Self, LcdError> {
        let hid_api = hidapi::HidApi::new()?;

        let matched_hid = hid_api.device_list().find_map(|dev| {
            let is_target_interface = dev.interface_number() == 1 || dev.interface_number() == -1;
            if dev.vendor_id() == ASUS_VENDOR_ID && is_target_interface {
                LcdModel::from_product_id(dev.product_id()).map(|model| (dev, model))
            } else {
                None
            }
        });

        let (hid_device, model) = match matched_hid {
            Some((info, model)) => (info.open_device(&hid_api)?, model),
            None => (hid_api.open(ASUS_VENDOR_ID, LCD_PRODUCT_ID)?, LcdModel::Extreme),
        };

        let target_pid = model.product_id();
        let usb_handle = open_bulk_handle(target_pid)?;

        info!(
            "Successfully opened ASUS {} Motherboard LCD Panel (0b05:{:04x})",
            model.display_name(),
            model.product_id()
        );
        Ok(Self {
            hid: hid_device,
            usb_handle,
            model,
        })
    }

    /// Sends a 65-byte HID packet over the command channel
    pub fn send_hid_packet(&self, packet: &[u8; PACKET_LEN]) -> Result<(), LcdError> {
        self.hid.write(packet)?;
        sleep(Duration::from_millis(5));
        Ok(())
    }

    /// Turns display off, or sets mode (static image, animation, or hardware monitor)
    pub fn set_mode(&self, mode: DisplayMode) -> Result<(), LcdError> {
        debug!("Setting display mode to {:?}", mode);
        let sync = protocol::packet_sync();
        self.send_hid_packet(&sync)?;

        let pkt = protocol::packet_set_mode(mode);
        self.send_hid_packet(&pkt)?;
        Ok(())
    }

    /// Sets backlight brightness (0-100%)
    pub fn set_brightness(&self, level: u8) -> Result<(), LcdError> {
        self.set_brightness_config(level, false)
    }

    /// Sets backlight brightness and whether default wallpaper stays lit on standby power (Cmd 0x5c)
    pub fn set_brightness_config(&self, level: u8, standby_wallpaper: bool) -> Result<(), LcdError> {
        let clamped = level.min(100);
        debug!("Setting brightness to {}% (standby wallpaper: {})", clamped, standby_wallpaper);
        let pkt = protocol::packet_set_brightness(clamped, standby_wallpaper);
        self.send_hid_packet(&pkt)?;
        Ok(())
    }

    /// Configures hardware monitor layout and visual theme
    pub fn init_hw_monitor(&self, layout: HwLayout, theme: u8) -> Result<(), LcdError> {
        self.send_hid_packet(&protocol::packet_sync())?;
        self.send_hid_packet(&protocol::packet_hw_layout(layout, theme))?;
        self.send_hid_packet(&protocol::packet_set_mode(DisplayMode::HardwareMonitor))?;
        Ok(())
    }

    /// Updates label and value in a specific hardware monitor slot
    pub fn update_telemetry_slot(&self, slot: u8, label: &str, value: &str) -> Result<(), LcdError> {
        let pkt = protocol::packet_telemetry(slot, label, value);
        self.send_hid_packet(&pkt)?;
        Ok(())
    }

    /// Drains any pending HID input reports from the controller
    pub fn drain_hid(&self) {
        let mut buf = [0u8; 65];
        while let Ok(n) = self.hid.read_timeout(&mut buf, 20) {
            if n == 0 {
                break;
            }
        }
    }

    /// Waits for an expected HID interrupt report matching a predicate, with timeout.
    pub fn wait_for_report<F>(&self, timeout: Duration, predicate: F) -> Result<[u8; 65], LcdError>
    where
        F: Fn(&[u8]) -> bool,
    {
        let mut buf = [0u8; 65];
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if let Ok(n) = self.hid.read_timeout(&mut buf, 50) {
                if n > 0 && predicate(&buf[..n]) {
                    return Ok(buf);
                }
            }
        }
        Err(LcdError::UploadTimeout("Timed out waiting for expected report from controller"))
    }

    /// Explicitly deletes a custom image stored in SPI flash for a specific slot
    pub fn delete_custom_image_slot(&self, slot: u8) -> Result<(), LcdError> {
        info!("Erasing custom image from motherboard SPI flash slot {}...", slot);
        self.drain_hid();

        let (p1, p2) = upload::step0_delete_slot(slot);
        self.send_hid_packet(&p1)?;
        sleep(Duration::from_millis(50));
        self.send_hid_packet(&p2)?;

        // Wait for flash erase ACK (ee 13 00 03)
        let _ = self.wait_for_report(Duration::from_secs(4), |buf| {
            buf.len() >= 4 && buf[0] == 0xee && buf[1] == 0x13 && buf[3] == 0x03
        });

        // Safe exit from flash programming mode
        self.send_hid_packet(&upload::step7_exit())?;
        sleep(Duration::from_millis(50));

        self.send_hid_packet(&protocol::packet_sync())?;
        sleep(Duration::from_millis(50));
        self.set_mode(DisplayMode::DefaultWallpaper(0))?;
        info!("Custom image slot {} successfully erased. Active mode reverted to default wallpaper.", slot);
        Ok(())
    }

    /// Explicitly deletes custom image slot 0 (backward compatibility)
    pub fn delete_custom_image(&self) -> Result<(), LcdError> {
        self.delete_custom_image_slot(0)
    }

    /// Alias for delete_custom_image_slot
    pub fn delete_image_slot(&self, slot: u8) -> Result<(), LcdError> {
        self.delete_custom_image_slot(slot)
    }

    /// Uploads a raw JPEG image buffer and displays it on the screen
    pub fn upload_and_display_jpeg(&self, jpeg_data: &[u8], target_slot: u8) -> Result<(), LcdError> {
        let total_len = jpeg_data.len() as u32;
        info!("Beginning JPEG upload to custom LCD slot {} ({} bytes)", target_slot, total_len);

        self.drain_hid();

        // 1. Upload Handshake (matches official Armoury Crate packet sequence)
        self.send_hid_packet(&upload::step1_prep())?;
        let _ = self.wait_for_report(Duration::from_millis(1000), |buf| {
            buf.len() >= 2 && buf[0] == 0xee && buf[1] == 0x12
        });

        self.send_hid_packet(&upload::step2_sync())?;
        sleep(Duration::from_millis(50));
        self.drain_hid();

        self.send_hid_packet(&upload::step3_enable(target_slot))?;
        sleep(Duration::from_millis(50));
        self.drain_hid();

        self.send_hid_packet(&upload::step4_start())?;
        let _ = self.wait_for_report(Duration::from_millis(1000), |buf| {
            buf.len() >= 4 && buf[0] == 0xee && buf[1] == 0x13 && buf[3] == 0x01
        });

        self.send_hid_packet(&upload::step5_size_header(total_len))?;
        sleep(Duration::from_millis(50));
        self.drain_hid();

        // 2. Pad payload to 4096-byte blocks for USB DMA controller
        let padded_len = (jpeg_data.len() + BULK_CHUNK_SIZE - 1) / BULK_CHUNK_SIZE * BULK_CHUNK_SIZE;
        let mut padded_buffer = Vec::with_capacity(padded_len);
        padded_buffer.extend_from_slice(jpeg_data);
        padded_buffer.resize(padded_len, 0);

        // 3. Stream data over Bulk Endpoint 2 OUT with lockstep ACK
        let mut offset = 0;
        let timeout = Duration::from_secs(5);
        while offset < padded_buffer.len() {
            let chunk = &padded_buffer[offset..offset + BULK_CHUNK_SIZE];
            let transferred = self.usb_handle.write_bulk(BULK_EP_OUT, chunk, timeout)?;
            if transferred != chunk.len() {
                return Err(LcdError::BulkTransferFailed {
                    transferred,
                    expected: chunk.len(),
                });
            }
            offset += BULK_CHUNK_SIZE;
            
            // Wait for MCU SPI flash write ACK (ee 14) for this chunk
            self.wait_for_report(Duration::from_millis(2000), |buf| {
                buf.len() >= 2 && buf[0] == 0xee && buf[1] == 0x14
            })?;
        }

        sleep(Duration::from_millis(50));

        // 4. Finalize upload & wait for flash write completion ACK (ee 13 00 ff)
        self.send_hid_packet(&upload::step6_finalize())?;
        self.wait_for_report(Duration::from_secs(5), |buf| {
            buf.len() >= 4 && buf[0] == 0xee && buf[1] == 0x13 && buf[3] == 0xff
        })?;

        // 5. Exit flash programming mode safely
        self.send_hid_packet(&upload::step7_exit())?;
        sleep(Duration::from_millis(50));

        self.send_hid_packet(&protocol::packet_sync())?;
        sleep(Duration::from_millis(50));

        // 6. Switch to the uploaded custom static image slot
        self.set_mode(DisplayMode::CustomSlot(target_slot))?;
        info!("JPEG upload completed successfully and activated on custom slot {}", target_slot);
        Ok(())
    }

    /// Switches the LCD panel to live uncompressed video frame streaming mode (Mode 0x20)
    pub fn enter_frame_stream(&self) -> Result<(), LcdError> {
        info!("Switching LCD panel to Live Frame Stream mode (0x20)...");
        self.set_mode(DisplayMode::FrameStream)?;
        sleep(Duration::from_millis(100));
        self.drain_hid();
        Ok(())
    }

    /// Sends a single raw 720x1280 BGRA8888 uncompressed video frame (3,686,400 bytes)
    /// to the LCD panel over USB Bulk Endpoint 2 with automatic safety pacing.
    pub fn send_stream_frame(&self, bgra_data: &[u8]) -> Result<(), LcdError> {
        if bgra_data.len() != protocol::FRAME_RAW_SIZE {
            return Err(LcdError::BulkTransferFailed {
                transferred: bgra_data.len(),
                expected: protocol::FRAME_RAW_SIZE,
            });
        }

        self.drain_hid();

        // 1. Announce frame size to LCD firmware
        let announce = protocol::packet_announce_stream_frame(protocol::FRAME_RAW_SIZE as u32);
        self.send_hid_packet(&announce)?;

        // 2. Wait for ACK from controller (0xec 0x7f 0x00 ...)
        self.wait_for_report(Duration::from_millis(500), |buf| {
            buf.len() >= 3 && buf[0] == 0xec && buf[1] == 0x7f && buf[2] == 0x00
        })?;

        // 3. Write raw 3.68 MB frame data via USB Bulk Endpoint 2 OUT
        let transferred = self.usb_handle.write_bulk(
            BULK_EP_OUT,
            bgra_data,
            Duration::from_secs(2),
        )?;

        if transferred != bgra_data.len() {
            return Err(LcdError::BulkTransferFailed {
                transferred,
                expected: bgra_data.len(),
            });
        }

        Ok(())
    }
}

/// Opens the USB bulk interface for the target product ID.
fn open_bulk_handle(target_pid: u16) -> Result<rusb::DeviceHandle<rusb::GlobalContext>, LcdError> {
    for dev in rusb::devices()?.iter() {
        let desc = match dev.device_descriptor() {
            Ok(d) => d,
            Err(_) => continue,
        };
        if desc.vendor_id() == ASUS_VENDOR_ID && desc.product_id() == target_pid {
            let handle = dev.open()?;
            let _ = handle.set_auto_detach_kernel_driver(true);
            handle.claim_interface(0)?;
            return Ok(handle);
        }
    }
    Err(LcdError::NotFound(ASUS_VENDOR_ID, target_pid))
}
