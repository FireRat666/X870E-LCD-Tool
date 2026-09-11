//! Linux firmware flasher for the ASUS ROG Crosshair X870E LCD panel.
//!
//! Reimplements the exact flashing protocol used by ASUS `AIOFanFWUpdate.exe`,
//! reverse-engineered from a USBPcap capture of a full stock-restore session
//! (see `findings_tearing.md` section 10):
//!
//!   0. `ec 10 aa 41 53 55 53 aa`          -> ACK `ec 10 00` (ENTER BOOTLOADER;
//!                                            device re-enumerates as 0x1c82
//!                                            "Motherboard LCD Panel Update Mode")
//!   1. `ec 8c` info probe                 -> ACK `ec 0c 00` + SPI NOR info
//!   2. `ec 20 02 01 01 <addr> <size>`     -> ACK `ec 20 00` (erase/prepare)
//!   3. `ec 7f 01 <size>`                  -> ACK `ec 7f 00 <chunk_le32>`
//!   4. N x 4096 B bulk EP 0x02 writes     -> per-packet ACK `ee 01`
//!   5. `ec 2f "DevRst"`                   -> ACK `ec 2f 00` (verify + reboot)
//!
//! The bootloader verifies the image (including the trailing Sum32) before
//! accepting DevRst; this tool recomputes the trailing Sum32 automatically
//! unless `--no-fix-checksum` is given.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::Parser;
use hidapi::{HidApi, HidDevice};
use rusb::{DeviceHandle, GlobalContext};
use tracing::debug;
use x870e_lcd_patcher::{calculate_sum32, EXPECTED_FW_SIZE};

/// App firmware VID:PID ("Motherboard LCD Panel")
const ASUS_VENDOR_ID: u16 = 0x0b05;
const LCD_PRODUCT_ID: u16 = 0x1c83;
/// Bootloader VID:PID ("Motherboard LCD Panel Update Mode") after `ec 10` magic
const BOOT_VENDOR_ID: u16 = 0x0b05;
const BOOT_PRODUCT_ID: u16 = 0x1c82;

const PACKET_LEN: usize = 65;
const REPORT_ID: u8 = 0xec;
const BULK_EP_OUT: u8 = 0x02;
/// Destination address of the firmware region in SPI NOR (bootloader occupies 0..1 MB).
const DEFAULT_FLASH_ADDR: u32 = 0x0010_0000;
const BULK_CHUNK_SIZE: usize = 4096;
const BULK_ACK_TIMEOUT: Duration = Duration::from_secs(30);

/// Validates that the flash address is at or above `DEFAULT_FLASH_ADDR`.
fn validate_flash_addr(s: &str) -> Result<u32, String> {
    let addr = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).map_err(|e| format!("Invalid hex address: {e}"))?
    } else {
        s.parse::<u32>().map_err(|e| format!("Invalid address: {e}"))?
    };
    if addr < DEFAULT_FLASH_ADDR {
        return Err(format!(
            "Address 0x{addr:08x} is below minimum allowed address 0x{DEFAULT_FLASH_ADDR:08x} (bootloader region is protected)"
        ));
    }
    Ok(addr)
}

#[derive(Parser)]
#[command(name = "x870e-lcd-flash")]
#[command(about = "Linux firmware flasher for the ASUS ROG Crosshair X870E LCD panel")]
#[command(version)]
struct Cli {
    /// Firmware image to flash (stock or patched, e.g. ALDR4-S7R7-0109.bin)
    firmware: PathBuf,

    /// Validate the image (size, alignment, Sum32) without touching the device
    #[arg(long)]
    dry_run: bool,

    /// Do not rewrite an invalid trailing Sum32 (exact-stock images)
    #[arg(long)]
    no_fix_checksum: bool,

    /// Skip the bootloader-entry magic (device already in update mode, PID 0x1c82)
    #[arg(long)]
    no_reboot: bool,

    /// SPI NOR destination address
    #[arg(long, default_value_t = DEFAULT_FLASH_ADDR, value_parser = validate_flash_addr)]
    addr: u32,
}

struct Flasher {
    hid: HidDevice,
    usb: DeviceHandle<GlobalContext>,
}

