//! Stdin handlers for the hotkey features: `bookmark`, `screenshot`,
//! `record_start` / `record_stop` / `record_toggle`, `hud_probe`. Built once in
//! `App::resumed`; the render thread fulfils screenshots (`App::screenshot_tick`).

use std::sync::{Arc, Mutex};

use crate::bookmarks::Bookmarks;
use crate::capture::CaptureShared;
use crate::protocol::{emit_or_log, Command, Outbound};
use crate::screenshot::Screenshots;

pub struct Extras {
    pub shared: Arc<CaptureShared>,
    /// Current source name (for screenshot / recording file names).
    pub source: Arc<Mutex<String>>,
    pub bookmarks: Arc<Bookmarks>,
    pub shots: Arc<Screenshots>,
    /// The island's HUD probe (one at a time, on its own background thread).
    pub hud: crate::hud_probe::HudProbe,
    #[cfg(feature = "encode")]
    pub longrec: Option<Arc<crate::longrec::LongRec>>,
    pub json: bool,
}

impl Extras {
    /// Handle a hotkey-feature command; `false` = not one of ours.
    pub fn handle(&self, cmd: &Command) -> bool {
        match cmd {
            Command::Bookmark => self.bookmark(),
            Command::Screenshot => self.shots.request(),
            Command::HudProbe => self.hud.request(),
            Command::RecordStart | Command::RecordStop | Command::RecordToggle => self.record(cmd),
            _ => return false,
        }
        true
    }

    fn source(&self) -> String {
        self.source.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn bookmark(&self) {
        let Some(at_us) = self.shared.recording_now_us() else {
            return self.log("bookmark ignored: nothing has been captured yet");
        };
        self.bookmarks.add(at_us);
        #[cfg(feature = "encode")]
        if let Some(lr) = &self.longrec {
            lr.mark(at_us);
        }
        let at_secs = at_us as f64 / 1_000_000.0;
        emit_or_log(self.json, &Outbound::Bookmarked { at_secs }, || format!("bookmark at {at_secs:.3}s"));
    }

    #[cfg(feature = "encode")]
    fn record(&self, cmd: &Command) {
        let Some(lr) = &self.longrec else {
            return self.record_unavailable();
        };
        // Capture time + the instant it was read, sampled together here: the
        // writer thread may pick the command up much later (HDD spin-up).
        let start = self.shared.recording_now();
        match cmd {
            Command::RecordStart => lr.start(&self.source(), start),
            Command::RecordStop => lr.stop(),
            _ => lr.toggle(&self.source(), start),
        }
    }

    #[cfg(not(feature = "encode"))]
    fn record(&self, _cmd: &Command) {
        self.record_unavailable();
    }

    fn record_unavailable(&self) {
        let reason = "engine built without the encode feature".to_string();
        emit_or_log(self.json, &Outbound::RecordingFailed { reason: reason.clone(), path: None, paths: Vec::new(), gap_seconds: 0.0, frozen_seconds: 0.0 }, || reason);
    }

    fn log(&self, message: &str) {
        emit_or_log(self.json, &Outbound::EncoderLog { message: message.into() }, || message.into());
    }
}
