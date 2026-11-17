//! Retail single-instance startup gate.
//!
//! Native `WinMain` creates a named mutex before it creates a window. When the
//! name is already taken it raises and restores the window the first copy owns,
//! closes its own handle, logs, and returns without ever creating a second
//! window. Two live copies would otherwise both write `VERA.INI` on quit —
//! the coherent `[Options]`/`[Video]`/`[Audio]` profile and the `[Skirmish]`
//! snapshot are whole-key overwrites, so the copy that quits second silently
//! discards everything the first one saved.
//!
//! ## Dependency rules
//! - Part of util/ — no dependencies on game modules.

/// Name of the process-wide gate.
///
/// Native uses a fixed GUID string as both the mutex name and its window class
/// name. VERA deliberately uses its own name: a retail `gamemd.exe` running
/// side by side is the reference this engine is compared against, and sharing
/// the native name would make either process treat the other as a second copy
/// of itself. VERA-internal deviation; the native gate's shape is unchanged.
#[cfg(windows)]
const INSTANCE_MUTEX_NAME: &str = "VERA20K-SINGLE-INSTANCE";

/// `ERROR_ALREADY_EXISTS` — the only status native treats as "another copy owns
/// the gate". Every other failure falls through into a normal launch.
#[cfg(any(windows, test))]
const ERROR_ALREADY_EXISTS: u32 = 0xb7;

/// `SW_RESTORE` — un-minimises the existing window without changing its size.
#[cfg(windows)]
const SW_RESTORE: i32 = 9;

/// Outcome of the startup gate.
#[derive(Debug)]
pub enum SingleInstance {
    /// This process owns the gate. The guard must live as long as the process:
    /// dropping it releases the name and lets a second copy through.
    Acquired(InstanceGuard),
    /// Another copy already owns the gate and has been raised to the
    /// foreground. The caller must return without creating a window.
    AlreadyRunning,
}

/// Holds the gate open for the lifetime of the process.
#[derive(Debug)]
pub struct InstanceGuard {
    #[cfg(windows)]
    handle: *mut core::ffi::c_void,
    /// The unix gate is an exclusive `flock`; closing the file releases it.
    #[cfg(unix)]
    _lock: std::fs::File,
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        if !self.handle.is_null() {
            // SAFETY: `handle` came from a successful CreateMutexW in
            // `acquire`, is closed exactly once here, and is never duplicated.
            unsafe { CloseHandle(self.handle) };
        }
    }
}

/// Whether `GetLastError` after the create call means another copy owns the
/// gate. Native compares against `ERROR_ALREADY_EXISTS` alone — any other
/// status, including an outright create failure, proceeds into a normal launch.
#[cfg(any(windows, test))]
fn another_copy_owns_gate(last_error: u32) -> bool {
    last_error == ERROR_ALREADY_EXISTS
}

/// Encode a NUL-terminated UTF-16 string for the wide Win32 entry points.
#[cfg(windows)]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateMutexW(
        attributes: *const core::ffi::c_void,
        initial_owner: i32,
        name: *const u16,
    ) -> *mut core::ffi::c_void;
    fn GetLastError() -> u32;
    fn CloseHandle(object: *mut core::ffi::c_void) -> i32;
}

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn FindWindowW(class_name: *const u16, window_name: *const u16) -> *mut core::ffi::c_void;
    fn SetForegroundWindow(window: *mut core::ffi::c_void) -> i32;
    fn ShowWindow(window: *mut core::ffi::c_void, command: i32) -> i32;
}

