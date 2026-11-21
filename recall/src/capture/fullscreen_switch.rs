//! Exclusive fullscreen and the island: a game in exclusive fullscreen recorded with monitor
//! Desktop Duplication hides every window over it, Recall's island included. Recorded with WGC
//! window capture, the island showed over the same game in the same mode (the user's
//! report, 2026-11-20: it showed before Desktop Duplication was turned on, and hides since).
//!
//! So capture picks the method per game, with no setting:
//! - A followed game seen in exclusive fullscreen while it is in front for [`ENTER_AFTER`]
//!   moves to WGC window capture (a capture restart of the same size: the replay buffer is
//!   kept), and its exe is remembered in `%APPDATA%\Recall\fullscreen-games.txt`, so its next
//!   run starts on WGC with no switch.
//! - A remembered game seen in front and out of exclusive fullscreen for [`LEAVE_AFTER`]
//!   (the user picked a borderless mode) is forgotten and goes back to Desktop Duplication.
//!
//! Exclusive fullscreen is the shell's `QUNS_RUNNING_D3D_FULL_SCREEN`; whether the island is
//! actually on screen cannot be read, so this is the signal. Only window captures switch; a
//! desktop capture never does. Under `RECALL_INSTANCE_TAG` (checks) nothing is written.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

use super::CaptureShared;

/// Exclusive fullscreen this long, game in front, before Desktop Duplication hands over (a
/// mode switch passes through it briefly).
pub(crate) const ENTER_AFTER: Duration = Duration::from_secs(2);
/// Out of exclusive fullscreen this long, game in front, before a remembered game goes back.
pub(crate) const LEAVE_AFTER: Duration = Duration::from_secs(10);
/// How often a WGC window capture looks (from its frame callback).
pub(crate) const CHECK_EVERY: Duration = Duration::from_millis(250);

const FILE: &str = "fullscreen-games.txt";

/// One look at the followed game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Look {
    /// The condition the watch waits for holds (game in front, in or out of exclusive
    /// fullscreen as the watch asks).
    Holds,
    /// Anything else: the game is not in front, minimized, or the other mode.
    Not,
}

/// Times one condition across looks; true once it held for `after` without a break.
#[derive(Debug, Default)]
pub(crate) struct Watch {
    since: Option<Instant>,
}

impl Watch {
    pub(crate) fn update(&mut self, look: Look, now: Instant, after: Duration) -> bool {
        if look == Look::Not {
            self.since = None;
            return false;
        }
        now.saturating_duration_since(*self.since.get_or_insert(now)) >= after
    }
}

/// The look a watch for `want_exclusive` gets.
pub(crate) fn look(game_in_front: bool, exclusive: bool, want_exclusive: bool) -> Look {
    if game_in_front && exclusive == want_exclusive { Look::Holds } else { Look::Not }
}

/// The remembered list key for an exe path: its file name, lower case.
pub(crate) fn key(exe_path: &str) -> Option<String> {
    let name = exe_path.rsplit(['\\', '/']).next()?.trim();
    (!name.is_empty()).then(|| name.to_ascii_lowercase())
}

/// The list's lines, one key each (blank lines and duplicates ignored).
pub(crate) fn parse(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let line = line.to_ascii_lowercase();
        if !out.contains(&line) {
            out.push(line);
        }
    }
    out
}

/// `list` with `key` added (`add`) or removed; None when nothing changes.
pub(crate) fn edited(list: &[String], key: &str, add: bool) -> Option<Vec<String>> {
    let has = list.iter().any(|k| k == key);
    match (add, has) {
        (true, false) => Some(list.iter().cloned().chain([key.to_string()]).collect()),
        (false, true) => Some(list.iter().filter(|k| *k != key).cloned().collect()),
        _ => None,
    }
}

/// The file's text for `list`.
pub(crate) fn render(list: &[String]) -> String {
    list.iter().map(|k| format!("{k}\n")).collect()
}

fn file() -> Option<PathBuf> {
    if std::env::var_os(crate::single_instance::TAG_ENV).is_some_and(|v| !v.is_empty()) {
        return None;
    }
    Some(crate::user_dirs::checked(crate::user_dirs::appdata()?.join("Recall").join(FILE)))
}

fn read(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path).map(|t| parse(&t)).unwrap_or_default()
}

