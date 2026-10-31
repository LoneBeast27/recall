//! `ReadDirectoryChangesW` on one clip folder: one thread per folder, asleep until the OS
//! reports a change. It only reads directory notifications and never writes to the drive.
use std::{
    ffi::OsString,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    sync::{Arc, mpsc::Sender},
    time::Duration,
};

use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_NOTIFY_ENUM_DIR, HANDLE, WAIT_OBJECT_0, WIN32_ERROR},
        Storage::FileSystem::{
            CreateFileW, FILE_ACTION, FILE_ACTION_ADDED, FILE_ACTION_REMOVED, FILE_ACTION_RENAMED_NEW_NAME, FILE_ACTION_RENAMED_OLD_NAME,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY, FILE_NAME_OPENED, FILE_NOTIFY_CHANGE_DIR_NAME,
            FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, GETFINALPATHNAMEBYHANDLE_FLAGS, GetFinalPathNameByHandleW, OPEN_EXISTING, ReadDirectoryChangesW, VOLUME_NAME_DOS,
        },
        System::{
            IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
            Threading::{CreateEventW, ResetEvent, SetEvent, WaitForMultipleObjects, WaitForSingleObject},
        },
    },
    core::PCWSTR,
};

use super::watch::WatchRoot;

/// One read's worth of notifications; a larger burst overflows and costs one re-list.
const BUFFER_BYTES: usize = 64 * 1024;
/// How often a folder that is gone is looked for again.
const RETRY: Duration = Duration::from_secs(3);
/// How often a watched folder is checked for still being at its path. A renamed or recycled
/// folder keeps its handle and sends no event; this asks the handle, not the drive.
const MOVED_CHECK: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    Added,
    Modified,
    Removed,
}

/// What the folder threads report to the settle thread.
pub(super) enum Raw {
    Change { root: PathBuf, recursive: bool, path: PathBuf, action: Action },
    /// More happened than the buffer holds: only a re-list can tell what.
    Overflow(PathBuf),
    Lost(PathBuf),
    Back(PathBuf),
    RootsChanged,
    Stop,
}

struct Handle(HANDLE);
// A kernel handle, used by one thread at a time or (`Stop::set`) from any thread.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn new_event() -> Option<Handle> {
    unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.ok().map(Handle)
}

/// Wakes a folder thread out of its wait.
pub(super) struct Stop(Handle);

impl Stop {
    pub fn new() -> Option<Self> {
        new_event().map(Self)
    }

    pub fn set(&self) {
        let _ = unsafe { SetEvent(self.0.0) };
    }

    /// True when stopped within `timeout`.
    fn wait(&self, timeout: Duration) -> bool {
        let wait = unsafe { WaitForSingleObject(self.0.0, timeout.as_millis() as u32) };
        wait == WAIT_OBJECT_0
    }
}

enum Ended {
    Stopped,
    /// The folder went away after it had been watched.
    Lost,
    /// It could not be watched at all.
    NeverArmed,
}

/// Watches `root` until stopped, re-arming whenever a lost folder is back. `armed` is told once
/// the first read is queued (or the first try failed): changes from then on are never missed.
pub(super) fn watch_root(root: WatchRoot, stop: Arc<Stop>, tx: Sender<Raw>, armed: Sender<()>) {
    let mut armed = Some(armed);
    let mut was_lost = false;
    loop {
        let ended = match open_dir(&root.path) {
            Some(dir) => pump(&dir, &root, &stop, &tx, &mut armed, was_lost),
            None => Ended::NeverArmed,
        };
        if let Some(armed) = armed.take() {
            let _ = armed.send(());
        }
        match ended {
            Ended::Stopped => return,
            Ended::Lost => {
                was_lost = true;
                if tx.send(Raw::Lost(root.path.clone())).is_err() {
                    return;
                }
            }
            // A folder missing at launch counts as lost, so its return re-lists the library.
            Ended::NeverArmed => was_lost = true,
        }
        if stop.wait(RETRY) {
            return;
        }
    }
}

fn open_dir(path: &Path) -> Option<Handle> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    let flags = FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED;
    unsafe { CreateFileW(PCWSTR(wide.as_ptr()), FILE_LIST_DIRECTORY.0, share, None, OPEN_EXISTING, flags, None) }.ok().map(Handle)
}

/// Where the open folder is now, as Windows knows it. The opened name comes from the handle;
/// a normalized one would read every parent folder, and this runs every [`MOVED_CHECK`].
fn final_path(dir: &Handle) -> Option<OsString> {
    let mut buffer = vec![0u16; 32 * 1024];
    let flags = GETFINALPATHNAMEBYHANDLE_FLAGS(FILE_NAME_OPENED.0 | VOLUME_NAME_DOS.0);
    let len = unsafe { GetFinalPathNameByHandleW(dir.0, &mut buffer, flags) } as usize;
    (len > 0 && len < buffer.len()).then(|| OsString::from_wide(&buffer[..len]))
}

