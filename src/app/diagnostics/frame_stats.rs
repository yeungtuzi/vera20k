//! Process-wide frame statistics.
//!
//! The on-screen Performance Stats panel is ephemeral (Ctrl+Alt+F), which
//! leaves no artefact after the process exits.  The launcher wants evidence of
//! the achieved frame rate, so count frames here and log one summary line at
//! shutdown.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::util::time::Instant;

static FRAMES: AtomicU64 = AtomicU64::new(0);
static ADAPTER: Mutex<String> = Mutex::new(String::new());

/// Per-second FPS log requested by `VERA_FPS_LOG=<seconds>` (0 = off).
static LOG_PERIOD_MS: AtomicU64 = AtomicU64::new(0);
static WINDOW_START: Mutex<Option<(Instant, u64)>> = Mutex::new(None);

/// Record one presented frame.
pub(crate) fn note_frame() {
    FRAMES.fetch_add(1, Ordering::Relaxed);
    let period = LOG_PERIOD_MS.load(Ordering::Relaxed);
    if period == 0 {
        return;
    }
    let now = Instant::now();
    let Ok(mut window) = WINDOW_START.lock() else { return };
    match *window {
        None => *window = Some((now, FRAMES.load(Ordering::Relaxed))),
        Some((start, start_frames)) => {
            let elapsed = now.duration_since(start).as_millis() as u64;
            if elapsed >= period {
                let frames = FRAMES.load(Ordering::Relaxed).saturating_sub(start_frames);
                let fps = frames as f64 * 1000.0 / elapsed as f64;
                log::info!(
                    "FPS window: {frames} frames in {:.1} s = {fps:.1} fps",
                    elapsed as f64 / 1000.0
                );
                *window = Some((now, FRAMES.load(Ordering::Relaxed)));
            }
        }
    }
}

/// `VERA_FPS_LOG=<milliseconds>`: log a rolling FPS line that often.
pub(crate) fn configure_from_env() {
    let ms = std::env::var("VERA_FPS_LOG")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    LOG_PERIOD_MS.store(ms, Ordering::Relaxed);
}

/// Remember which wgpu adapter the run ended up on.
pub(crate) fn set_adapter(name: &str) {
    if let Ok(mut adapter) = ADAPTER.lock() {
        adapter.clear();
        adapter.push_str(name);
    }
}

/// One line for the log: adapter, frame count, wall time and average FPS.
pub(crate) fn summary(start: Instant) -> String {
    let frames = FRAMES.load(Ordering::Relaxed);
    let seconds = start.elapsed().as_secs_f64();
    let fps = if seconds > 0.0 { frames as f64 / seconds } else { 0.0 };
    let adapter = ADAPTER.lock().map(|a| a.clone()).unwrap_or_default();
    format!(
        "Performance summary: adapter={adapter}, frames={frames}, seconds={seconds:.1}, avg_fps={fps:.1}"
    )
}
