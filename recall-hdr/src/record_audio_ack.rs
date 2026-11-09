//! Re-send what the engine did not apply (design-audio-mute P13). Every audio line carries a
//! sequence number; the engine reports the last one it applied (`status.audio_seq`). Behind for
//! 2 s: the whole audio intent is queued again (a line dropped by a full queue, or one the
//! engine never read). Still behind 2 s later: the engine does not read its stdin, so nothing it
//! holds can be trusted, and it gets a recovery restart. An engine whose status has no
//! `audio_seq` (below the protocol floor) is not watched.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::record_app::Controller;

/// How long the engine may stay behind before each step.
pub const BEHIND_FOR: Duration = Duration::from_secs(2);

/// What a status asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckStep {
    None,
    /// Queue the whole audio intent again (`Supervisor::restate_audio`).
    Restate,
    /// A recovery restart (`restart_for_recovery`).
    Recover,
}

/// The watch on one engine's acks.
#[derive(Debug, Default)]
pub struct AudioAck {
    behind_since: Option<Instant>,
    restated: bool,
}

impl AudioAck {
    /// A status at `now`: the engine applied up to `acked` (None: it does not say), the
    /// controller queued up to `sent`.
    pub fn on_status(&mut self, acked: Option<u64>, sent: u64, now: Instant) -> AckStep {
        let behind = acked.is_some_and(|acked| acked < sent);
        if !behind {
            *self = AudioAck::default();
            return AckStep::None;
        }
        let since = *self.behind_since.get_or_insert(now);
        if now.saturating_duration_since(since) < BEHIND_FOR {
            return AckStep::None;
        }
        if !self.restated {
            *self = AudioAck { behind_since: Some(now), restated: true };
            return AckStep::Restate;
        }
        *self = AudioAck::default();
        AckStep::Recover
    }
}

/// The controller's watch (one controller per process).
static WATCH: std::sync::Mutex<AudioAck> = std::sync::Mutex::new(AudioAck { behind_since: None, restated: false });

/// Every engine event (`drain_engine_events`): a `status` is checked, an exit forgets the watch.
pub(crate) fn on_engine_event(ctl: &mut Controller, ev: &Value, now: Instant) {
    let mut watch = WATCH.lock().unwrap_or_else(|p| p.into_inner());
    match ev.get("type").and_then(Value::as_str) {
        Some("status") => {}
        Some(crate::supervisor::ENGINE_EXITED) => return *watch = AudioAck::default(),
        _ => return,
    }
    let step = watch.on_status(ev.get("audio_seq").and_then(Value::as_u64), ctl.supervisor.last_audio_seq(), now);
    drop(watch);
    match step {
        AckStep::None => {}
        AckStep::Restate => {
            crate::log!("record: the engine has not applied every audio line for {BEHIND_FOR:?}; restating them");
            ctl.supervisor.restate_audio();
        }
        AckStep::Recover => {
            crate::log!("record: the engine still has not applied its audio lines; restarting it");
            ctl.restart_for_recovery();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P13: a line dropped (a full queue): restated after 2 s behind, then a recovery restart
    /// after 2 s more; caught up in time: nothing. Fake clock.
    #[test]
    fn a_dropped_line_is_restated_then_recovered() {
        let t0 = Instant::now();
        let mut ack = AudioAck::default();
        assert_eq!(ack.on_status(Some(4), 5, t0), AckStep::None, "behind, not for long");
        assert_eq!(ack.on_status(Some(4), 5, t0 + Duration::from_millis(1999)), AckStep::None);
        assert_eq!(ack.on_status(Some(4), 5, t0 + BEHIND_FOR), AckStep::Restate);
        assert_eq!(ack.on_status(Some(4), 9, t0 + Duration::from_secs(3)), AckStep::None, "the restatement's 2 s");
        assert_eq!(ack.on_status(Some(4), 9, t0 + Duration::from_secs(4)), AckStep::Recover);
        let mut ok = AudioAck::default();
        ok.on_status(Some(4), 5, t0);
        assert_eq!(ok.on_status(Some(5), 5, t0 + Duration::from_secs(9)), AckStep::None, "caught up");
        assert_eq!(ok.on_status(None, 5, t0 + Duration::from_secs(20)), AckStep::None, "an engine that does not say");
        let actions = include_str!("record_actions.rs").split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(actions.contains("crate::record_audio_ack::on_engine_event(self, &ev, now);"), "the engine-event loop runs it");
    }
}
