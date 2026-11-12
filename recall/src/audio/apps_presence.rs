//! The status label of a chosen app that runs but has not played a sound yet. Capture starts
//! from the app's first audio session (`apps_sessions.rs`); without this, such an app would
//! read "(not running)" until then. While some chosen app is closed, the scanner lists
//! processes every [`PRESENCE_EVERY`] (Toolhelp snapshot: no process is opened, the game's
//! included) and only relabels. Kept apart so it can be dropped on its own.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::super::voice::{tree_root, ProcEntry};
use super::super::TrackRing;

/// timer-ok: no process-start event exists without admin rights; the label only.
pub(super) const PRESENCE_EVERY: Duration = Duration::from_secs(2);

/// One chosen app (not the game's own exe) as the scanner sees it.
pub(super) struct Slot {
    pub ring: Arc<TrackRing>,
    pub exe: String,
    /// Its watcher is not capturing it (the watcher sets this).
    pub closed: AtomicBool,
    /// The pid the label names while it runs without a capture.
    shown: Mutex<Option<u32>>,
}

impl Slot {
    pub(super) fn new(ring: Arc<TrackRing>, exe: String) -> Self {
        Slot { ring, exe, closed: AtomicBool::new(true), shown: Mutex::new(None) }
    }

    /// The watcher took over the label (a capture started or ended).
    pub(super) fn forget(&self) {
        *self.shown.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

/// Relabel closed apps that run ("running") or stopped running again ("closed").
pub(super) fn update_labels(slots: &[Arc<Slot>], procs: &[ProcEntry]) {
    for slot in slots.iter().filter(|s| s.closed.load(Ordering::SeqCst)) {
        let pid = tree_root(procs, &slot.exe);
        let mut shown = slot.shown.lock().unwrap_or_else(|p| p.into_inner());
        if *shown == pid {
            continue;
        }
        let text = match pid {
            Some(pid) => label_running(&slot.exe, pid),
            None => format!("{} (closed)", slot.exe),
        };
        if let Ok(mut endpoint) = slot.ring.endpoint.lock() {
            *endpoint = text;
        }
        *shown = pid;
    }
}

/// A running app that has not played a sound yet.
pub(super) fn label_running(exe: &str, pid: u32) -> String {
    format!("{exe} pid {pid} (running; recorded from its first sound)")
}
