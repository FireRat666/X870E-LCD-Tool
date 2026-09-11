//! FFmpeg-backed video decoder and stream feeder for ASUS ROG X870E LCD.

use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use anyhow::{Context, Result};
use tracing::{error, info};
use x870e_lcd_core::FRAME_RAW_SIZE;
use crate::media::{MediaConfig, Rotation, ScaleMode};

pub struct VideoPlayer {
    child: Option<Child>,
    reader: Option<BufReader<ChildStdout>>,
    pub current_path: Option<PathBuf>,
    pub config: MediaConfig,
    pub is_playing: bool,
    last_frame: Vec<u8>,
}

impl VideoPlayer {
    pub fn new() -> Self {
        Self {
            child: None,
            reader: None,
            current_path: None,
            config: MediaConfig::default(),
            is_playing: true,
            last_frame: vec![0u8; FRAME_RAW_SIZE],
        }
    }

    /// Builds the ffmpeg video filter graph string based on media display settings.
    pub fn build_filter_string(config: &MediaConfig) -> String {
        let mut filters = Vec::new();

        // 1. Rotation filter
        match config.rotation {
            Rotation::Rot0 => {}
            Rotation::Rot90 => filters.push("transpose=1".to_string()),
            Rotation::Rot180 => filters.push("hflip,vflip".to_string()),
            Rotation::Rot270 => filters.push("transpose=2".to_string()),
        }

        // 2. Scaling / cropping filter
        match config.scale_mode {
            ScaleMode::Stretch => {
                filters.push("scale=720:1280".to_string());
            }
            ScaleMode::Fit => {
                filters.push("scale=720:1280:force_original_aspect_ratio=decrease,pad=720:1280:(ow-iw)/2:(oh-ih)/2:black".to_string());
            }
            ScaleMode::Fill => {
                filters.push("scale=720:1280:force_original_aspect_ratio=increase,crop=720:1280".to_string());
            }
            ScaleMode::CustomCrop => {
                let zoom = config.zoom.clamp(0.5, 4.0);
                let pan_x = config.pan_x.clamp(-1.0, 1.0);
                let pan_y = config.pan_y.clamp(-1.0, 1.0);

                let scaled_w = (720.0 * zoom).round() as u32;
                let scaled_h = (1280.0 * zoom).round() as u32;

                // Scale first so image fills at least 720x1280, then crop with offset
                let crop_filter = format!(
                    "scale={}:{}:force_original_aspect_ratio=increase,crop=720:1280:(in_w-720)/2+{}*(in_w-720)/2:(in_h-1280)/2+{}*(in_h-1280)/2",
                    scaled_w, scaled_h, pan_x, pan_y
                );
                filters.push(crop_filter);
            }
        }

        filters.join(",")
    }

    /// Loads and begins decoding a video file.
    pub fn load<P: AsRef<Path>>(&mut self, path: P, config: MediaConfig) -> Result<()> {
        self.stop();

        let path_ref = path.as_ref();
        let path_str = path_ref.to_str().context("Invalid video path encoding")?;
        let filter_str = Self::build_filter_string(&config);

        info!("Launching ffmpeg for video playback: {} (filters: {})", path_str, filter_str);

        let mut cmd = Command::new("ffmpeg");
        cmd.arg("-nostdin")
           .arg("-loglevel").arg("error");

        if config.loop_media {
            cmd.arg("-stream_loop").arg("-1");
        }

        cmd.arg("-i").arg(path_str);

        if !filter_str.is_empty() {
            cmd.arg("-vf").arg(filter_str);
        }

        cmd.arg("-f").arg("rawvideo")
           .arg("-pix_fmt").arg("bgra")
           .arg("-")
           .stdout(Stdio::piped())
           .stderr(Stdio::null());

        let mut child = cmd.spawn().with_context(|| {
            format!("Failed to spawn ffmpeg. Is ffmpeg installed? Target: {}", path_str)
        })?;

        let stdout = child.stdout.take().context("Failed to capture ffmpeg stdout")?;
        self.reader = Some(BufReader::with_capacity(FRAME_RAW_SIZE * 2, stdout));
        self.child = Some(child);
        self.current_path = Some(path_ref.to_path_buf());
        self.config = config;
        self.is_playing = true;

        Ok(())
    }

    /// Updates media configuration and restarts ffmpeg if needed.
    pub fn set_config(&mut self, new_config: MediaConfig) -> Result<()> {
        if self.config != new_config {
            self.config = new_config;
            if let Some(ref path) = self.current_path.clone() {
                self.load(path, self.config.clone())?;
            }
        }
        Ok(())
    }

    /// Reads the next 720x1280 BGRA frame into `buffer`. Returns true if a valid frame was read.
    pub fn read_frame(&mut self, buffer: &mut [u8]) -> bool {
        if buffer.len() < FRAME_RAW_SIZE {
            return false;
        }

        if !self.is_playing {
            // Repeat last frame when paused
            buffer[..FRAME_RAW_SIZE].copy_from_slice(&self.last_frame);
            return true;
        }

        if let Some(ref mut reader) = self.reader {
            match reader.read_exact(&mut self.last_frame) {
                Ok(_) => {
                    buffer[..FRAME_RAW_SIZE].copy_from_slice(&self.last_frame);
                    true
                }
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::UnexpectedEof {
                        // End of stream
                        if !self.config.loop_media {
                            self.is_playing = false;
                        }
                    } else {
                        error!("Video reader error: {}", e);
                    }
                    buffer[..FRAME_RAW_SIZE].copy_from_slice(&self.last_frame);
                    false
                }
            }
        } else {
            false
        }
    }

    /// Stops video decoding and terminates the child process.
    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.reader = None;
    }
}

impl Drop for VideoPlayer {
    fn drop(&mut self) {
        self.stop();
    }
}
