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
/// The engine `hello` capability: a released engine sends `save_progress` at once and on every
/// drain poll and answers its own give-up (`recall` `Outbound::Hello::drain_heartbeat`).
pub(super) const DRAIN_HEARTBEAT_CAP: &str = "drain_heartbeat";
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

    /// The running engine sends drain heartbeats (design P7): the controller then never settles
    /// it on a save-stall timer; the engine judges its own saves.
    pub fn engine_takes_drain_heartbeat(&self) -> bool {
        self.hello_says(DRAIN_HEARTBEAT_CAP)
    }

    /// A game switch whose old engine is released (see the module notes), then `spec` runs
    /// (`None`: no engine follows).
    pub fn release(&self, spec: Option<EngineSpec>) {
        self.shared.set_desired_how(spec, Sent::Switching, true);
    }

    /// The controller has no save in flight (R2-detect-r10-1): a released engine still running
    /// gets the plain quit grace from now, so a wedged one is killed 8 s after its saves
    /// landed (or after `SAVE_STALE` gave up on them), not 30 min later. Cheap; every tick.
    pub fn release_settled(&self) {
        let release = self.shared.release_gen.load(Ordering::SeqCst);
        self.shared.settled_gen.store(release, Ordering::SeqCst);
    }
}

impl Shared {
    /// A release was asked after the engine of generation `gen` was spawned.
    pub(super) fn release_asked(&self, gen: u64) -> bool {
        self.release_gen.load(Ordering::SeqCst) > gen
    }

    /// The release of generation `release` was settled (`Supervisor::release_settled` after it).
    pub(super) fn release_is_settled(&self, release: u64) -> bool {
        release > 0 && self.settled_gen.load(Ordering::SeqCst) >= release
    }

    /// `stop_capture` takes the quit slot; from here on every line is refused with the last
    /// retarget's reason (read under the `desired` lock, as `send_retarget_quit`). A shutdown
    /// already under way (its stop flag, read under the stdin lock as in `send_quit`) turns a
    /// game switch's reason into "isn't responding" (`at_shutdown`).
    pub(super) fn send_release_quit(&self) -> bool {
        let g = self.desired.lock().unwrap_or_else(|p| p.into_inner());
        let mut stdin = self.child_stdin.lock().unwrap_or_else(|p| p.into_inner());
        stdin.quitting = Some(if self.stop_requested() { super::refusal::at_shutdown(g.2) } else { g.2 });
        stdin.queue.as_ref().is_some_and(|queue| queue.send_quit(STOP_CAPTURE_LINE))
    }
}

#[cfg(test)]
mod tests {
    use super::super::quit_tests::Recorded;
    use super::*;
    use std::sync::Arc;

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
        shared.set_child_stdin(None, None);
        let lines = rec.lines();
        assert_eq!(lines, vec![STOP_CAPTURE_LINE.to_string()], "stop_capture, no quit, nothing after");
    }

    #[test]
    fn a_settled_release_is_killed_after_the_quit_grace_not_30_min() {
        // R2-detect-r10-1 (b): a wedged released engine no longer holds the slot for 30 min.
        let shared = Shared::new(None);
        shared.set_desired_how(None, Sent::Switching, true);
        let release = shared.release_gen.load(Ordering::SeqCst);
        assert!(!shared.release_is_settled(release), "a save still in flight");
        let sup = Supervisor { shared: Arc::new(shared), handle: None };
        sup.release_settled();
        assert!(sup.shared.release_is_settled(release));
        // The kill moment itself: `supervisor_release_wait` (design P10).
        sup.shared.set_desired_how(None, Sent::Switching, true);
        assert!(!sup.shared.release_is_settled(sup.shared.release_gen.load(Ordering::SeqCst)), "a newer release waits for its own saves");
    }

    /// A shutdown landing between a game switch's release and its `stop_capture` (the stop
    /// flag set, no quit window open yet) is no game switch: lines say "isn't responding".
    /// Run on a thread with a 5 s bound, so a lock taken twice fails the test, never hangs it.
    #[test]
    fn a_shutdown_before_the_release_quit_says_isnt_responding() {
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let shared = Shared::new(None);
            let _rec = Recorded::attach(&shared);
            shared.set_desired_how(Some(EngineSpec::default()), Sent::Switching, true);
            shared.request_stop(Duration::ZERO); // no quit window yet: nothing to turn
            assert!(shared.send_release_quit());
            let _ = done.send(shared.send_line(r#"{"cmd":"save_clip"}"#));
        });
        let sent = finished.recv_timeout(Duration::from_secs(5)).expect("the release quit did not finish in 5 s");
        assert_eq!(sent, Sent::NotResponding, "shutdown is no game switch");
    }

    #[test]
    fn release_waits_past_the_engines_own_save_bound() {
        assert!(RELEASE_GRACE > Duration::from_secs(30 * 60), "the engine's orphan budget, plus its exit");
    }
    /// The engine's line as the reader hands it on: forwarded to the controller's channel.
    fn forwarded(shared: &Shared, rx: &std::sync::mpsc::Receiver<serde_json::Value>, line: serde_json::Value) -> Option<serde_json::Value> {
        shared.forward_event(&line);
        rx.try_recv().ok()
    }

    /// R2-detect-r10-p3-2: a draining engine's `save_progress` reaches the controller
    /// (dropped from FORWARDED_EVENTS, the -p2 P1 comes back: the save is killed at ~128 s).
    #[test]
    fn a_draining_engines_save_progress_is_forwarded_to_the_controller() {
        let shared = Shared::new(None);
        let (tx, rx) = std::sync::mpsc::channel();
        *shared.events.lock().unwrap() = Some(tx);
        let ev = serde_json::json!({"type": "save_progress", "save_writing": {"bytes": 1, "queued": 0, "step": 1}});
        assert_eq!(forwarded(&shared, &rx, ev.clone()), Some(ev));
    }

    /// R2-detect-r10-p3-2, the live path end to end: engine line -> `forward_event` ->
    /// `EngineActivity::on_engine_event` -> `retarget_busy` (as `plan_engine` reads it every
    /// tick) -> `release_may_settle`. 15 min of progress (past the 120 s window and the 10 min
    /// cap) never settles; a 120 s stall does.
    #[test]
    fn a_forwarded_drain_keeps_the_release_unsettled_until_it_stalls() {
        use crate::record_activity::EngineActivity;
        let shared = Shared::new(None);
        let (tx, rx) = std::sync::mpsc::channel();
        *shared.events.lock().unwrap() = Some(tx);
        let t0 = crate::awake_clock::Awake::ZERO;
        let mut activity = EngineActivity::default();
        activity.save_sent(t0);
        let mut now = t0;
        for step in 1..=180u64 {
            now = t0 + Duration::from_secs(step * 5);
            let line = serde_json::json!({"type": "save_progress", "save_writing": {"bytes": step, "queued": 0, "step": step}});
            let ev = forwarded(&shared, &rx, line).expect("forwarded");
            activity.on_engine_event(&ev, 0.0, now);
            let _ = activity.retarget_busy(now); // plan_engine's read: expiry and the cap run here
            assert!(!activity.release_may_settle(now), "settled at {} s while the save moved", step * 5);
        }
        let _ = activity.retarget_busy(now + Duration::from_secs(135));
        assert!(activity.release_may_settle(now + Duration::from_secs(135)), "135 s with no progress settles (no heartbeats, P8)");
    }
}
