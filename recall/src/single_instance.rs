//! Per-user single-instance guard: a named mutex in the session's `Local\` namespace.
//!
//! Shared by the engine, the controller, the overlay and the app (`#[path]` includes), so it
//! uses raw kernel32 only and no crate features. Each program role has its own name, so a
//! child the controller starts is never blocked by its parent's guard.
//!
//! Windows closes every handle of a process when it ends, a crash or a kill included, so a
//! dead instance never blocks the next start. A restart that overlaps the old process's last
//! moments waits with [`claim_within`].
#![allow(dead_code)]

use std::time::{Duration, Instant};

/// Controller (`recall-hdr.exe`): the name older builds used too, so an old and a new
/// controller still see each other across an install.
pub const CONTROLLER: &str = "recall-hdr-controller";
/// The recording engine (`recall.exe` without a one-shot command).
pub const ENGINE: &str = "Recall.Engine";
/// The island (`recall-overlay.exe --parent PID`).
pub const OVERLAY: &str = "Recall.Overlay";
/// The library app (`recall-native.exe`).
pub const APP: &str = "Recall.App";

/// `RECALL_INSTANCE_TAG` gives tests and checks their own guard (`Local\<role>.<tag>`), so
/// they never collide with the Recall the person at the PC runs.
pub const TAG_ENV: &str = "RECALL_INSTANCE_TAG";

/// Poll interval while waiting for an old instance to end.
const POLL: Duration = Duration::from_millis(100);

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateMutexW(attributes: *const core::ffi::c_void, initial_owner: i32, name: *const u16) -> *mut core::ffi::c_void;
    fn CloseHandle(handle: *mut core::ffi::c_void) -> i32;
    fn GetLastError() -> u32;
}
const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_ALREADY_EXISTS: u32 = 183;

/// Held for as long as this process is the instance. Dropping it lets the next one start.
pub struct Guard {
    handle: *mut core::ffi::c_void,
}

// A kernel handle may be closed from any thread.
unsafe impl Send for Guard {}

impl Drop for Guard {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

/// The result of one claim.
pub enum Claim {
    /// This process is the instance; keep the guard alive.
    First(Guard),
    /// Another process holds it.
    Taken,
    /// The mutex could not be made (the Win32 error); run without a guard.
    Unavailable(u32),
}

impl Claim {
    pub fn is_first(&self) -> bool {
        matches!(self, Claim::First(_))
    }
    pub fn is_taken(&self) -> bool {
        matches!(self, Claim::Taken)
    }
}

/// `Local\<role>`, or `Local\<role>.<tag>` when [`TAG_ENV`] is set.
pub fn mutex_name(role: &str) -> String {
    name_with_tag(role, std::env::var(TAG_ENV).ok().as_deref())
}

/// [`TAG_ENV`] cleaned as the guards use it ("" when unset); other per-instance
/// names (the overlay's ETW session) follow it too.
pub fn tag() -> String {
    clean_tag(std::env::var(TAG_ENV).ok().as_deref())
}

fn clean_tag(tag: Option<&str>) -> String {
    // A backslash would leave the Local namespace; keep only plain characters.
    tag.unwrap_or("").chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')).collect()
}

fn name_with_tag(role: &str, tag: Option<&str>) -> String {
    let tag = clean_tag(tag);
    if tag.is_empty() {
        format!("Local\\{role}")
    } else {
        format!("Local\\{role}.{tag}")
    }
}

/// Claims `name` once.
pub fn claim(name: &str) -> Claim {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
    let error = unsafe { GetLastError() };
    if handle.is_null() {
        // Access denied: the mutex exists under another security context (an elevated
        // instance). That is still another instance.
        return if error == ERROR_ACCESS_DENIED { Claim::Taken } else { Claim::Unavailable(error) };
    }
    if error == ERROR_ALREADY_EXISTS {
        unsafe { CloseHandle(handle) };
        return Claim::Taken;
    }
    Claim::First(Guard { handle })
}

/// Claims `name`, waiting up to `wait` for an instance that is ending (a restart or an
/// install that just quit the old one).
pub fn claim_within(name: &str, wait: Duration) -> Claim {
    let until = Instant::now() + wait;
    loop {
        match claim(name) {
            Claim::Taken if Instant::now() < until => std::thread::sleep(POLL),
            other => return other,
        }
    }
}

#[cfg(test)]
mod single_instance_tests {
    use super::*;

    fn unique(role: &str) -> String {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        name_with_tag(role, Some(&format!("test-{}-{n}", std::process::id())))
    }

    #[test]
    fn the_second_claim_loses_while_the_first_holds() {
        let name = unique("Recall.Test");
        let first = claim(&name);
        assert!(first.is_first(), "first claim wins");
        assert!(claim(&name).is_taken(), "second claim loses");
        drop(first);
        assert!(claim(&name).is_first(), "a released guard frees the name");
    }

    #[test]
    fn roles_do_not_block_each_other() {
        let tag = format!("roles-{}", std::process::id());
        let names: Vec<String> = [CONTROLLER, ENGINE, OVERLAY, APP].iter().map(|r| name_with_tag(r, Some(&tag))).collect();
        let held: Vec<Claim> = names.iter().map(|n| claim(n)).collect();
        assert!(held.iter().all(Claim::is_first), "each role has its own name");
    }

    #[test]
    fn a_waiting_claim_takes_over_when_the_old_instance_ends() {
        let name = unique("Recall.Test");
        let first = claim(&name);
        assert!(first.is_first());
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(first);
        });
        let started = Instant::now();
        let second = claim_within(&name, Duration::from_secs(5));
        releaser.join().unwrap();
        assert!(second.is_first(), "the waiter takes over");
        assert!(started.elapsed() < Duration::from_secs(3), "and does not wait out the whole budget");
    }

    #[test]
    fn a_waiting_claim_gives_up_after_its_budget() {
        let name = unique("Recall.Test");
        let _first = claim(&name);
        let started = Instant::now();
        assert!(claim_within(&name, Duration::from_millis(300)).is_taken());
        assert!(started.elapsed() >= Duration::from_millis(300));
    }

    #[test]
    fn names_stay_in_the_local_namespace() {
        assert_eq!(name_with_tag(APP, None), "Local\\Recall.App");
        assert_eq!(name_with_tag(APP, Some("")), "Local\\Recall.App");
        assert_eq!(name_with_tag(ENGINE, Some("a\\b c")), "Local\\Recall.Engine.abc");
        assert_eq!(name_with_tag(CONTROLLER, None), "Local\\recall-hdr-controller");
    }
}
