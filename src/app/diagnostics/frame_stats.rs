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
/// One logging window: frames, the longest gap, and the gap distribution.
struct Window {
    start: Instant,
    start_frames: u64,
    last: Instant,
    worst_ms: u128,
    gaps: u64,
    gap_sum_ms: f64,
    gap_sumsq_ms: f64,
}

static WINDOW_START: Mutex<Option<Window>> = Mutex::new(None);

/// Record one presented frame.
pub(crate) fn note_frame() {
    FRAMES.fetch_add(1, Ordering::Relaxed);
    let period = LOG_PERIOD_MS.load(Ordering::Relaxed);
    if period == 0 {
        return;
    }
    let now = Instant::now();
    let Ok(mut guard) = WINDOW_START.lock() else { return };
    if guard.is_none() {
        *guard = Some(Window {
            start: now,
            start_frames: FRAMES.load(Ordering::Relaxed),
            last: now,
            worst_ms: 0,
            gaps: 0,
            gap_sum_ms: 0.0,
            gap_sumsq_ms: 0.0,
        });
        return;
    }
    let window = guard.as_mut().expect("window initialised above");
    // Gap since the previous present: a hitch shows up even when the window
    // average stays at the target rate.  The mean/σ of the gaps is the judder
    // the eye sees when the average is already on target.
    let gap_ms = now.duration_since(window.last).as_secs_f64() * 1000.0;
    window.gaps += 1;
    window.gap_sum_ms += gap_ms;
    window.gap_sumsq_ms += gap_ms * gap_ms;
    window.worst_ms = window.worst_ms.max(gap_ms as u128);
    window.last = now;
    let elapsed = now.duration_since(window.start).as_millis() as u64;
    if elapsed < period {
        return;
    }
    let frames = FRAMES.load(Ordering::Relaxed).saturating_sub(window.start_frames);
    let fps = frames as f64 * 1000.0 / elapsed as f64;
    let n = window.gaps.max(1) as f64;
    let mean = window.gap_sum_ms / n;
    let variance = (window.gap_sumsq_ms / n - mean * mean).max(0.0);
    log::info!(
        "FPS window: {frames} frames in {:.1} s = {fps:.1} fps, worst frame {:.1} ms, \
         mean gap {mean:.1} ms, jitter {:.1} ms",
        elapsed as f64 / 1000.0,
        window.worst_ms as f64,
        variance.sqrt()
    );
    *guard = Some(Window {
        start: now,
        start_frames: FRAMES.load(Ordering::Relaxed),
        last: now,
        worst_ms: 0,
        gaps: 0,
        gap_sum_ms: 0.0,
        gap_sumsq_ms: 0.0,
    });
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
