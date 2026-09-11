//! Firmware patch definitions and checksum utilities for ASUS ROG X870E LCD.

use thiserror::Error;

/// Known official firmware 0109 size in bytes (8,384,512 bytes = 0x7FF000).
pub const EXPECTED_FW_SIZE: usize = 8_384_512;

/// Known official firmware 0109 expected Sum32 checksum.
pub const STOCK_FW_SUM32: u32 = 0x0383db44;

#[derive(Error, Debug)]
pub enum PatchError {
    #[error("Firmware size mismatch: expected at least {expected} bytes, got {actual}")]
    InvalidSize { expected: usize, actual: usize },
    #[error("Patch '{name}' failed safety assertion: byte mismatch at offset 0x{offset:06x}. Expected {expected:02x?}, found {found:02x?}")]
    SafetyAssertionFailed {
        name: &'static str,
        offset: usize,
        expected: Vec<u8>,
        found: Vec<u8>,
    },
    #[error("Patch offset out of bounds: 0x{offset:06x} + {len} exceeds firmware size {size}")]
    OutOfBounds { offset: usize, len: usize, size: usize },
    #[error("Checksum verification failed: calculated 0x{calculated:08x}, stored 0x{stored:08x}")]
    ChecksumMismatch { calculated: u32, stored: u32 },
}

/// Description of a single binary patch
#[derive(Debug, Clone)]
pub struct Patch {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub offset: usize,
    pub expected: &'static [u8],
    pub replacement: &'static [u8],
}

/// Complete video streaming patch:
/// 1. Vsync shadow reload (0x0000801a): Switches immediate reload to VBLANK reload,
///    eliminating horizontal boundary tearing on frame swap.
/// 2. Zero-heap dynamic bitmap registration (0x0000cf46): Points TouchGFX dynamic bitmap
///    directly to USB staging buffer (0x90E00000) with zero heap allocation, keeping 100%
///    of dynamic bitmap cache free for stock wallpapers and JPEG playback.
/// 3. Zero-copy tick NOP (0x0000cfd8): NOPs 3.68MB CPU memcpy in handleTickEvent to eliminate
///    external RAM bus contention.
pub const STREAMING_PATCHES: [&Patch; 3] = [
    &PATCH_VSYNC_RELOAD,
    &PATCH_ZERO_HEAP_SETUP,
    &PATCH_MEMCPY_NOP,
];

/// Sub-patch 1: Switch the TouchGFX framebuffer swap from an immediate LTDC shadow reload to a vertical-blanking reload.
pub const PATCH_VSYNC_RELOAD: Patch = Patch {
    id: "vsync-reload",
    name: "Vsync framebuffer reload",
    description: "setTFTFrameBuffer shadow reload IMMEDIATE -> VBLANK (01 22 -> 02 22 at 0x801a)",
    offset: 0x0000_801a,
    expected: &[0x01, 0x22],
    replacement: &[0x02, 0x22],
};

/// Sub-patch 2: Registers USB staging buffer directly with zero TouchGFX heap usage.
pub const PATCH_ZERO_HEAP_SETUP: Patch = Patch {
    id: "zero-heap-setup",
    name: "Zero-Heap Dynamic Bitmap Setup",
    description: "Registers TouchGFX dynamic bitmap directly to USB staging buffer (0x90E00000) with zero heap allocation",
    offset: 0x0000_cf46,
    expected: &[
        0x00, 0x23, 0x02, 0x22, 0x4f, 0xf4, 0xa0, 0x61, 0x4f, 0xf4, 0x34, 0x70, 0x04, 0xf1,
        0x6c, 0x05, 0x18, 0xf0, 0x5b, 0xfc, 0xa4, 0xf8, 0x98, 0x00, 0x17, 0xf0, 0x93, 0xfe,
        0x4f, 0xf4, 0x61, 0x12, 0x00, 0x21, 0x3c, 0xf0, 0x9c, 0xfc, 0xb4, 0xf8, 0x98, 0x00,
        0x17, 0xf0, 0x8a, 0xfe, 0xc4, 0xf8, 0x9c, 0x00, 0xf8, 0xf7, 0xcc, 0xfa, 0xd4, 0xf8,
        0x9c, 0x00, 0xf8, 0xf7, 0xd0, 0xfa,
    ],
    replacement: &[
        0x04, 0xf1, 0x6c, 0x05, 0x18, 0xf0, 0xe5, 0xfb, 0x4e, 0xf2, 0xcc, 0x53, 0xc2, 0xf2,
        0x00, 0x43, 0x5a, 0x68, 0x0e, 0x21, 0x01, 0xfb, 0x00, 0x22, 0x0f, 0x30, 0xa4, 0xf8,
        0x98, 0x00, 0x40, 0xf2, 0xd0, 0x21, 0xc0, 0xf2, 0x00, 0x51, 0x91, 0x60, 0x22, 0x21,
        0x11, 0x73, 0x40, 0xf2, 0x00, 0x02, 0xc9, 0xf2, 0xe0, 0x02, 0x19, 0x68, 0x41, 0xf8,
        0x20, 0x20, 0xc4, 0xf8, 0x9c, 0x20,
    ],
};

