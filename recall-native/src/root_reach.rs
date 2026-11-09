//! A recorder that runs but can't be reached (audit R2-deep-startup-3): checked once a second
//! while the pipe is down, so the pill and the Start buttons say why instead of offering a Start
//! that does nothing (`services/daemon/reach.rs`).
use std::time::{Duration, Instant};

use super::{Root, ToastKind};
use crate::services::daemon::{self, RecorderReach};

const CHECK_EVERY: Duration = Duration::from_secs(1);

pub(crate) const UNREACHABLE_TITLE: &str = "Recorder can't be reached";
pub(crate) const UNREACHABLE_DETAIL: &str = "A recorder runs in this session but doesn't answer: it may run as another \
     account or as administrator, or its control link failed (see recall-hdr.log). Quit it from its tray icon, then open Recall again.";

impl Root {
    /// Updates [`Root::recorder_reach`]. True when it changed (the pill redraws).
    pub(super) fn reach_tick(&mut self) -> bool {
        let now = Instant::now();
        let refused = self.snap.refused.is_some();
        let next = if self.snap.connected || (!refused && std::env::var_os("RECALL_CONTROL_PIPE").is_some()) {
            // Connected, or a scripted check on a private pipe (the real recorder is not ours).
            self.reach_since = None;
            RecorderReach::Off
        } else {
            if self.reach_checked.is_some_and(|at| now.duration_since(at) < CHECK_EVERY) {
                return false;
            }
            self.reach_checked = Some(now);
            let running = daemon::controller_running();
            self.reach_since = if running { Some(self.reach_since.unwrap_or(now)) } else { None };
            daemon::reach_or_refused(refused, running, self.reach_since.map_or(Duration::ZERO, |since| now.duration_since(since)))
        };
        if next == self.recorder_reach {
            return false;
        }
        if next == RecorderReach::Unreachable && !refused {
            eprintln!("Recall native: a recorder runs but its control pipe does not answer");
            crate::trace::note("recorder unreachable");
        }
        self.recorder_reach = next;
        self.status_changed()
    }

    /// Why the recorder can't be reached: the owner check's refusal, or the generic reason.
    pub(crate) fn unreachable_detail(&self) -> String {
        daemon::unreachable_detail(self.snap.refused.as_deref(), UNREACHABLE_DETAIL)
    }

    /// The Start buttons: start a recorder, or say why one that runs can't be reached.
    pub(crate) fn start_recorder(&mut self) {
        if self.setup.is_some() {
            self.start_setup_recorder();
            return;
        }
        match daemon::start_controller() {
            Ok(true) => self.show_toast(ToastKind::Info, "Starting the recorder…", None),
            Ok(false) if self.recorder_reach == RecorderReach::Unreachable => {
                self.show_toast(ToastKind::Bad, UNREACHABLE_TITLE, Some(self.unreachable_detail()))
            }
            Ok(false) => self.show_toast(ToastKind::Info, "The recorder is starting…", None),
            Err(error) => self.show_toast(ToastKind::Bad, "Couldn't start the recorder", Some(error)),
        }
    }
}
