//! Library exports for ASUS ROG X870E LCD Firmware Patcher.

pub mod patches;

pub use patches::{
    apply_patches, apply_streaming_patch, calculate_sum32, diff_firmware, is_streaming_patched,
    verify_checksum, ByteDiff, Patch, PatchError, EXPECTED_FW_SIZE, STOCK_FW_SUM32,
    STREAMING_PATCHES,
};