/// Claim the startup gate, reproducing the native create/raise/bail sequence.
///
/// `window_title` is the title of the window a running copy owns, used to find
/// and raise it. Native calls `FindWindow` with its GUID *class* name; winit
/// registers its own window class, so the title is the available equivalent.
/// VERA-internal substitution; the native lookup key is a class name.
#[cfg(windows)]
pub fn acquire(window_title: &str) -> SingleInstance {
    let name = wide(INSTANCE_MUTEX_NAME);
    // SAFETY: a null security descriptor and a non-owning create are the
    // documented no-argument form; `name` outlives the call and is
    // NUL-terminated. GetLastError is read immediately, before any other call
    // on this thread can overwrite it — the same ordering native relies on.
    let (handle, last_error) = unsafe {
        let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        (handle, GetLastError())
    };
    if !another_copy_owns_gate(last_error) {
        return SingleInstance::Acquired(InstanceGuard { handle });
    }

    // Native raises and restores the first copy's window before bailing, so a
    // second double-click behaves like "show me the game I already started".
    let title = wide(window_title);
    // SAFETY: a null class name matches any class; `title` is NUL-terminated
    // and outlives the call. A null result means no such window, which native
    // also tolerates by skipping the raise.
    unsafe {
        let existing = FindWindowW(std::ptr::null(), title.as_ptr());
        if !existing.is_null() {
            SetForegroundWindow(existing);
            ShowWindow(existing, SW_RESTORE);
        }
        if !handle.is_null() {
            CloseHandle(handle);
        }
    }
    SingleInstance::AlreadyRunning
}

/// Unix hosts take an exclusive `flock` in the runtime directory.
///
/// The native gate is a Win32 named mutex, but the reason it exists — two live
/// copies both rewriting the whole `VERA.INI` profile on quit — applies to
/// every host, and on Linux a second copy also splits the GPU budget with the
/// first. The lock lives in `$XDG_RUNTIME_DIR` (falling back to the temp
/// directory) so a reboot never leaves a stale gate behind.
/// `VERA_ALLOW_MULTI_INSTANCE=1` opts out for automated runs.
#[cfg(unix)]
pub fn acquire(_window_title: &str) -> SingleInstance {
    use std::os::fd::AsRawFd;

    let dev_null = || std::fs::File::open("/dev/null");
    if std::env::var_os("VERA_ALLOW_MULTI_INSTANCE").is_some() {
        return match dev_null() {
            Ok(lock) => SingleInstance::Acquired(InstanceGuard { _lock: lock }),
            Err(_) => SingleInstance::AlreadyRunning,
        };
    }
    let dir = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(runtime) => std::path::PathBuf::from(runtime).join("vera20k"),
        None => std::env::temp_dir().join("vera20k"),
    };
    // An unwritable runtime directory must not stop the game from starting;
    // native also proceeds when its mutex create fails for another reason.
    let Ok(()) = std::fs::create_dir_all(&dir) else {
        return match dev_null() {
            Ok(lock) => SingleInstance::Acquired(InstanceGuard { _lock: lock }),
            Err(_) => SingleInstance::AlreadyRunning,
        };
    };
    let Ok(lock) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("instance.lock"))
    else {
        return match dev_null() {
            Ok(lock) => SingleInstance::Acquired(InstanceGuard { _lock: lock }),
            Err(_) => SingleInstance::AlreadyRunning,
        };
    };
    // SAFETY: `lock` owns a valid descriptor for the duration of the call and
    // LOCK_EX|LOCK_NB never blocks.
    let claimed = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if claimed {
        SingleInstance::Acquired(InstanceGuard { _lock: lock })
    } else {
        SingleInstance::AlreadyRunning
    }
}

/// Hosts with neither the Win32 mutex nor `flock` stay ungated.
#[cfg(not(any(windows, unix)))]
pub fn acquire(_window_title: &str) -> SingleInstance {
    SingleInstance::Acquired(InstanceGuard {})
}

#[cfg(all(test, tests_util))]
mod tests {
    use super::*;

    #[test]
    fn only_error_already_exists_reports_a_running_copy() {
        assert!(another_copy_owns_gate(ERROR_ALREADY_EXISTS));
        // Success, and every failure native does not test for, launch normally.
        assert!(!another_copy_owns_gate(0));
        assert!(!another_copy_owns_gate(5)); // ERROR_ACCESS_DENIED
        assert!(!another_copy_owns_gate(0xb6)); // adjacent status, not the gate
        assert!(!another_copy_owns_gate(0xb8));
    }
}