/// Opens the HID control channel (interface 1) and the bulk channel (interface 0),
/// mirroring `x870e-lcd-core`'s device-open conventions. Matches either PID.
fn open_device() -> Result<(HidDevice, DeviceHandle<GlobalContext>)> {
    let hid_api = HidApi::new()?;

    let mut target = None;
    for dev in hid_api.device_list() {
        if dev.vendor_id() == ASUS_VENDOR_ID
            && (dev.product_id() == LCD_PRODUCT_ID || dev.product_id() == BOOT_PRODUCT_ID)
        {
            if dev.interface_number() == 1 || dev.interface_number() == -1 {
                target = Some(dev);
                break;
            }
        }
    }
    let hid = match target {
        Some(info) => info.open_device(&hid_api)?,
        None => hid_api.open(ASUS_VENDOR_ID, LCD_PRODUCT_ID)?,
    };

    let mut usb_handle = None;
    for dev in rusb::devices()?.iter() {
        if let Ok(desc) = dev.device_descriptor() {
            if desc.vendor_id() == ASUS_VENDOR_ID
                && (desc.product_id() == LCD_PRODUCT_ID || desc.product_id() == BOOT_PRODUCT_ID)
            {
                let handle = dev.open()?;
                let _ = handle.set_auto_detach_kernel_driver(true);
                handle.claim_interface(0)?;
                usb_handle = Some(handle);
                break;
            }
        }
    }
    let usb = usb_handle.ok_or_else(|| {
        anyhow::anyhow!(
            "device not found (VID 0x{ASUS_VENDOR_ID:04x}, PID 0x{LCD_PRODUCT_ID:04x}/0x{BOOT_PRODUCT_ID:04x})"
        )
    })?;

    Ok((hid, usb))
}

impl Flasher {
    /// Opens the flasher by establishing both HID and bulk USB connections to the device.
    fn open() -> Result<Self> {
        let (hid, usb) = open_device()?;
        Ok(Self { hid, usb })
    }

    /// Discards any pending unread HID input reports.
    fn drain(&self) {
        let mut buf = [0u8; PACKET_LEN];
        while self.hid.read_timeout(&mut buf, 20).unwrap_or(0) > 0 {}
    }

    /// Sends an HID command and waits for an ACK with the given prefix.
    fn hid_command(
        &self,
        packet: &[u8; PACKET_LEN],
        ack: &[u8],
        timeout: Duration,
    ) -> Result<[u8; PACKET_LEN]> {
        self.drain();
        debug!("hid cmd 0x{:02x}: {:02x?}", packet[1], &packet[..8]);
        self.hid.write(packet)?;

        let mut buf = [0u8; PACKET_LEN];
        let start = Instant::now();
        while start.elapsed() < timeout {
            let n = self.hid.read_timeout(&mut buf, 100)?;
            if n >= ack.len() && &buf[..ack.len()] == ack {
                return Ok(buf);
            }
            if n > 0 {
                debug!(
                    "unexpected report while waiting for 0x{:02x}: {:02x?}",
                    packet[1],
                    &buf[..n.min(8)]
                );
            }
        }
        bail!(
            "timed out waiting for ACK {:02x?} (command 0x{:02x})",
            ack,
            packet[1]
        );
    }

    /// Waits for the per-packet bulk write ACK (`ee 01`).
    fn wait_bulk_ack(&self, index: usize) -> Result<()> {
        let mut buf = [0u8; PACKET_LEN];
        let start = Instant::now();
        while start.elapsed() < BULK_ACK_TIMEOUT {
            let n = self.hid.read_timeout(&mut buf, 1000)?;
            if n >= 2 && buf[0] == 0xee && buf[1] == 0x01 {
                return Ok(());
            }
            if n > 0 {
                bail!(
                    "packet {index}: unexpected report {:02x?} (expected ee 01)",
                    &buf[..n.min(8)]
                );
            }
        }
        bail!("packet {index}: no ee 01 ACK within {:?}", BULK_ACK_TIMEOUT);
    }
}

/// Builds the magic bootloader-entry command packet (ec 10 aa "ASUS" aa).
fn rep_enter_bootloader() -> [u8; PACKET_LEN] {
    let mut p = [0u8; PACKET_LEN];
    p[0] = REPORT_ID;
    p[1] = 0x10;
    p[2..8].copy_from_slice(&[0xaa, 0x41, 0x53, 0x55, 0x53, 0xaa]); // aa "ASUS" aa
    p
}

/// Builds the flash info probe packet (ec 8c).
fn rep_info() -> [u8; PACKET_LEN] {
    let mut p = [0u8; PACKET_LEN];
    p[0] = REPORT_ID;
    p[1] = 0x8c;
    p
}