/// Sub-patch 3: NOPs the 3.68MB memcpy in handleTickEvent to eliminate external RAM bus contention.
pub const PATCH_MEMCPY_NOP: Patch = Patch {
    id: "memcpy-nop",
    name: "Memcpy NOP",
    description: "NOPs the 3.68MB CPU memcpy in handleTickEvent, eliminating external RAM bus contention",
    offset: 0x0000_cfd8,
    expected: &[0xf8, 0xf7, 0xa4, 0xfa],
    replacement: &[0x00, 0xbf, 0x00, 0xbf],
};


/// Calculates the 32-bit addition sum (Sum32) over all little-endian 32-bit words,
/// excluding the trailing 4 bytes.
pub fn calculate_sum32(data: &[u8]) -> Result<u32, PatchError> {
    if data.len() < 8 || data.len() % 4 != 0 {
        return Err(PatchError::InvalidSize {
            expected: 8,
            actual: data.len(),
        });
    }

    let word_count = (data.len() - 4) / 4;
    let mut sum: u32 = 0;
    for i in 0..word_count {
        let chunk = &data[i * 4..(i + 1) * 4];
        let word = u32::from_le_bytes(chunk.try_into().unwrap());
        sum = sum.wrapping_add(word);
    }
    Ok(sum)
}

/// Verifies whether the trailing 4-byte checksum in the firmware matches the calculated Sum32.
pub fn verify_checksum(data: &[u8]) -> Result<(u32, u32), PatchError> {
    let calculated = calculate_sum32(data)?;
    let stored_bytes = &data[data.len() - 4..];
    let stored = u32::from_le_bytes(stored_bytes.try_into().unwrap());
    if calculated == stored {
        Ok((calculated, stored))
    } else {
        Err(PatchError::ChecksumMismatch { calculated, stored })
    }
}

/// Applies a list of patches to a mutable firmware buffer with strict safety checks,
/// then recalculates and updates the Sum32 checksum at `data[data.len() - 4..]`.
pub fn apply_patches(data: &mut [u8], patches: &[&Patch]) -> Result<u32, PatchError> {
    if data.len() < EXPECTED_FW_SIZE {
        return Err(PatchError::InvalidSize {
            expected: EXPECTED_FW_SIZE,
            actual: data.len(),
        });
    }

    // Step 1: Verify all safety assertions before modifying anything
    for patch in patches {
        if patch.expected.len() != patch.replacement.len() {
            return Err(PatchError::InvalidSize {
                expected: patch.expected.len(),
                actual: patch.replacement.len(),
            });
        }
        let exp_end = patch.offset.checked_add(patch.expected.len())
            .ok_or(PatchError::OutOfBounds {
                offset: patch.offset,
                len: patch.expected.len(),
                size: data.len(),
            })?;
        if exp_end > data.len() {
            return Err(PatchError::OutOfBounds {
                offset: patch.offset,
                len: patch.expected.len(),
                size: data.len(),
            });
        }
        let rep_end = patch.offset.checked_add(patch.replacement.len())
            .ok_or(PatchError::OutOfBounds {
                offset: patch.offset,
                len: patch.replacement.len(),
                size: data.len(),
            })?;
        if rep_end > data.len() {
            return Err(PatchError::OutOfBounds {
                offset: patch.offset,
                len: patch.replacement.len(),
                size: data.len(),
            });
        }
        let current_bytes = &data[patch.offset..exp_end];
        if current_bytes != patch.expected {
            return Err(PatchError::SafetyAssertionFailed {
                name: patch.name,
                offset: patch.offset,
                expected: patch.expected.to_vec(),
                found: current_bytes.to_vec(),
            });
        }
    }

    // Step 2: Apply all replacement bytes
    for patch in patches {
        let end = patch.offset.checked_add(patch.replacement.len()).unwrap();
        data[patch.offset..end].copy_from_slice(patch.replacement);
    }

    // Step 3: Recalculate and write the new Sum32 checksum
    let new_sum = calculate_sum32(data)?;
    let len = data.len();
    data[len - 4..].copy_from_slice(&new_sum.to_le_bytes());

    Ok(new_sum)
}