/// Writes through a temp sibling and a rename, so a crash never leaves half a list.
fn write(path: &Path, list: &[String]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("txt.tmp");
    std::fs::write(&tmp, render(list))?;
    std::fs::rename(&tmp, path)
}

/// Adds or removes a game in the remembered list (a failed write is logged; this run's
/// choice holds either way through `CaptureShared::fullscreen_wgc`).
fn remember(key: &str, add: bool) {
    let Some(path) = file() else { return };
    if let Some(list) = edited(&read(&path), key, add) {
        if let Err(e) = write(&path, &list) {
            eprintln!("[recall] capture: could not update {}: {e}", path.display());
        }
    }
}

/// The key of the game owning `hwnd`.
fn game_key(hwnd: usize) -> Option<String> {
    let pid = super::start::window_owner(hwnd)?;
    key(&crate::image_path::exe_path(pid)?)
}

/// Whether a window capture of `hwnd` starts on WGC: this run switched, or the game is
/// remembered from an earlier run.
pub(crate) fn starts_on_wgc(shared: &CaptureShared, hwnd: usize) -> bool {
    if shared.fullscreen_wgc.load(Relaxed) {
        return true;
    }
    let remembered = game_key(hwnd).is_some_and(|k| file().is_some_and(|p| read(&p).contains(&k)));
    if remembered {
        shared.fullscreen_wgc.store(true, Relaxed);
    }
    remembered
}

/// The shell says a Direct3D app holds the display in exclusive fullscreen.
pub(crate) fn exclusive_now() -> bool {
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHQueryUserNotificationState(state: *mut i32) -> i32;
    }
    const QUNS_RUNNING_D3D_FULL_SCREEN: i32 = 3;
    let mut state = 0i32;
    let hr = unsafe { SHQueryUserNotificationState(&mut state) };
    hr == 0 && state == QUNS_RUNNING_D3D_FULL_SCREEN
}

/// Whether the foreground window belongs to the process owning `hwnd`.
pub(crate) fn game_in_front(hwnd: usize) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    let Some(game) = super::start::window_owner(hwnd) else { return false };
    let mut front = 0u32;
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut front)) };
    front != 0 && front == game
}

/// Desktop Duplication following `hwnd` saw exclusive fullscreen long enough: remember the
/// game, mark the run, and ask for the restart (it lands on WGC window capture).
pub(crate) fn hand_to_wgc(shared: &CaptureShared, hwnd: usize) {
    eprintln!("[recall] capture: the game runs in exclusive fullscreen; window capture keeps Recall's island visible, capture restarts on WGC");
    shared.fullscreen_wgc.store(true, Relaxed);
    if let Some(k) = game_key(hwnd) {
        remember(&k, true);
    }
    shared.restart.store(true, Relaxed);
}

/// A WGC window capture of a game that moved here for exclusive fullscreen: watches for the
/// game leaving it (from the frame callback, every [`CHECK_EVERY`]).
#[derive(Debug)]
pub(crate) struct BackWatch {
    hwnd: usize,
    watch: Watch,
    last: Option<Instant>,
    done: bool,
}

impl BackWatch {
    /// Only for a window capture that started on WGC because of exclusive fullscreen, while
    /// Desktop Duplication is wanted (else going back would land on WGC again).
    pub(crate) fn new(shared: &CaptureShared) -> Option<Self> {
        let hwnd = shared.window.load(Relaxed);
        let applies = hwnd != 0 && shared.fullscreen_wgc.load(Relaxed) && shared.dda_wanted().is_ok();
        applies.then_some(Self { hwnd, watch: Watch::default(), last: None, done: false })
    }

    pub(crate) fn tick(&mut self, shared: &CaptureShared, now: Instant) {
        if self.done || self.last.is_some_and(|t| now.saturating_duration_since(t) < CHECK_EVERY) {
            return;
        }
        self.last = Some(now);
        let seen = look(game_in_front(self.hwnd), exclusive_now(), false);
        if !self.watch.update(seen, now, LEAVE_AFTER) {
            return;
        }
        self.done = true;
        eprintln!("[recall] capture: the game left exclusive fullscreen; capture restarts on desktop duplication");
        shared.fullscreen_wgc.store(false, Relaxed);
        if let Some(k) = game_key(self.hwnd) {
            remember(&k, false);
        }
        shared.restart.store(true, Relaxed);
    }
}

#[cfg(test)]
#[path = "fullscreen_switch_tests.rs"]
mod tests;
