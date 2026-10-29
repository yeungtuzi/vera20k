//! Presentation frame cap.
//!
//! The window presents with `PresentMode::Fifo`, so the compositor returns one
//! frame per display refresh. A 144 Hz panel therefore renders 144 frames a
//! second for a simulation that advances a few ticks per second, which heats the
//! machine for no gameplay benefit. Holding the frame slot is what a 60 Hz
//! display did in retail.
//!
//! Focused play is capped at 60 fps by default (`VERA_FPS_CAP` overrides it, `0`
//! disables the cap). An unfocused window drops to 15 fps (`VERA_FPS_CAP_IDLE`),
//! because the player is not looking at it and the frames cost real power.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// QoS classes: the scheduler prefers P-cores for interactive work and E-cores
/// for background work, which is the only way macOS lets a process influence its
/// own core type. Unfocused frames are background work.
#[cfg(target_os = "macos")]
const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
#[cfg(target_os = "macos")]
const QOS_CLASS_BACKGROUND: u32 = 0x09;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
}

thread_local! {
    /// Last presentation instant for this thread; the render callback runs on the
    /// event-loop thread only.
    static LAST: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
    /// QoS class currently requested for this thread.
    static QOS: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Ask the scheduler for P-cores while focused and E-cores while unfocused.
///
/// Darwin has no core pinning; QoS is the supported hint and it steers placement.
fn apply_qos(focused: bool) {
    #[cfg(target_os = "macos")]
    {
        if QOS.with(|cell| cell.get()) == Some(focused) {
            return;
        }
        let class = if focused {
            QOS_CLASS_USER_INTERACTIVE
        } else {
            QOS_CLASS_BACKGROUND
        };
        // SAFETY: pthread_set_qos_class_self_np takes a plain class value and a
        // relative priority; both are in range for the caller's own thread.
        let result = unsafe { pthread_set_qos_class_self_np(class, 0) };
        if result == 0 {
            QOS.with(|cell| cell.set(Some(focused)));
        } else {
            log::warn!("pthread_set_qos_class_self_np failed: {result}");
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = focused;
}

/// Frames per second granted to presentation; `None` means uncapped.
fn cap_hz(focused: bool) -> Option<u32> {
    static FOCUSED: OnceLock<Option<u32>> = OnceLock::new();
    static UNFOCUSED: OnceLock<Option<u32>> = OnceLock::new();
    if focused {
        *FOCUSED.get_or_init(|| {
            let cap = parse_cap(std::env::var("VERA_FPS_CAP").ok().as_deref(), 60);
            match cap {
                Some(hz) => log::info!("presentation cap: {hz} fps"),
                None => log::info!("presentation cap: disabled"),
            }
            cap
        })
    } else {
        *UNFOCUSED.get_or_init(|| {
            let cap = parse_cap(std::env::var("VERA_FPS_CAP_IDLE").ok().as_deref(), 15);
            match cap {
                Some(hz) => log::info!("presentation cap (unfocused): {hz} fps"),
                None => log::info!("presentation cap (unfocused): disabled"),
            }
            cap
        })
    }
}

/// Parse a cap override: unset or empty keeps `default`, `0` means uncapped.
fn parse_cap(raw: Option<&str>, default: u32) -> Option<u32> {
    match raw {
        None | Some("") => Some(default),
        Some(text) => match text.parse::<u32>() {
            Ok(0) => None,
            Ok(hz) => Some(hz),
            Err(_) => Some(default),
        },
    }
}

/// Sleep until the next presentation slot is due.
///
/// A late frame resets the slot instead of accumulating debt, so a slow render
/// never busy-loops to catch up.
pub(crate) fn pace_frame(focused: bool) {
    apply_qos(focused);
    let Some(hz) = cap_hz(focused) else {
        return;
    };
    let slot = Duration::from_secs_f64(1.0 / f64::from(hz));
    let last = LAST.with(|cell| {
        let previous = cell.get();
        cell.set(Some(Instant::now()));
        previous
    });
    let Some(last) = last else {
        return;
    };
    let elapsed = last.elapsed();
    if elapsed < slot {
        std::thread::sleep(slot - elapsed);
    }
}

#[cfg(test)]
mod tests {
    use super::parse_cap;

    #[test]
    fn unset_or_empty_keeps_the_given_default() {
        assert_eq!(parse_cap(None, 60), Some(60));
        assert_eq!(parse_cap(Some(""), 15), Some(15));
    }

    #[test]
    fn zero_disables_the_cap_and_garbage_keeps_the_default() {
        assert_eq!(parse_cap(Some("0"), 60), None);
        assert_eq!(parse_cap(Some("144"), 60), Some(144));
        assert_eq!(parse_cap(Some("abc"), 15), Some(15));
    }
}