/// Builds the flash erase / preparation command packet (ec 20 02 01 01 <addr> <size>).
fn rep_erase(addr: u32, size: u32) -> [u8; PACKET_LEN] {
    let mut p = [0u8; PACKET_LEN];
    p[0] = REPORT_ID;
    p[1] = 0x20;
    p[2] = 0x02; // action: erase/prepare (from capture)
    p[3] = 0x01;
    p[4] = 0x01;
    p[5..9].copy_from_slice(&addr.to_le_bytes());
    // p[9] = 0x00 separator (from capture)
    p[10..14].copy_from_slice(&size.to_le_bytes());
    p
}

/// Builds the bulk transfer size announcement packet (ec 7f 01 <size>).
fn rep_announce(size: u32) -> [u8; PACKET_LEN] {
    let mut p = [0u8; PACKET_LEN];
    p[0] = REPORT_ID;
    p[1] = 0x7f;
    p[2] = 0x01; // subcommand: flash announce (0x03 is frame streaming, 0x02 is custom image upload)
    p[3..7].copy_from_slice(&size.to_le_bytes());
    p
}

/// Builds the device reset / reboot packet (ec 2f "DevRst").
fn rep_devrst() -> [u8; PACKET_LEN] {
    let mut p = [0u8; PACKET_LEN];
    p[0] = REPORT_ID;
    p[1] = 0x2f;
    p[2..8].copy_from_slice(b"DevRst");
    p
}

/// Validates the image and (unless disabled) fixes the trailing Sum32 in place.
fn prepare_image(path: &PathBuf, no_fix_checksum: bool) -> Result<Vec<u8>> {
    let mut image = fs::read(path).with_context(|| format!("Failed to read {}", path.display()))?;
    println!(
        "Image: {}  ({} bytes = 0x{:x})",
        path.display(),
        image.len(),
        image.len()
    );

    if image.len() < 8 || image.len() % 4 != 0 {
        bail!("Image size {} is not a multiple of 4 (min 8 bytes)", image.len());
    }

    if image.len() != EXPECTED_FW_SIZE || image.len() % BULK_CHUNK_SIZE != 0 {
        bail!(
            "Invalid firmware size: {} bytes. Expected exactly {} bytes (4096-byte aligned)",
            image.len(),
            EXPECTED_FW_SIZE
        );
    }

    let calc = calculate_sum32(&image)?;
    let stored = u32::from_le_bytes(image[image.len() - 4..].try_into().unwrap());
    if calc == stored {
        println!("  Sum32 valid: 0x{calc:08x}");
    } else if no_fix_checksum {
        println!(
            "  WARNING: Sum32 INVALID (calc 0x{calc:08x}, stored 0x{stored:08x}); bootloader may reject DevRst."
        );
    } else {
        let last = image.len() - 4;
        image[last..].copy_from_slice(&calc.to_le_bytes());
        println!("  Sum32 fixed: 0x{stored:08x} -> 0x{calc:08x}");
    }
    Ok(image)
}

/// Sends the bootloader-entry magic and waits for the device to re-enumerate
/// as the Update Mode device (PID 0x1c82).
fn enter_bootloader() -> Result<()> {
    let hid_api = HidApi::new()?;
    let hid = hid_api.open(ASUS_VENDOR_ID, LCD_PRODUCT_ID)?;

    println!("[0/5] Entering bootloader (ec 10 aa 41 53 55 53 aa)...");
    hid.write(&rep_enter_bootloader())?;

    // Wait for ACK ec 10 00 (best effort — the device may drop the link mid-ACK)
    let mut buf = [0u8; PACKET_LEN];
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        let n = hid.read_timeout(&mut buf, 100)?;
        if n >= 3 && buf[0] == REPORT_ID && buf[1] == 0x10 {
            debug!("bootloader-entry ACK: {:02x?}", &buf[..n.min(8)]);
            break;
        }
    }
    drop(hid);

    // Wait for re-enumeration: PID changes 0x1c83 -> 0x1c82
    let start = Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(10) {
            bail!("device did not re-enumerate as Update Mode (0x1c82) within 10s");
        }
        std::thread::sleep(Duration::from_millis(100));
        let found = rusb::devices()?
            .iter()
            .filter_map(|d| d.device_descriptor().ok())
            .any(|desc| desc.vendor_id() == BOOT_VENDOR_ID && desc.product_id() == BOOT_PRODUCT_ID);
        if found {
            break;
        }
    }
    std::thread::sleep(Duration::from_millis(300)); // let the kernel finish binding
    println!("      panel is in Update Mode (0x{:04x})", BOOT_PRODUCT_ID);
    Ok(())
}

