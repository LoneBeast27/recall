//! Pre-encode upkeep for the open clip (`services::pre_encode`): once a change to the cuts or
//! the export settings has settled, the re-encode export runs in the background, so Export
//! only copies the finished file. An untouched clip is never prepared.
use std::time::{Duration, Instant};

use super::EditorState;
use crate::services::{
    pre_encode::{self, Status},
    storage,
};

/// Quiet time after the last change before a pre-encode starts.
const SETTLE: Duration = Duration::from_millis(1500);

#[derive(Default)]
pub struct PreEncodeState {
    /// Key of the plan as opened.
    baseline: Option<u64>,
    /// The cuts or settings changed since the clip opened.
    touched: bool,
    /// Key of the current plan (re-encode only).
    current: Option<u64>,
    /// When `current` last changed, until its pre-encode starts.
    pending_since: Option<Instant>,
    /// Panel line last shown.
    shown: Option<String>,
    /// What the key was last computed from, and when: an idle tick reuses it.
    keyed: Option<(KeyInputs, Instant)>,
    /// Times the key was computed (tests check idle ticks skip it).
    key_runs: u64,
}

/// Everything the key reads that lives in the editor (the engine build is rechecked by age).
#[derive(PartialEq)]
struct KeyInputs {
    clip: (String, u64, u64),
    plan: crate::edit_model::EditPlan,
    fps: u32,
    export: crate::edit_model::ExportSettings,
}

/// A key computed from unchanged inputs is reused this long (the engine file may be replaced).
const REKEY_AFTER: Duration = Duration::from_secs(2);

impl EditorState {
    /// Per-tick: follows plan changes, starts and stops the pre-encode. True when the panel line changed.
    pub(super) fn tick_pre_encode(&mut self) -> bool {
        if !self.exporting {
            self.follow_pre_encode();
        }
        let line = self.pre_encode_line();
        if line == self.pre.shown {
            return false;
        }
        self.pre.shown = line;
        true
    }

    fn follow_pre_encode(&mut self) {
        // Not while the clip still loads: the pre-encode would read the same hard drive as the copy.
        let Some(info) = self.info.as_ref().filter(|_| self.loading_path.is_none() && !self.loading.active()) else { return };
        let inputs = KeyInputs {
            clip: (info.path.clone(), info.source_bytes, info.source_modified_ms),
            plan: self.plan.clone(),
            fps: self.fps,
            export: self.export_settings.clone(),
        };
        let fresh = self.pre.keyed.as_ref().is_some_and(|(seen, at)| *seen == inputs && at.elapsed() < REKEY_AFTER);
        let key = if fresh {
            self.pre.current
        } else {
            self.pre.key_runs += 1;
            self.pre.keyed = Some((inputs, Instant::now()));
            pre_encode::key(info, &self.plan, self.fps, &self.export_settings)
        };
        if key != self.pre.current {
            let baseline = *self.pre.baseline.get_or_insert(key.unwrap_or(0));
            self.pre.touched |= key != Some(baseline);
            self.pre.current = key;
            self.pre.pending_since = (self.pre.touched && key.is_some()).then(Instant::now);
            pre_encode::cancel_other(key);
        }
        let (Some(key), Some(since)) = (self.pre.current, self.pre.pending_since) else {
            if storage::recorder_busy() && pre_encode::running() {
                // A game started recording: stop, and prepare again once it is done.
                pre_encode::cancel_other(None);
                self.pre.pending_since = self.pre.current.map(|_| Instant::now());
            }
            return;
        };
        let settled = since.elapsed() >= SETTLE && self.drag.is_none();
        if settled && !storage::recorder_busy() && !pre_encode::running() && pre_encode::status(key) == Status::None {
            let info = info.clone();
            if pre_encode::start(key, info, self.plan.clone(), self.fps, self.export_settings.clone()) {
                self.pre.pending_since = None;
            }
        }
    }

    /// The panel line under Export, if any.
    pub fn pre_encode_line(&self) -> Option<String> {
        let key = self.pre.current?;
        match pre_encode::status(key) {
            Status::Running(Some(share)) => Some(format!("Preparing export… {}%", (share * 100.0).floor() as u32)),
            Status::Running(None) => Some("Preparing export…".into()),
            Status::Ready => Some("Ready · export is instant".into()),
            Status::Failed(error) => Some(format!("Couldn't prepare the export: {error}")),
            Status::None if self.pre.pending_since.is_some() && storage::recorder_busy() => {
                Some("Prepares the export once recording stops".into())
            }
            Status::None => None,
        }
    }

    /// The pre-encode is ready for exactly this plan.
    pub fn pre_encode_ready(&self) -> bool {
        self.pre.current.is_some_and(|key| pre_encode::status(key) == Status::Ready)
    }

    /// Clip switch or editor close: stop the pre-encode and drop its file (non-blocking).
    pub fn drop_pre_encode(&mut self) {
        self.pre = Default::default();
        pre_encode::cancel_other(None);
    }
}

#[cfg(test)]
#[path = "editor_state_pre_encode_tests.rs"]
mod tests;