/// Applies the complete video streaming patch to stock firmware and updates Sum32.
pub fn apply_streaming_patch(data: &mut [u8]) -> Result<u32, PatchError> {
    apply_patches(data, &STREAMING_PATCHES)
}

/// Checks if firmware already has the complete video streaming patch applied.
pub fn is_streaming_patched(data: &[u8]) -> bool {
    if data.len() < EXPECTED_FW_SIZE {
        return false;
    }
    STREAMING_PATCHES.iter().all(|patch| {
        let end = patch.offset + patch.replacement.len();
        end <= data.len() && &data[patch.offset..end] == patch.replacement
    })
}

/// Summary of a byte difference between original and patched firmware
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteDiff {
    pub offset: usize,
    pub original: u8,
    pub patched: u8,
}

/// Compares two firmware buffers and returns all changed bytes.
/// Returns an error if the buffers have different lengths.
pub fn diff_firmware(orig: &[u8], patched: &[u8]) -> Result<Vec<ByteDiff>, PatchError> {
    if orig.len() != patched.len() {
        return Err(PatchError::InvalidSize {
            expected: orig.len(),
            actual: patched.len(),
        });
    }
    let mut diffs = Vec::new();
    for i in 0..orig.len() {
        if orig[i] != patched[i] {
            diffs.push(ByteDiff {
                offset: i,
                original: orig[i],
                patched: patched[i],
            });
        }
    }
    Ok(diffs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_sum32_simple() {
        let mut data = vec![0u8; 16];
        // words: [1, 2, 3, checksum]
        data[0..4].copy_from_slice(&1u32.to_le_bytes());
        data[4..8].copy_from_slice(&2u32.to_le_bytes());
        data[8..12].copy_from_slice(&3u32.to_le_bytes());
        let sum = calculate_sum32(&data).unwrap();
        assert_eq!(sum, 6);
    }

    #[test]
    fn test_patch_safety_assertion_failure() {
        let mut data = vec![0u8; EXPECTED_FW_SIZE];
        // Offset has 0x00 instead of expected 0x01
        let result = apply_patches(&mut data, &[&PATCH_VSYNC_RELOAD]);
        assert!(result.is_err());
        match result.unwrap_err() {
            PatchError::SafetyAssertionFailed { name, offset, .. } => {
                assert_eq!(name, PATCH_VSYNC_RELOAD.name);
                assert_eq!(offset, PATCH_VSYNC_RELOAD.offset);
            }
            other => panic!("Unexpected error: {:?}", other),
        }
    }

    #[test]
    fn test_apply_streaming_patch() {
        let mut data = vec![0u8; EXPECTED_FW_SIZE];
        for patch in STREAMING_PATCHES {
            data[patch.offset..patch.offset + patch.expected.len()]
                .copy_from_slice(patch.expected);
        }

        assert!(!is_streaming_patched(&data));

        let sum = apply_streaming_patch(&mut data).unwrap();
        for patch in STREAMING_PATCHES {
            assert_eq!(
                &data[patch.offset..patch.offset + patch.replacement.len()],
                patch.replacement
            );
        }
        let (calc, stored) = verify_checksum(&data).unwrap();
        assert_eq!(calc, sum);
        assert_eq!(stored, sum);
        assert!(is_streaming_patched(&data));
    }

    #[test]
    fn test_diff_firmware() {
        let a = vec![1, 2, 3, 4];
        let b = vec![1, 99, 3, 4];
        let diffs = diff_firmware(&a, &b).unwrap();
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].offset, 1);
        assert_eq!(diffs[0].original, 2);
        assert_eq!(diffs[0].patched, 99);

        // Identical
        let diffs_same = diff_firmware(&a, &a).unwrap();
        assert!(diffs_same.is_empty());

        // Length mismatch
        let c = vec![1, 2, 3];
        let err = diff_firmware(&a, &c);
        assert!(err.is_err());
        match err.unwrap_err() {
            PatchError::InvalidSize { expected, actual } => {
                assert_eq!(expected, 4);
                assert_eq!(actual, 3);
            }
            other => panic!("Unexpected error: {:?}", other),
        }
    }
}

