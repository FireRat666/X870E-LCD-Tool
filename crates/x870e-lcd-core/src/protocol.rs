//! Protocol definitions and packet builders for ASUS ROG Crosshair X870E Extreme LCD panel (0b05:1c83).

pub const ASUS_VENDOR_ID: u16 = 0x0b05;
pub const LCD_PRODUCT_ID: u16 = 0x1c83;

pub const REPORT_ID: u8 = 0xec;
pub const PACKET_LEN: usize = 65; // Report ID + 64 bytes

pub const PANEL_WIDTH: u32 = 720;
pub const PANEL_HEIGHT: u32 = 1280;

pub const BULK_EP_OUT: u8 = 0x02;
pub const BULK_CHUNK_SIZE: usize = 4096;

/// Hardware Monitor Layout Type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HwLayout {
    /// Single Info (1 large sensor gauge)
    Single = 0,
    /// Dual Info (2 sensors)
    Dual = 1,
    /// Triple Info (3 sensors)
    #[default]
    Triple = 2,
    /// Multi Info (5 sensors)
    Multi = 4,
}

impl HwLayout {
    pub fn slot_count(&self) -> usize {
        match self {
            HwLayout::Single => 1,
            HwLayout::Dual => 2,
            HwLayout::Triple => 3,
            HwLayout::Multi => 5,
        }
    }
}

/// Display operating modes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayMode {
    /// Screen turned off / standby
    Off,
    /// Factory built-in default wallpaper (Presets 0..=5)
    DefaultWallpaper(u8),
    /// Custom static image from SPI flash memory (Slots 0..=N)
    CustomSlot(u8),
    /// Backward-compatible alias for CustomSlot
    StaticImage(u8),
    /// Built-in animation (preset index: 0 or 1)
    Animation(u8),
    /// Real-time hardware telemetry dashboard
    HardwareMonitor,
    /// Live high-speed uncompressed frame streaming (720x1280 BGRA8888, 3.68MB)
    FrameStream,
}

pub const FRAME_WIDTH: u32 = PANEL_WIDTH;
pub const FRAME_HEIGHT: u32 = PANEL_HEIGHT;
pub const FRAME_BYTES_PER_PIXEL: usize = 4;
pub const FRAME_RAW_SIZE: usize = (FRAME_WIDTH * FRAME_HEIGHT) as usize * FRAME_BYTES_PER_PIXEL; // 3,686,400 bytes

/// Helper to create a zeroed 65-byte HID packet with the 0xEC report ID
#[inline]
pub fn new_packet() -> [u8; PACKET_LEN] {
    let mut pkt = [0u8; PACKET_LEN];
    pkt[0] = REPORT_ID;
    pkt
}

/// Packet for announcing an incoming live stream frame (Cmd 0x7F, Subcmd 0x03)
pub fn packet_announce_stream_frame(len: u32) -> [u8; PACKET_LEN] {
    let mut pkt = new_packet();
    pkt[1] = 0x7f;
    pkt[2] = 0x03;
    pkt[3..7].copy_from_slice(&len.to_le_bytes());
    pkt
}

/// Packet for turning display on/off or changing mode (Cmd 0x51)
pub fn packet_set_mode(mode: DisplayMode) -> [u8; PACKET_LEN] {
    let mut pkt = new_packet();
    pkt[1] = 0x51;
    match mode {
        DisplayMode::Off => {
            pkt[2] = 0x00;
        }
        DisplayMode::DefaultWallpaper(preset) => {
            pkt[2] = 0x11;
            pkt[3] = 0x00; // 0x00 = Default/ROM wallpaper
            pkt[4] = preset;
        }
        DisplayMode::CustomSlot(slot) | DisplayMode::StaticImage(slot) => {
            pkt[2] = 0x11;
            pkt[3] = 0x01; // 0x01 = Custom/SPI Flash wallpaper
            pkt[4] = slot;
        }
        DisplayMode::Animation(index) => {
            pkt[2] = 0x14;
            pkt[3] = 0x00;
            pkt[4] = index;
        }
        DisplayMode::HardwareMonitor => {
            pkt[2] = 0x21;
        }
        DisplayMode::FrameStream => {
            pkt[2] = 0x20;
        }
    }
    pkt
}

/// Packet for setting backlight brightness (0..=100%) and standby wallpaper state (Cmd 0x5c)
///
/// `standby_wallpaper`: If true (0x14), keeps Default Wallpaper visible on standby power
/// when system enters sleep, hibernate, or soft-off states. If false (0x00), turns off display.
pub fn packet_set_brightness(level: u8, standby_wallpaper: bool) -> [u8; PACKET_LEN] {
    let mut pkt = new_packet();
    let val = level.min(100);
    pkt[1] = 0x5c;
    pkt[2] = 0x01;
    pkt[5] = val;
    pkt[9] = val;
    pkt[10] = 0x14; // Backlight enable flag
    pkt[15] = val;
    pkt[16] = if standby_wallpaper { 0x14 } else { 0x00 };
    pkt
}

