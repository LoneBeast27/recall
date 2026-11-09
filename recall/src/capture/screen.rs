//! Whether the captured target can send frames now (`capture_watchdog`): a minimized game,
//! the lock screen (UAC, Ctrl+Alt+Del) and a display that power management turned off send
//! none, and a capture started then fails or gets nothing until the user is back. Reuses
//! Desktop Duplication's desktop check (`dda_desktop`).

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::IsIconic;

/// One look at the target and the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Screen {
    pub iconic: bool,
    /// The input desktop is not the user's (lock screen, UAC, Ctrl+Alt+Del).
    pub locked: bool,
    pub display_off: bool,
}

impl Screen {
    /// Nothing can be captured now; a restart would only fail or wait.
    pub fn away(self) -> bool {
        self.iconic || self.locked || self.display_off
    }
}

/// The screen now, and the window `hwnd` (none for a monitor capture).
pub fn now(hwnd: Option<usize>) -> Screen {
    let desktop = super::dda_desktop::now();
    // SAFETY: IsIconic tolerates a stale handle.
    let iconic = hwnd.is_some_and(|h| unsafe { IsIconic(HWND(h as *mut _)) }.as_bool());
    Screen { iconic, locked: !desktop.input_is_users, display_off: !desktop.display_on }
}
