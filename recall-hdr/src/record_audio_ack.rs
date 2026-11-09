//! Re-send what the engine did not apply (design-audio-mute P13). Every audio line carries a
//! sequence number; the engine reports the last one it applied (`status.audio_seq`). Behind for
//! 2 s: the whole audio intent is queued again (a line dropped by a full queue, or one the
//! engine never read). Still behind 2 s later, the engine does not read its stdin, so nothing it
//! holds can be trusted, and it gets a recovery restart, but only (N2, N5):
//! - never for an engine whose stdin ended (`status.stdin_ended`): it already holds every
//!   device track and is ending; a kill would only lose its save;
//! - never while a save or a long recording is in flight: the intent is restated again and the
//!   watch waits (a kill would cut the clip; the engine's own end-of-stdin hold covers a dead
//!   pipe);
//! - at most once per [`RECOVER_EVERY`].
//!
//! An engine whose status has no `audio_seq` (below the protocol floor) is not watched.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::record_app::Controller;

/// How long the engine may stay behind before each step.
pub const BEHIND_FOR: Duration = Duration::from_secs(2);
/// The fewest time between two ack recoveries (N5: no restart loop on a systematic failure).
pub const RECOVER_EVERY: Duration = Duration::from_secs(10 * 60);

/// What a status asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckStep {
    None,
    /// Queue the whole audio intent again (`Supervisor::restate_audio`).
    Restate,
    /// A recovery restart (`restart_for_recovery`).
    Recover,
}

/// What the engine's status and the controller say right now.
#[derive(Debug, Clone, Copy, Default)]
pub struct AckState {
    /// `status.audio_seq` (None: the engine does not say).
    pub acked: Option<u64>,
    /// The last audio line sent (`Supervisor::last_audio_seq`).
    pub sent: u64,
    /// `status.stdin_ended`.
    pub stdin_ended: bool,
    /// A save or a long recording is in flight (`EngineActivity::busy`).
    pub busy: bool,
    /// A line was dropped and no whole restatement is queued since (F-1).
    pub dirty: bool,
}

/// The watch on one engine's acks.
#[derive(Debug, Default)]
pub struct AudioAck {
    behind_since: Option<Instant>,
    restated: bool,
    last_recover: Option<Instant>,
}

impl AudioAck {
    /// A status at `now`.
    pub fn on_status(&mut self, st: AckState, now: Instant) -> AckStep {
        let behind = !st.stdin_ended && st.acked.is_some_and(|acked| acked < st.sent || st.dirty);
        if !behind {
            self.behind_since = None;
            self.restated = false;
            return AckStep::None;
        }
        let since = *self.behind_since.get_or_insert(now);
        if now.saturating_duration_since(since) < BEHIND_FOR {
            return AckStep::None;
        }
        self.behind_since = Some(now);
        let recent = self.last_recover.is_some_and(|at| now.saturating_duration_since(at) < RECOVER_EVERY);
        if !self.restated || st.busy || recent {
            self.restated = true;
            return AckStep::Restate; // the safe state: say it again, never kill a clip
        }
        self.restated = false;
        self.last_recover = Some(now);
        AckStep::Recover
    }

    /// The engine exited: a fresh watch (the recovery backoff stays).
    fn engine_exited(&mut self) {
        self.behind_since = None;
        self.restated = false;
    }
}

/// The controller's watch (one controller per process).
static WATCH: std::sync::Mutex<AudioAck> = std::sync::Mutex::new(AudioAck { behind_since: None, restated: false, last_recover: None });

