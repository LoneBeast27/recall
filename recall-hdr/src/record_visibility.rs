//! A session window that stays hidden ends the session (third audit U5).
//!
//! `check_windows` used to drop the target only when `IsWindow` failed, so a
//! game that hides its window while it saves and shuts down, or hides to the
//! tray, kept the session: no drain, a long recording writing over a frozen
//! frame, and the next game on the 2 s replace debounce instead of 300 ms.
//!
//! Now a window that is not `IsWindowVisible` for more than [`HIDDEN_FOR`]
//! counts as gone and takes the same path as a destroyed one
//! (`TargetTracker::on_window_gone_at`: the drain starts, and the process
//! stays adoptable, so a game that shows a window again is the session again).
//! A minimized window (`IsIconic`) and a window cloaked by a virtual-desktop
//! switch (`DWMWA_CLOAKED`) are kept: the player can come back to them.

use std::time::{Duration, Instant};

use crate::record_target::TargetTracker;

/// How long the session window may stay hidden before it counts as gone.
pub const HIDDEN_FOR: Duration = Duration::from_secs(1);

/// What a window looks like to the player right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    Visible,
    /// Not `IsWindowVisible`, not minimized, not cloaked.
    Hidden,
    Minimized,
    /// On another virtual desktop (or otherwise cloaked by DWM).
    Cloaked,
}

/// When the session window was first seen hidden.
#[derive(Debug, Default)]
pub struct HiddenWatch {
    since: Option<(isize, Instant)>,
}

impl HiddenWatch {
    /// One pump tick for the session window `hwnd` (None = no session).
    /// `probe` reads the window. Returns the window once it has been hidden
    /// for more than [`HIDDEN_FOR`]; any other state resets the clock.
    pub fn step(&mut self, hwnd: Option<isize>, now: Instant, probe: impl Fn(isize) -> Shown) -> Option<isize> {
        let Some(hwnd) = hwnd else {
            self.since = None;
            return None;
        };
        if probe(hwnd) != Shown::Hidden {
            self.since = None;
            return None;
        }
        let since = match self.since {
            Some((h, at)) if h == hwnd => at,
            _ => {
                self.since = Some((hwnd, now));
                now
            }
        };
        if now.duration_since(since) > HIDDEN_FOR {
            self.since = None;
            return Some(hwnd);
        }
        None
    }
}

/// End the tracker's session when its window has stayed hidden
/// ([`HiddenWatch::step`]); the same path as a destroyed window. Returns the
/// window that was dropped.
pub fn drop_hidden(watch: &mut HiddenWatch, tracker: &mut TargetTracker, now: Instant, probe: impl Fn(isize) -> Shown) -> Option<isize> {
    let hwnd = watch.step(tracker.current().map(|t| t.hwnd), now, probe)?;
    tracker.on_window_gone_at(hwnd, now).then_some(hwnd)
}

/// The live read: `IsWindowVisible` first (the common case, one call); only
/// a window that is not visible is asked `IsIconic` and `DWMWA_CLOAKED`.
pub fn shown(hwnd: isize) -> Shown {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use windows::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindowVisible};
    let h = HWND(hwnd as *mut _);
    // SAFETY: plain queries; each tolerates a stale handle.
    unsafe {
        if IsWindowVisible(h).as_bool() {
            return Shown::Visible;
        }
        if IsIconic(h).as_bool() {
            return Shown::Minimized;
        }
        let mut cloaked = 0u32;
        let read = DwmGetWindowAttribute(h, DWMWA_CLOAKED, (&raw mut cloaked).cast(), 4);
        if read.is_ok() && cloaked != 0 { Shown::Cloaked } else { Shown::Hidden }
    }
}

#[cfg(test)]
#[path = "record_visibility_tests.rs"]
mod tests;
