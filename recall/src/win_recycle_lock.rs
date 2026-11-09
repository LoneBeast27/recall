//! One Recycle Bin move at a time across every Recall process. The storage cap (`recall-hdr`)
//! and the library's Delete (`recall-native`) each check the bin's free room
//! (`win_recycle.rs`) and then move files (`win_fileop.rs`). Without a shared lock both checks
//! can pass on the same room and the second move overflows the bin, and Windows then purges
//! older bin items for good to make room. So both hold the named mutex [`NAME`] from the room
//! check to the end of the call ([`held`]).
//!
//! A mutex left by a process that died is taken over (`WAIT_ABANDONED`): the move it guarded
//! is over. A lock not free within [`WAIT`] (another move stuck on Windows) refuses with
//! `win_fileop::STUCK_REFUSAL`: nothing is moved, the caller tells the user as for a busy bin.

use std::ffi::c_void;
use std::time::Duration;

/// Session-local, like the processes that share it.
pub const NAME: &str = r"Local\RecallRecycleBin";
/// Longer than one call may take (`win_fileop::WAIT`, 30 s) plus its room check.
pub const WAIT: Duration = Duration::from_secs(60);

const WAIT_OBJECT_0: u32 = 0;
const WAIT_ABANDONED: u32 = 0x80;

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn CreateMutexW(attributes: *const c_void, initial_owner: i32, name: *const u16) -> *mut c_void;
    fn WaitForSingleObject(handle: *mut c_void, millis: u32) -> u32;
    fn ReleaseMutex(handle: *mut c_void) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

/// The lock, held by this thread until dropped.
pub struct Held(*mut c_void);

impl Drop for Held {
    fn drop(&mut self) {
        // SAFETY: this thread owns the mutex (acquired below) and the handle is ours.
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

/// Take the named mutex `name`, waiting at most `wait`.
pub fn acquire(name: &str, wait: Duration) -> Result<Held, String> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: a NUL-terminated name; default security; not owned at creation.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
    if handle.is_null() {
        return Err(format!("couldn't open the Recycle Bin lock ({}); nothing was moved", std::io::Error::last_os_error()));
    }
    let millis = u32::try_from(wait.as_millis()).unwrap_or(u32::MAX - 1);
    // SAFETY: a valid mutex handle.
    match unsafe { WaitForSingleObject(handle, millis) } {
        WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Held(handle)),
        _ => {
            // SAFETY: the handle is ours and not owned.
            unsafe { CloseHandle(handle) };
            Err(crate::win_fileop::STUCK_REFUSAL.into())
        }
    }
}

/// Runs `body` (the room check and the move) while holding the shared lock; Err when the
/// lock could not be taken (then `body` never runs).
pub fn held<T>(body: impl FnOnce() -> T) -> Result<T, String> {
    held_with(|| acquire(NAME, WAIT), body)
}

/// [`held`] with an injected lock.
pub fn held_with<G, T>(lock: impl FnOnce() -> Result<G, String>, body: impl FnOnce() -> T) -> Result<T, String> {
    let guard = lock()?;
    let out = body();
    drop(guard);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Recorded(Arc<Mutex<Vec<&'static str>>>);
    impl Drop for Recorded {
        fn drop(&mut self) {
            self.0.lock().unwrap().push("unlock");
        }
    }

    #[test]
    fn the_check_and_the_move_run_inside_the_lock() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let (a, b) = (log.clone(), log.clone());
        let out = held_with(
            move || {
                a.lock().unwrap().push("lock");
                Ok(Recorded(a.clone()))
            },
            move || {
                b.lock().unwrap().extend(["check", "move"]);
                7
            },
        );
        assert_eq!(out, Ok(7));
        assert_eq!(log.lock().unwrap().clone(), vec!["lock", "check", "move", "unlock"]);
        let refused = held_with(|| Err::<(), _>("busy".to_string()), || panic!("never runs"));
        assert_eq!(refused, Err::<(), _>("busy".into()));
    }

    fn unique() -> String {
        format!(r"Local\RecallRecycleBinTest-{}-{:?}", std::process::id(), std::thread::current().id())
    }

    #[test]
    fn the_named_mutex_admits_one_holder_and_survives_a_dead_one() {
        let name = unique();
        let held = acquire(&name, Duration::ZERO).expect("free at first");
        let other = name.clone();
        let busy = std::thread::spawn(move || acquire(&other, Duration::from_millis(50)).is_err()).join().unwrap();
        assert!(busy, "a second holder waits, then refuses");
        drop(held);
        let other = name.clone();
        assert!(std::thread::spawn(move || acquire(&other, Duration::ZERO).is_ok()).join().unwrap(), "free again once released");
        // A holder that ends without releasing (a crashed process): the next caller takes over.
        let other = name.clone();
        std::thread::spawn(move || std::mem::forget(acquire(&other, Duration::ZERO).unwrap())).join().unwrap();
        assert!(acquire(&name, Duration::from_millis(500)).is_ok(), "abandoned = free");
    }
}
