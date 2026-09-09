//! Persistent storage catalog for custom LCD flash slots and thumbnails.
//!
//! Because the LCD panel's hardware controller does not provide a filesystem query interface
//! over USB, metadata and thumbnails for custom flashed slots are maintained locally on the host.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use image::DynamicImage;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info};

/// Metadata entry for a custom image flashed to an SPI flash slot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SlotEntry {
    /// Zero-based slot index on the SPI flash memory (0..=255)
    pub slot: u8,
    /// Display name or label for the image
    pub title: String,
    /// Original source filename (e.g. "cyberpunk_logo.png")
    pub original_filename: String,
    /// Epoch timestamp when the image was flashed
    pub timestamp: u64,
    /// JPEG file size in bytes
    pub file_size_bytes: usize,
    /// Relative filename of the cached preview thumbnail (e.g. "slot_0.png")
    pub thumbnail_filename: String,
}

/// Catalog holding all tracked custom flash slots.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SlotCatalog {
    pub slots: BTreeMap<u8, SlotEntry>,
}

/// Returns the configuration directory for the application (`~/.config/x870e-lcd`).
pub fn config_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.trim().is_empty() {
            return PathBuf::from(xdg).join("x870e-lcd");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.trim().is_empty() {
            return PathBuf::from(home).join(".config").join("x870e-lcd");
        }
    }
    std::env::temp_dir().join("x870e-lcd")
}

/// Returns the directory where thumbnail images are cached.
pub fn thumbnails_dir() -> PathBuf {
    config_dir().join("thumbnails")
}

/// Returns the path to the catalog JSON file.
pub fn catalog_file_path() -> PathBuf {
    config_dir().join("catalog.json")
}

impl SlotEntry {
    /// Constructs a new `SlotEntry` from slot index, source filename, and file size in bytes.
    pub fn new(slot: u8, orig_filename: &str, file_size_bytes: usize) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let title = std::path::Path::new(orig_filename)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| format!("Custom {slot}"));

        Self {
            slot,
            title,
            original_filename: orig_filename.to_string(),
            timestamp: now,
            file_size_bytes,
            thumbnail_filename: format!("slot_{slot}.png"),
        }
    }
}

impl SlotCatalog {
    /// Loads the slot catalog from disk, or returns an empty catalog if none exists.
    pub fn load() -> Self {
        let mut catalog = Self::default();
        catalog.reload();
        catalog
    }

    /// Reloads the catalog from disk into self.
    pub fn reload(&mut self) {
        let path = catalog_file_path();
        if !path.is_file() {
            return;
        }

        let json = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(e) => {
                error!("Failed to read catalog file {:?}: {}", path, e);
                return;
            }
        };

