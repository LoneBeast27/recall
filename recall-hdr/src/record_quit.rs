//! Why the record controller is shutting down, and whether that closes the Recall app.
//!
//! Only the user's tray "Quit Recall" closes the app's windows. A quit over the
//! control pipe (the installer, scripts) or a WM_QUIT leaves the app open: it keeps
//! its library and player and reconnects when a recorder comes back.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitReason {
    /// The tray menu's "Quit Recall".
    TrayMenu,
    /// `{"cmd":"quit"}` on the control pipe.
    ControlPipe,
    /// WM_QUIT reached the message pump.
    WindowsQuit,
    /// Sign-out, restart or shutdown (`WM_ENDSESSION`).
    SessionEnd,
}

impl QuitReason {
    pub fn closes_ui(self) -> bool {
        matches!(self, Self::TrayMenu)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::TrayMenu => "tray quit",
            Self::ControlPipe => "control quit",
            Self::WindowsQuit => "WM_QUIT",
            Self::SessionEnd => "session end",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::QuitReason;

    #[test]
    fn only_the_tray_quit_closes_the_app() {
        assert!(QuitReason::TrayMenu.closes_ui());
        assert!(!QuitReason::ControlPipe.closes_ui(), "an install or script quit keeps the app open");
        assert!(!QuitReason::WindowsQuit.closes_ui());
        assert!(!QuitReason::SessionEnd.closes_ui(), "Windows closes the app itself");
    }

    #[test]
    fn labels_name_the_source() {
        assert_eq!(QuitReason::TrayMenu.label(), "tray quit");
        assert_eq!(QuitReason::ControlPipe.label(), "control quit");
        assert_eq!(QuitReason::WindowsQuit.label(), "WM_QUIT");
    }
}
