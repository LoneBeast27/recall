//! The privacy stop's release (final review F-detect-1, option b): the old engine is sent
//! `{"cmd":"stop_capture"}` in place of `quit`. It stops capturing at once (no frame or sample
//! enters its ring after the line), reads nothing more (no save can start), and exits once the
//! saves already asked for are written, within the engine's own orphan bounds (30 min at most,
//! while a save makes progress). The next engine starts after it exits: the engine slot
//! (`Local\Recall.Engine`) admits one recorder at a time, so it waits like any retarget. Lines
//! sent meanwhile are refused as for a game switch. An engine whose `hello` lacks
//! [`STOP_CAPTURE_CAP`] is never sent it (the controller holds the stop instead).

use std::sync::atomic::Ordering;
use std::time::Duration;

use super::{Shared, Supervisor, QUIT_GRACE};
use crate::engine_policy::EngineSpec;
use crate::record_dispatch::Sent;

/// The engine `hello` capability (`recall` `protocol::Outbound::Hello::stop_capture`).
pub(super) const STOP_CAPTURE_CAP: &str = "stop_capture";
/// The line sent in the quit slot.
pub(super) const STOP_CAPTURE_LINE: &str = r#"{"cmd":"stop_capture"}"#;
/// How long a released engine may run on: the engine's own cap for finishing saves
/// (`recall` `ORPHAN_SAVE_BUDGET`, 30 min) plus the quit grace. A supervisor stop (app quit)
/// cuts it to the stop's own grace.
pub(super) const RELEASE_GRACE: Duration = Duration::from_secs(30 * 60).saturating_add(QUIT_GRACE);

impl Supervisor {
    /// The running engine said it takes `stop_capture`.
    pub fn engine_takes_stop_capture(&self) -> bool {
        self.hello_says(STOP_CAPTURE_CAP)
    }

    /// A game switch whose old engine is released (see the module notes), then `spec` runs
    /// (`None`: no engine follows).
    pub fn release(&self, spec: Option<EngineSpec>) {
        self.shared.set_desired_how(spec, Sent::Switching, true);
    }
}

impl Shared {
    /// A release was asked after the engine of generation `gen` was spawned.
    pub(super) fn release_asked(&self, gen: u64) -> bool {
        self.release_gen.load(Ordering::SeqCst) > gen
    }

    /// `stop_capture` takes the quit slot; from here on every line is refused with the last
    /// retarget's reason (read under the `desired` lock, as `send_retarget_quit`).
    pub(super) fn send_release_quit(&self) -> bool {
        let g = self.desired.lock().unwrap_or_else(|p| p.into_inner());
        let mut stdin = self.child_stdin.lock().unwrap_or_else(|p| p.into_inner());
        stdin.quitting = Some(g.2);
        stdin.queue.as_ref().is_some_and(|queue| queue.send_quit(STOP_CAPTURE_LINE))
    }
}

#[cfg(test)]
mod tests {
    use super::super::quit_tests::Recorded;
    use super::*;

    #[test]
    fn a_release_sends_stop_capture_in_place_of_quit_and_refuses_later_lines() {
        let shared = Shared::new(None);
        let rec = Recorded::attach(&shared);
        assert!(!shared.release_asked(0), "nothing asked yet");
        shared.set_desired_how(None, Sent::Switching, false);
        assert!(!shared.release_asked(0), "a plain retarget is no release");
        shared.set_desired_how(None, Sent::Switching, true);
        assert!(shared.release_asked(1), "asked after the engine of generation 1 spawned");
        assert!(!shared.release_asked(2), "an engine spawned for it is not released");
        assert!(shared.send_release_quit());
        assert_eq!(shared.send_line(r#"{"cmd":"save_clip"}"#), Sent::Waiting, "no save after it (a stop: no game follows)");
        shared.set_child_stdin(None);
        let lines = rec.lines();
        assert_eq!(lines, vec![STOP_CAPTURE_LINE.to_string()], "stop_capture, no quit, nothing after");
    }

    #[test]
    fn release_waits_past_the_engines_own_save_bound() {
        assert!(RELEASE_GRACE > Duration::from_secs(30 * 60), "the engine's orphan budget, plus its exit");
    }
}
