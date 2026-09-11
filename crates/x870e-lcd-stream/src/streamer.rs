//! Background streaming controller for the ASUS ROG X870E Motherboard LCD.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use anyhow::{Context, Result};
use tracing::{error, info};
use x870e_lcd_core::{DisplayMode, LcdDevice, FRAME_RAW_SIZE};

#[derive(Clone)]
pub struct StreamerStats {
    pub frames_sent: Arc<AtomicU32>,
    pub running: Arc<AtomicBool>,
    pub current_fps: Arc<Mutex<f32>>,
}

impl StreamerStats {
    pub fn new() -> Self {
        Self {
            frames_sent: Arc::new(AtomicU32::new(0)),
            running: Arc::new(AtomicBool::new(false)),
            current_fps: Arc::new(Mutex::new(0.0)),
        }
    }

    pub fn get_fps(&self) -> f32 {
        self.current_fps.lock().map(|g| *g).unwrap_or(0.0)
    }
}

pub struct StreamerHandle {
    stats: StreamerStats,
    join_handle: Option<JoinHandle<()>>,
}

impl StreamerHandle {
    pub fn stop(&mut self) {
        self.stats.running.store(false, Ordering::SeqCst);
        if let Some(h) = self.join_handle.take() {
            let _ = h.join();
        }
    }

    pub fn stats(&self) -> &StreamerStats {
        &self.stats
    }
}

impl Drop for StreamerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Spawns a background worker that streams frames produced by `frame_generator` to the LCD.
pub fn start_stream_worker<F>(
    frame_generator: F,
    pacing_ms: u64,
) -> Result<StreamerHandle>
where
    F: FnMut(&mut [u8], &StreamerStats) -> Result<bool> + Send + 'static,
{
    let device = LcdDevice::open().context("Failed to open ASUS ROG X870E LCD panel over USB")?;
    device.enter_frame_stream().context("Failed to set LCD panel to Frame Stream mode")?;

    let stats = StreamerStats::new();
    stats.running.store(true, Ordering::SeqCst);

    let stats_clone = stats.clone();
    let mut gen = frame_generator;

    let join_handle = thread::spawn(move || {
        let mut frame_buf = vec![0u8; FRAME_RAW_SIZE];

        info!("Live frame streaming loop started (pacing: {}ms)...", pacing_ms);

        while stats_clone.running.load(Ordering::SeqCst) {
            let frame_start = Instant::now();

            // 1. Generate frame
            match gen(&mut frame_buf, &stats_clone) {
                Ok(continue_stream) => {
                    if !continue_stream {
                        info!("Frame generator requested end of stream.");
                        break;
                    }
                }
                Err(e) => {
                    error!("Error generating frame: {}", e);
                    break;
                }
            }

            // 2. Transmit frame over Bulk EP 2 OUT
            if let Err(e) = device.send_stream_frame(&frame_buf) {
                error!("Failed to transmit frame to LCD panel: {}", e);
                break;
            }

            stats_clone.frames_sent.fetch_add(1, Ordering::Relaxed);

            // 3. Inter-frame pacing delay
            if pacing_ms > 0 {
                thread::sleep(Duration::from_millis(pacing_ms));
            }

            // 4. Update FPS metrics
            let frame_duration = frame_start.elapsed().as_secs_f32();
            let instant_fps = if frame_duration > 0.0 { 1.0 / frame_duration } else { 0.0 };
            if let Ok(mut lock) = stats_clone.current_fps.lock() {
                *lock = if *lock == 0.0 {
                    instant_fps
                } else {
                    *lock * 0.7 + instant_fps * 0.3
                };
            }
        }

        info!("Exiting streaming loop. Restoring LCD panel to default wallpaper...");
        let _ = device.set_mode(DisplayMode::DefaultWallpaper(0));
        stats_clone.running.store(false, Ordering::SeqCst);
    });

    Ok(StreamerHandle {
        stats,
        join_handle: Some(join_handle),
    })
}