/// Sync / state query packet (Cmd 0xdc)
pub fn packet_sync() -> [u8; PACKET_LEN] {
    let mut pkt = new_packet();
    pkt[1] = 0xdc;
    pkt
}

/// Configure hardware monitor layout and theme (Cmd 0x52)
///
/// layout: Single (0), Dual (1), Triple (2), Multi (4)
/// theme: 1, 2, 3 (visual background styles)
pub fn packet_hw_layout(layout: HwLayout, theme: u8) -> [u8; PACKET_LEN] {
    let mut pkt = new_packet();
    pkt[1] = 0x52;
    pkt[2] = 0x00;
    pkt[3] = layout as u8;
    pkt[4] = 0x00;
    pkt[5] = theme;
    pkt
}

/// Update a text slot in hardware monitor mode (Cmd 0x53)
///
/// Offset 3..20: Label (e.g. "CPU", "GPU", "Processor", "RAM", "FAN #1")
/// Offset 21..: Value (e.g. "45.0\u{2103}", "15%", "1250 RPM")
pub fn packet_telemetry(slot: u8, label: &str, value: &str) -> [u8; PACKET_LEN] {
    let mut pkt = new_packet();
    pkt[1] = 0x53;
    pkt[2] = slot;

    // Label: bytes 3..20 (max 17 bytes null-terminated)
    let label_bytes = label.as_bytes();
    let max_label = 17;
    let l_len = label_bytes.len().min(max_label);
    pkt[3..3 + l_len].copy_from_slice(&label_bytes[..l_len]);

    // Value: bytes 21..64 (max 43 bytes null-terminated)
    let val_bytes = value.as_bytes();
    let max_val = 43;
    let v_len = val_bytes.len().min(max_val);
    pkt[21..21 + v_len].copy_from_slice(&val_bytes[..v_len]);

    pkt
}

/// Custom Image Upload Handshake Packets
pub mod upload {
    use super::{new_packet, PACKET_LEN};

    /// Delete existing custom image from flash memory for a specific slot
    pub fn step0_delete_slot(slot: u8) -> ([u8; PACKET_LEN], [u8; PACKET_LEN]) {
        let mut p1 = new_packet();
        p1[1] = 0x72;
        p1[2] = 0x01;
        p1[3] = 0x01;
        p1[4] = slot; // Specific custom slot

        let mut p2 = new_packet();
        p2[1] = 0x73;
        p2[2] = 0x03;
        p2[3] = 0x00;
        (p1, p2)
    }

    pub fn step1_prep() -> [u8; PACKET_LEN] {
        let mut pkt = new_packet();
        pkt[1] = 0x71;
        pkt[2] = 0x01;
        pkt[3] = 0x01;
        pkt
    }

    pub fn step2_sync() -> [u8; PACKET_LEN] {
        let mut pkt = new_packet();
        pkt[1] = 0xf1;
        pkt
    }

    /// Enable flash write mode for a specific custom slot (Cmd 0x72 0x01 0x01 [slot])
    pub fn step3_enable(slot: u8) -> [u8; PACKET_LEN] {
        let mut pkt = new_packet();
        pkt[1] = 0x72;
        pkt[2] = 0x01;
        pkt[3] = 0x01;
        pkt[4] = slot;
        pkt
    }

    pub fn step4_start() -> [u8; PACKET_LEN] {
        let mut pkt = new_packet();
        pkt[1] = 0x73;
        pkt[2] = 0x01;
        pkt
    }

    pub fn step5_size_header(jpeg_size: u32) -> [u8; PACKET_LEN] {
        let mut pkt = new_packet();
        pkt[1] = 0x7f;
        pkt[2] = 0x02;
        let le = jpeg_size.to_le_bytes();
        pkt[3..7].copy_from_slice(&le);
        pkt
    }

    pub fn step6_finalize() -> [u8; PACKET_LEN] {
        let mut pkt = new_packet();
        pkt[1] = 0x73;
        pkt[2] = 0xff;
        pkt
    }

    /// Flash write disable / safe exit (Cmd 0x72 0x01 0x00)
    pub fn step7_exit() -> [u8; PACKET_LEN] {
        let mut pkt = new_packet();
        pkt[1] = 0x72;
        pkt[2] = 0x01;
        pkt[3] = 0x00;
        pkt
    }
}
