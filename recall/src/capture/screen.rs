//! Whether the captured target can send frames now (`capture_watchdog`): a minimized game,
//! the lock screen (UAC, Ctrl+Alt+Del) and a display that power management turned off send
//! none, and a capture started then fails or gets nothing until the user is back; a game
//! behind another window may send none either (exclusive fullscreen, a DDA cover). Reuses
//! Desktop Duplication's desktop check (`dda_desktop`).

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId, IsIconic};

/// One look at the target and the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Screen {
    pub iconic: bool,
    /// The input desktop is not the user's (lock screen, UAC, Ctrl+Alt+Del).
    pub locked: bool,
    pub display_off: bool,
    /// A game window is the target and its process has the foreground (false for a
    /// monitor capture).
    pub game_in_front: bool,
}

impl Screen {
    /// Nothing can be captured now; a restart would only fail or wait.
    pub fn away(self) -> bool {
        self.iconic || self.locked || self.display_off
    }

    /// The game is in front and can be seen: its capture should be delivering frames.
    pub fn watched(self) -> bool {
        self.game_in_front && !self.away()
    }
}

/// The screen now, and the window `hwnd` (none for a monitor capture).
pub fn now(hwnd: Option<usize>) -> Screen {
    let desktop = super::dda_desktop::now();
    let window = hwnd.map(|h| HWND(h as *mut _));
    // SAFETY: plain queries; each tolerates a stale handle (pid stays 0).
    let (iconic, game_in_front) = window.map_or((false, false), |w| unsafe {
        let (mut game, mut front) = (0u32, 0u32);
        GetWindowThreadProcessId(w, Some(&mut game));
        GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut front));
        (IsIconic(w).as_bool(), game != 0 && game == front)
    });
    Screen { iconic, locked: !desktop.input_is_users, display_off: !desktop.display_on, game_in_front }
}
