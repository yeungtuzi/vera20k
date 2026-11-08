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

/// Record one presented frame.
pub(crate) fn note_frame() {
    FRAMES.fetch_add(1, Ordering::Relaxed);
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