fn pump(dir: &Handle, root: &WatchRoot, stop: &Stop, tx: &Sender<Raw>, armed: &mut Option<Sender<()>>, was_lost: bool) -> Ended {
    let Some(event) = new_event() else { return Ended::NeverArmed };
    let home = final_path(dir);
    // u32 storage: the OS wants the buffer DWORD-aligned.
    let mut buffer = vec![0u32; BUFFER_BYTES / 4];
    let filter = FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME | FILE_NOTIFY_CHANGE_SIZE | FILE_NOTIFY_CHANGE_LAST_WRITE;
    let mut first = true;
    loop {
        let mut overlapped = OVERLAPPED { hEvent: event.0, ..Default::default() };
        let _ = unsafe { ResetEvent(event.0) };
        let queued = unsafe {
            ReadDirectoryChangesW(dir.0, buffer.as_mut_ptr().cast(), BUFFER_BYTES as u32, root.recursive, filter, None, Some(&raw mut overlapped), None)
        };
        if queued.is_err() {
            return if first { Ended::NeverArmed } else { Ended::Lost };
        }
        if first {
            first = false;
            if let Some(armed) = armed.take() {
                let _ = armed.send(());
            }
            if was_lost && tx.send(Raw::Back(root.path.clone())).is_err() {
                cancel(dir, &overlapped);
                return Ended::Stopped;
            }
        }
        loop {
            let wait = unsafe { WaitForMultipleObjects(&[stop.0.0, event.0], false, MOVED_CHECK.as_millis() as u32) };
            if wait == WAIT_OBJECT_0 {
                cancel(dir, &overlapped);
                return Ended::Stopped;
            }
            if wait.0 == WAIT_OBJECT_0.0 + 1 {
                break;
            }
            if final_path(dir) != home {
                cancel(dir, &overlapped);
                return Ended::Lost;
            }
        }
        let mut bytes = 0u32;
        let done = unsafe { GetOverlappedResult(dir.0, &overlapped, &mut bytes, false) };
        let overflow = match done {
            Ok(()) => bytes == 0,
            Err(error) if WIN32_ERROR::from_error(&error) == Some(ERROR_NOTIFY_ENUM_DIR) => true,
            Err(_) => return Ended::Lost,
        };
        if overflow {
            if !root.path.is_dir() {
                return Ended::Lost;
            }
            if tx.send(Raw::Overflow(root.path.clone())).is_err() {
                return Ended::Stopped;
            }
            continue;
        }
        let data: Vec<u8> = buffer.iter().flat_map(|word| word.to_ne_bytes()).take(bytes as usize).collect();
        for (action, name) in parse_notifications(&data) {
            let path = root.path.join(name);
            let change = Raw::Change { root: root.path.clone(), recursive: root.recursive, path, action };
            if tx.send(change).is_err() {
                return Ended::Stopped;
            }
        }
    }
}

/// Cancels the queued read and waits for it: the OS owns the buffer until then.
fn cancel(dir: &Handle, overlapped: &OVERLAPPED) {
    let _ = unsafe { CancelIoEx(dir.0, Some(std::ptr::from_ref(overlapped))) };
    let mut bytes = 0u32;
    let _ = unsafe { GetOverlappedResult(dir.0, overlapped, &mut bytes, true) };
}

/// Decodes a `FILE_NOTIFY_INFORMATION` chain: (action, path relative to the folder).
pub(super) fn parse_notifications(data: &[u8]) -> Vec<(Action, OsString)> {
    const NAME_AT: usize = 12;
    let word = |at: usize| data.get(at..at + 4).map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let mut found = Vec::new();
    let mut at = 0usize;
    while let (Some(next), Some(action), Some(len)) = (word(at), word(at + 4), word(at + 8)) {
        let Some(name) = data.get(at + NAME_AT..at + NAME_AT + len) else { break };
        let units: Vec<u16> = name.chunks_exact(2).map(|c| u16::from_ne_bytes([c[0], c[1]])).collect();
        found.push((action_of(FILE_ACTION(action as u32)), OsString::from_wide(&units)));
        if next == 0 {
            break;
        }
        at += next;
    }
    found
}

fn action_of(action: FILE_ACTION) -> Action {
    match action {
        FILE_ACTION_ADDED | FILE_ACTION_RENAMED_NEW_NAME => Action::Added,
        FILE_ACTION_REMOVED | FILE_ACTION_RENAMED_OLD_NAME => Action::Removed,
        _ => Action::Modified,
    }
}