/// Every engine event (`drain_engine_events`): a `status` is checked, an exit forgets the watch.
pub(crate) fn on_engine_event(ctl: &mut Controller, ev: &Value, now: Instant) {
    let mut watch = WATCH.lock().unwrap_or_else(|p| p.into_inner());
    match ev.get("type").and_then(Value::as_str) {
        Some("status") => {}
        Some("audio_gap") => {
            drop(watch);
            crate::log!("record: the engine skipped an audio line; restating them");
            let _ = ctl.supervisor.restate_audio(); // F-1: at once, not after 2 s
            return;
        }
        Some(crate::supervisor::ENGINE_EXITED) => return watch.engine_exited(),
        _ => return,
    }
    let st = AckState {
        acked: ev.get("audio_seq").and_then(Value::as_u64),
        sent: ctl.supervisor.last_audio_seq(),
        stdin_ended: ev.get("stdin_ended").and_then(Value::as_bool) == Some(true),
        busy: ctl.activity.busy(now),
        dirty: ctl.supervisor.audio_dirty(),
    };
    let step = watch.on_status(st, now);
    drop(watch);
    match step {
        AckStep::None => {}
        AckStep::Restate => {
            crate::log!("record: the engine has not applied every audio line for {BEHIND_FOR:?}; restating them");
            ctl.supervisor.restate_audio();
        }
        AckStep::Recover => {
            crate::log!("record: the engine still has not applied its audio lines; restarting it");
            ctl.recovery_restart(now); // the encoder path's recovery (record_fail first), F-5
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(acked: u64, sent: u64) -> AckState {
        AckState { acked: Some(acked), sent, ..AckState::default() }
    }

    /// P13: a line dropped (a full queue): restated after 2 s behind, then a recovery restart
    /// after 2 s more; caught up in time: nothing. Fake clock.
    #[test]
    fn a_dropped_line_is_restated_then_recovered() {
        let t0 = Instant::now();
        let mut ack = AudioAck::default();
        assert_eq!(ack.on_status(st(4, 5), t0), AckStep::None, "behind, not for long");
        assert_eq!(ack.on_status(st(4, 5), t0 + Duration::from_millis(1999)), AckStep::None);
        assert_eq!(ack.on_status(st(4, 5), t0 + BEHIND_FOR), AckStep::Restate);
        assert_eq!(ack.on_status(st(4, 9), t0 + Duration::from_secs(3)), AckStep::None, "the restatement's 2 s");
        assert_eq!(ack.on_status(st(4, 9), t0 + Duration::from_secs(4)), AckStep::Recover);
        let mut ok = AudioAck::default();
        ok.on_status(st(4, 5), t0);
        assert_eq!(ok.on_status(st(5, 5), t0 + Duration::from_secs(9)), AckStep::None, "caught up");
        assert_eq!(ok.on_status(AckState { sent: 5, ..AckState::default() }, t0 + Duration::from_secs(20)), AckStep::None, "an engine that does not say");
        let actions = include_str!("record_actions.rs").split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(actions.contains("crate::record_audio_ack::on_engine_event(self, &ev, now);"), "the engine-event loop runs it");
    }

    /// F-1: line 10 dropped by a full queue, line 11 queued and acked. The ack equals the last
    /// sent, but the drop leaves the intent dirty: restated after 2 s, never left as is.
    #[test]
    fn a_dropped_line_masked_by_a_later_one_is_still_restated() {
        let t0 = Instant::now();
        let mut ack = AudioAck::default();
        let masked = AckState { dirty: true, ..st(11, 11) };
        assert_eq!(ack.on_status(masked, t0), AckStep::None);
        assert_eq!(ack.on_status(masked, t0 + BEHIND_FOR), AckStep::Restate, "the gap is not masked");
        let src = include_str!("record_audio_ack.rs").split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(src.contains("dirty: ctl.supervisor.audio_dirty(),"), "the live event feeds it");
        assert!(crate::supervisor::FORWARDED_EVENTS.contains(&"audio_gap"), "the engine's gap reaches the watch");
    }

    /// N2: an orphan whose stdin ended (it holds its tracks and may write a long save) is never
    /// recovered; nor is any engine while a save or long recording is in flight (restated
    /// instead, every 2 s); and N5: one recovery per 10 min.
    #[test]
    fn no_recovery_for_an_ended_stdin_a_clip_in_flight_or_twice_in_10_min() {
        let t0 = Instant::now();
        let s = |n: u64| t0 + Duration::from_secs(n);
        let mut ended = AudioAck::default();
        let gone = AckState { stdin_ended: true, ..st(4, 9) };
        assert!((0..60).all(|n| ended.on_status(gone, s(n)) == AckStep::None), "stdin ended: never");
        let mut busy = AudioAck::default();
        let saving = AckState { busy: true, ..st(4, 9) };
        let steps: Vec<_> = (0..12).map(|n| busy.on_status(saving, s(n))).collect();
        assert!(!steps.contains(&AckStep::Recover), "a save in flight: never killed {steps:?}");
        assert!(steps.contains(&AckStep::Restate), "it is restated instead");
        let mut twice = AudioAck::default();
        let mut recovers = 0;
        for n in 0..300 {
            if twice.on_status(st(4, 9), s(n)) == AckStep::Recover {
                recovers += 1;
            }
        }
        assert_eq!(recovers, 1, "at most one recovery in 10 min");
        let src = include_str!("record_audio_ack.rs").split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(src.contains("stdin_ended: ev.get(\"stdin_ended\").and_then(Value::as_bool) == Some(true), busy: ctl.activity.busy(now),"), "the live event feeds both");
        // F-5: the ack recovery takes the encoder path's recovery (record_fail, the end-early wait).
        assert!(src.contains("ctl.recovery_restart(now);"), "the ack watch recovers through it");
        assert!(!src.contains("ctl.restart_for_recovery()"), "never the bare restart");
        let recover = include_str!("record_recover.rs").split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(recover.contains("Recovery::Restart => { log!(\"record: the encoder failed, its recovery ran past its bound, or the engine hung; restarting the engine (intended)\"); self.recovery_restart(now); }"), "the encoder path uses the same one");
    }
}