/// Executes the complete firmware flashing pipeline based on CLI options.
fn run(cli: &Cli) -> Result<()> {
    if cli.addr < DEFAULT_FLASH_ADDR {
        bail!(
            "Invalid flash destination address: 0x{:08x}. Addresses below 0x{:08x} are protected bootloader regions",
            cli.addr,
            DEFAULT_FLASH_ADDR
        );
    }
    let image = prepare_image(&cli.firmware, cli.no_fix_checksum)?;

    if image.len() % BULK_CHUNK_SIZE != 0 {
        println!(
            "  NOTE: image is not {}-aligned ({} % {} = {})",
            BULK_CHUNK_SIZE,
            image.len(),
            BULK_CHUNK_SIZE,
            image.len() % BULK_CHUNK_SIZE
        );
    }
    let packets = image.len().div_ceil(BULK_CHUNK_SIZE);

    if cli.dry_run {
        println!(
            "Dry run OK. Would flash to 0x{:x} in {} x {}B bulk packets.",
            cli.addr, packets, BULK_CHUNK_SIZE
        );
        return Ok(());
    }

    if !cli.no_reboot {
        enter_bootloader()?;
        std::thread::sleep(Duration::from_millis(200));
    }

    let flasher = Flasher::open()?;

    // 1. Info probe
    println!("[1/5] Info probe (ec 8c)...");
    let ack = flasher.hid_command(&rep_info(), &[REPORT_ID, 0x0c, 0x00], Duration::from_secs(5))?;
    println!("      flash info: {:02x?}", &ack[2..18]);

    // 2. Erase / prepare
    println!(
        "[2/5] Erase/prepare (ec 20): addr=0x{:08x} size=0x{:x}",
        cli.addr,
        image.len()
    );
    let t0 = Instant::now();
    flasher.hid_command(
        &rep_erase(cli.addr, image.len() as u32),
        &[REPORT_ID, 0x20, 0x00],
        Duration::from_secs(10),
    )?;
    println!("      accepted in {:.2}s", t0.elapsed().as_secs_f32());

    // 3. Announce size
    println!("[3/5] Announce (ec 7f 01): size=0x{:x}", image.len());
    let ack = flasher.hid_command(
        &rep_announce(image.len() as u32),
        &[REPORT_ID, 0x7f, 0x00],
        Duration::from_secs(10),
    )?;
    let chunk = u32::from_le_bytes(ack[3..7].try_into().unwrap()) as usize;
    println!("      device accepted, chunk size = {chunk} bytes");
    if chunk != BULK_CHUNK_SIZE {
        bail!("device reported unexpected chunk size {chunk} (expected {BULK_CHUNK_SIZE})");
    }

    // 4. Bulk data
    println!("[4/5] Flashing {packets} x {chunk}B packets via bulk EP 0x02...");
    let t0 = Instant::now();
    for i in 0..packets {
        let data = &image[i * chunk..(i + 1) * chunk];
        let transferred = flasher
            .usb
            .write_bulk(BULK_EP_OUT, data, Duration::from_secs(10))
            .context(format!("bulk write {i} failed"))?;
        if transferred != chunk {
            bail!("bulk write {i}: transferred {transferred}/{chunk} bytes");
        }
        flasher.wait_bulk_ack(i)?;
        if i % 64 == 0 || i == packets - 1 {
            let el = t0.elapsed().as_secs_f32();
            let pct = 100.0 * (i + 1) as f32 / packets as f32;
            let eta = el / (i + 1) as f32 * (packets - i - 1) as f32;
            println!(
                "      {}/{packets} ({pct:5.1}%)  {el:6.1}s elapsed  ETA {eta:5.1}s",
                i + 1
            );
        }
    }
    println!("      done in {:.1}s", t0.elapsed().as_secs_f32());

    // 5. Reboot into new firmware
    println!("[5/5] DevRst (ec 2f)...");
    flasher.hid_command(&rep_devrst(), &[REPORT_ID, 0x2f, 0x00], Duration::from_secs(5))?;
    println!("\nDONE. Panel is rebooting into the new firmware.");
    println!("The bootloader verified the image before accepting DevRst.");
    Ok(())
}

/// Main entry point for the x870e-lcd-flash CLI utility.
fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    run(&cli)
}