        match serde_json::from_str::<SlotCatalog>(&json) {
            Ok(loaded) => {
                debug!("Loaded slot catalog with {} slots from {:?}", loaded.slots.len(), path);
                self.slots = loaded.slots;
            }
            Err(e) => {
                error!("Failed to parse catalog JSON from {:?}: {}", path, e);
                let backup_path = path.with_extension("json.bak");
                if let Err(err) = fs::rename(&path, &backup_path) {
                    error!("Failed to backup corrupt catalog file to {:?}: {}", backup_path, err);
                } else {
                    info!("Preserved corrupt catalog file as {:?}", backup_path);
                }
            }
        }
    }

    /// Atomically saves the slot catalog to disk.
    pub fn save(&self) -> std::io::Result<()> {
        let dir = config_dir();
        fs::create_dir_all(&dir)?;

        let path = catalog_file_path();
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        let temp_path = dir.join("catalog.json.tmp");
        fs::write(&temp_path, json)?;
        fs::rename(temp_path, path)?;
        debug!("Saved slot catalog ({} slots)", self.slots.len());
        Ok(())
    }

    /// Finds the lowest available free slot index within `0..max_slots` (e.g. 256 for slots 0..=255).
    pub fn next_free_slot(&self, max_slots: u16) -> Option<u8> {
        let limit = max_slots.min(256);
        for s in 0..limit {
            let slot_u8 = s as u8;
            if !self.slots.contains_key(&slot_u8) {
                return Some(slot_u8);
            }
        }
        None
    }

    /// Returns whether the given slot is currently occupied in the catalog.
    pub fn is_occupied(&self, slot: u8) -> bool {
        self.slots.contains_key(&slot)
    }

    /// Returns the number of occupied slots.
    pub fn occupied_count(&self) -> usize {
        self.slots.len()
    }

    /// Returns the absolute path to a slot's thumbnail image.
    pub fn thumbnail_path(&self, slot: u8) -> PathBuf {
        if let Some(entry) = self.slots.get(&slot) {
            thumbnails_dir().join(&entry.thumbnail_filename)
        } else {
            thumbnails_dir().join(format!("slot_{slot}.png"))
        }
    }

    /// Adds or updates a custom slot entry, generating a thumbnail and saving the catalog.
    pub fn add_slot(
        &mut self,
        entry: SlotEntry,
        preview_img: &DynamicImage,
    ) -> std::io::Result<()> {
        let thumbs_dir = thumbnails_dir();
        fs::create_dir_all(&thumbs_dir)?;

        let thumb_path = thumbs_dir.join(&entry.thumbnail_filename);

        // Resize image to small thumbnail maintaining 720:1280 ratio (e.g. 72x128)
        let thumbnail = preview_img.resize_exact(72, 128, image::imageops::FilterType::Triangle);
        thumbnail
            .save(&thumb_path)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

        info!("Registering Slot {} in catalog: {}", entry.slot, entry.title);
        self.slots.insert(entry.slot, entry);
        self.save()
    }

    /// Removes a slot from the catalog and deletes its cached thumbnail.
    pub fn remove_slot(&mut self, slot: u8) -> std::io::Result<Option<SlotEntry>> {
        let thumb_path = self.thumbnail_path(slot);
        let removed = self.slots.remove(&slot);
        if removed.is_some() {
            if thumb_path.exists() {
                let _ = fs::remove_file(thumb_path);
            }
            info!("Removed Slot {} from catalog", slot);
            self.save()?;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_next_free_slot() {
        let mut catalog = SlotCatalog::default();
        assert_eq!(catalog.next_free_slot(64), Some(0));

        catalog.slots.insert(
            0,
            SlotEntry {
                slot: 0,
                title: "Test 0".into(),
                original_filename: "test0.png".into(),
                timestamp: 1000,
                file_size_bytes: 5000,
                thumbnail_filename: "slot_0.png".into(),
            },
        );
        assert_eq!(catalog.next_free_slot(64), Some(1));

        catalog.slots.insert(
            1,
            SlotEntry {
                slot: 1,
                title: "Test 1".into(),
                original_filename: "test1.png".into(),
                timestamp: 1001,
                file_size_bytes: 5000,
                thumbnail_filename: "slot_1.png".into(),
            },
        );
        assert_eq!(catalog.next_free_slot(64), Some(2));

        // When slot 0 is removed, slot 0 should be reallocated
        catalog.slots.remove(&0);
        assert_eq!(catalog.next_free_slot(64), Some(0));
    }

    #[test]
    fn test_catalog_serialization() {
        let mut catalog = SlotCatalog::default();
        catalog.slots.insert(
            3,
            SlotEntry {
                slot: 3,
                title: "ROG Neon".into(),
                original_filename: "neon.jpg".into(),
                timestamp: 1234567,
                file_size_bytes: 145000,
                thumbnail_filename: "slot_3.png".into(),
            },
        );

        let json = serde_json::to_string(&catalog).expect("serialize");
        let decoded: SlotCatalog = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.slots.len(), 1);
        let entry = decoded.slots.get(&3).unwrap();
        assert_eq!(entry.slot, 3);
        assert_eq!(entry.title, "ROG Neon");
        assert_eq!(entry.original_filename, "neon.jpg");
        assert_eq!(entry.file_size_bytes, 145000);
    }

    #[test]
    fn test_slot_entry_new_and_thumbnail_path() {
        let entry = SlotEntry::new(5, "my_wallpaper.png", 42000);
        assert_eq!(entry.slot, 5);
        assert_eq!(entry.title, "my_wallpaper");
        assert_eq!(entry.thumbnail_filename, "slot_5.png");
        assert_eq!(entry.file_size_bytes, 42000);

        let mut catalog = SlotCatalog::default();
        catalog.slots.insert(entry.slot, entry);

        let thumb = catalog.thumbnail_path(5);
        assert!(thumb.ends_with("slot_5.png"));
    }
}
