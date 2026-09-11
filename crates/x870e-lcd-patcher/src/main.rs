//! CLI utility for inspecting, validating, and patching ASUS ROG X870E LCD Panel firmware.

use std::fs;
use std::path::{Path, PathBuf};
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use x870e_lcd_patcher::{
    apply_streaming_patch, calculate_sum32, diff_firmware, is_streaming_patched, verify_checksum,
    EXPECTED_FW_SIZE, STOCK_FW_SUM32, STREAMING_PATCHES,
};

#[derive(Parser)]
#[command(name = "x870e-lcd-patcher")]
#[command(about = "Firmware inspection and patching tool for ASUS ROG Crosshair X870E LCD Panel")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Inspect a firmware binary (checksum, size, and patch status)
    Info {
        /// Path to the firmware .bin file
        firmware: PathBuf,
    },

    /// Apply the Video Streaming Patch to stock firmware 0109
    Patch {
        /// Input original firmware .bin file (e.g. ALDR4-S7R7-0109.bin)
        input: PathBuf,

        /// Output path for the patched firmware binary
        #[arg(short, long)]
        output: PathBuf,
    },

    /// Compare two firmware binaries and show byte differences
    Diff {
        /// Original firmware binary
        original: PathBuf,

        /// Patched or modified firmware binary
        modified: PathBuf,
    },
}

/// Main entry point for the x870e-lcd-patcher CLI utility.
fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match cli.command {
        Commands::Info { firmware } => handle_info(&firmware),
        Commands::Patch { input, output } => handle_patch(&input, &output),
        Commands::Diff { original, modified } => handle_diff(&original, &modified),
    }
}

/// Displays detailed information, checksum, and patch status for a firmware binary.
fn handle_info(path: &Path) -> Result<()> {
    let data = fs::read(path).with_context(|| format!("Failed to read {}", path.display()))?;
    println!("=== Firmware Binary Info: {} ===", path.display());
    println!("File Size: {} bytes (0x{:06x})", data.len(), data.len());

    let (calc, _stored) = match verify_checksum(&data) {
        Ok((calc, stored)) => {
            println!("Sum32 Checksum: 0x{:08x} (VALID ✓)", calc);
            (calc, stored)
        }
        Err(e) => {
            let calc = calculate_sum32(&data).unwrap_or(0);
            let stored = if data.len() >= 4 {
                u32::from_le_bytes(data[data.len() - 4..].try_into().unwrap())
            } else {
                0
            };
            println!("Sum32 Checksum: calculated 0x{:08x}, stored 0x{:08x} (INVALID ✗: {})", calc, stored, e);
            (calc, stored)
        }
    };

    if calc == STOCK_FW_SUM32 {
        println!("Match: Official stock ASUS FW 0109");
    }

    let patched = is_streaming_patched(&data);
    println!("\nVideo Streaming Patch Status:");
    if patched {
        println!("  [✓] Video Streaming Patch: Applied (Tear-Free Live Video Streaming Ready)");
    } else {
        println!("  [ ] Video Streaming Patch: Stock / Unpatched");
    }

    Ok(())
}

/// Applies the video streaming patch to an official stock firmware image.
fn handle_patch(input: &Path, output: &Path) -> Result<()> {
    let mut data = fs::read(input).with_context(|| format!("Failed to read {}", input.display()))?;
    if data.len() < EXPECTED_FW_SIZE {
        bail!(
            "Input file size ({} bytes) is smaller than expected 0109 firmware ({} bytes)",
            data.len(),
            EXPECTED_FW_SIZE
        );
    }
    if data.len() != EXPECTED_FW_SIZE {
        bail!(
            "Input file size ({} bytes) does not match expected 0109 firmware ({} bytes)",
            data.len(),
            EXPECTED_FW_SIZE
        );
    }

    let (calc, _stored) = verify_checksum(&data)
        .context("Input firmware failed checksum verification")?;

    if calc != STOCK_FW_SUM32 && !is_streaming_patched(&data) {
        bail!(
            "Input firmware checksum (0x{:08x}) does not match stock 0109 firmware (0x{:08x})",
            calc,
            STOCK_FW_SUM32
        );
    }

    if is_streaming_patched(&data) {
        println!("Firmware already has the video streaming patch applied.");
        fs::write(output, &data).with_context(|| format!("Failed to write {}", output.display()))?;
        return Ok(());
    }

    println!("Applying Video Streaming Patch to {}...", input.display());
    for patch in STREAMING_PATCHES {
        println!("  -> Applying {}: {}", patch.name, patch.description);
    }

    let new_checksum = apply_streaming_patch(&mut data)
        .context("Failed to apply patch. Ensure input is official stock ASUS FW 0109 (ALDR4-S7R7-0109.bin).")?;

    fs::write(output, &data).with_context(|| format!("Failed to write {}", output.display()))?;

    println!("\n✓ Video Streaming Patch successfully applied!");
    println!("Output File: {}", output.display());
    println!("New Sum32 Checksum: 0x{:08x}", new_checksum);
    println!("Verification: Sum32 is valid for AIOFanFWUpdate.exe and x870e-lcd-flash");

    if let Some(file_name) = output.file_name().and_then(|s| s.to_str()) {
        if !file_name.ends_with("-0109.bin") && file_name.ends_with(".bin") {
            println!("\nNOTE: ASUS's AIOFanFWUpdate.exe parses the expected version string from the 4 characters following the last '-' in the filename.");
            println!("      Because the filename does not end in '-0109.bin', AIOFanFWUpdate.exe would display 'Return Code = 99' (version mismatch) at 99%.");
            println!("      Recommended filename: 'ALDR4-S7R7-0109.bin' or 'ALDR4-S7R7-patched-0109.bin'.");
        }
    }

    Ok(())
}

/// Compares two firmware binaries byte-by-byte and displays diff groups.
fn handle_diff(orig_path: &Path, mod_path: &Path) -> Result<()> {
    let orig = fs::read(orig_path).with_context(|| format!("Failed to read {}", orig_path.display()))?;
    let modified = fs::read(mod_path).with_context(|| format!("Failed to read {}", mod_path.display()))?;

    println!("=== Comparing {} vs {} ===", orig_path.display(), mod_path.display());
    println!("Original size: {} bytes, Modified size: {} bytes", orig.len(), modified.len());

    let diffs = diff_firmware(&orig, &modified)?;
    if diffs.is_empty() {
        println!("Files are identical. No differences found.");
        return Ok(());
    }

    println!("Found {} byte differences:", diffs.len());
    // Group contiguous diffs
    let mut i = 0;
    while i < diffs.len() {
        let start = diffs[i].offset;
        let mut orig_chunk = vec![diffs[i].original];
        let mut mod_chunk = vec![diffs[i].patched];
        let mut j = i + 1;
        while j < diffs.len() && diffs[j].offset == diffs[j - 1].offset + 1 {
            orig_chunk.push(diffs[j].original);
            mod_chunk.push(diffs[j].patched);
            j += 1;
        }

        print!("  Offset 0x{:06x} (len {}): ", start, orig_chunk.len());
        for b in &orig_chunk {
            print!("{:02x} ", b);
        }
        print!("-> ");
        for b in &mod_chunk {
            print!("{:02x} ", b);
        }
        if orig.len() >= 4 && start == orig.len() - 4 {
            print!(" (Sum32 Checksum)");
        } else if let Some(p) = STREAMING_PATCHES.iter().find(|p| p.offset == start) {
            print!(" ({}: {})", p.name, p.description);
        }
        println!();
        i = j;
    }

    Ok(())
}
