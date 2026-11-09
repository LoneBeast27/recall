//! `transitions_sim::Sim`'s proof rule and registration changes (split out of
//! `transitions_sim`, file size): what `Controller::classify`, `poll_tracker`'s render
//! check and `recheck_registration` do, with injected verdicts and probes.

use std::time::Instant;

use super::Sim;
use crate::config::Config;
use crate::record_hosted::{check_proof, Checked};
use crate::record_registry::registrations_changed;
use crate::record_target::{Admit, Target};
use crate::record_transition::Transitions;
use crate::watcher::WatchEvent;

impl Sim {
    /// `apply_config_patch` -> `recheck_registration`: the new config applies, and a
    /// registration change re-checks the live session (`end_for_registration`, probing its
    /// window = the last foreground seen of that window unless [`Sim::probe_fails`]), then
    /// reads the foreground again (`probe_foreground` with `last_fed` cleared).
    pub fn set_config(&mut self, cfg: Config) {
        let old = std::mem::replace(&mut self.cfg, cfg);
        if !registrations_changed(&old, &self.cfg) {
            return;
        }
        self.last_fed = None;
        let session = self.tracker.current().map(|t| (t.pid, t.hwnd));
        let probed = self.foreground.clone().filter(|e| !self.probe_fails && Some((e.pid, e.hwnd)) == session);
        if self.end_for_registration(&old, probed.as_ref()) {
            if let Some(ev) = self.foreground.clone() {
                self.seen(ev);
            }
        }
    }

    /// `Controller::classify`: the proof rule (no vendor list in the matrix).
    pub(super) fn classify_with_proof(&mut self, ev: &WatchEvent) -> Option<Target> {
        match check_proof(ev, &self.cfg, self.can_check, || None) {
            Checked::Candidate(c) => {
                self.checked = c.needs_check.then_some((ev.pid, ev.hwnd));
                Some(c.target)
            }
            Checked::NoVerdict | Checked::No => None,
        }
    }

    /// `poll_tracker`: a candidate that needs the render check commits on a "game" verdict;
    /// [`Sim::not_games`] answer "not a game".
    pub(super) fn poll_with_verdicts(&mut self, now: Instant) {
        let (checked, not_games) = (self.checked, &self.not_games);
        self.tracker.poll_admit(now, |t| {
            let rejected = checked == Some((t.pid, t.hwnd)) && not_games.iter().any(|g| t.exe.eq_ignore_ascii_case(&format!("{g}.exe")));
            if rejected { Admit::Reject } else { Admit::Commit }
        });
    }
}
