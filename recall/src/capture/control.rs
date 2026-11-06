//! The running capture, WGC or Desktop Duplication, behind one handle for the app.

use super::dda::DdaControl;
use super::WgcControl;

/// Keeps the capture thread alive; held by the app. Drop or `stop` ends it.
pub enum CaptureControlHandle {
    Wgc(WgcControl),
    Dda(DdaControl),
}

impl CaptureControlHandle {
    pub fn stop(self) -> Result<(), String> {
        match self {
            Self::Wgc(control) => control.stop().map_err(|e| e.to_string()),
            Self::Dda(control) => control.stop(),
        }
    }

    /// The WGC capture thread has ended (the handler returned an error, or the window
    /// closed). Desktop Duplication asks for its own restarts (`restart_reason`).
    pub fn is_finished(&self) -> bool {
        match self {
            Self::Wgc(control) => control.is_finished(),
            Self::Dda(_) => false,
        }
    }
}
