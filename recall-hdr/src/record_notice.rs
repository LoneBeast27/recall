//! Recall Island notices (spec `.planning/ISLAND_SPEC.md`, "Build contract"):
//! one `session` event, plus the soft tick, each time what Recall records
//! changes -- a game picked up, desktop recording, paused, waiting for a game
//! (stopped), an encoder problem (the failure tone). The overlay shows it in
//! the island for 3 s.
//!
//! ```json
//! {"type":"notice","kind":"game|desktop|paused|waiting|encoder","title":S,"detail":S}
//! ```
//!
//! Rules: `starting` is transitional and says nothing, so a settings restart
//! that comes back to the same game is silent; the first situation after
//! controller start-up says nothing when it is "waiting"; the same situation
//! is never announced twice within [`REPEAT_GAP`] (a flapping target). With
//! `overlay.enabled` off the tracker still follows the state but nothing is
//! sent or heard (`record_cmds::announce`).

use std::time::{Duration, Instant};

use serde_json::json;

use crate::record_engine::EngineTarget;
use crate::record_target::DESKTOP_NAME;

/// No repeat notice for the same situation within this long.
pub const REPEAT_GAP: Duration = Duration::from_secs(5);

/// What the island tells the player about recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Situation {
    /// Recording this game (its display name).
    Game(String),
    Desktop,
    Paused,
    /// No target: not recording, waiting for a game.
    Waiting,
    /// The game closed; its last clip stays saveable this long (`record_drain`).
    /// The end of the drain ("waiting") is silent: nothing new happened.
    Draining { secs: u64 },
    /// The encoder failed; `gave_up` = the engine crashed repeatedly.
    Encoder { gave_up: bool },
}

impl Situation {
    /// The situation a session `state` (see `record_status::session_state`)
    /// and target describe; `None` while starting.
    pub fn of(state: &str, wanted: &EngineTarget, gave_up: bool) -> Option<Situation> {
        match state {
            "paused" => Some(Situation::Paused),
            "waiting" => Some(Situation::Waiting),
            // The caller knows the drain's length (`Controller::announce`).
            "draining" => None,
            "error" => Some(Situation::Encoder { gave_up }),
            "recording" => Some(match wanted {
                EngineTarget::Window(t) => Situation::Game(t.name.clone()),
                EngineTarget::Desktop => Situation::Desktop,
                EngineTarget::Off => Situation::Waiting,
            }),
            _ => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Situation::Game(_) => "game",
            Situation::Desktop => "desktop",
            Situation::Paused => "paused",
            Situation::Waiting | Situation::Draining { .. } => "waiting",
            Situation::Encoder { .. } => "encoder",
        }
    }

    pub fn title(&self) -> &'static str {
        match self {
            Situation::Game(_) | Situation::Desktop => "Recording",
            Situation::Paused => "Recording paused",
            Situation::Waiting => "Waiting for a game",
            Situation::Draining { .. } => "Game closed",
            Situation::Encoder { gave_up: false } => "Encoder problem",
            Situation::Encoder { gave_up: true } => "Recorder stopped",
        }
    }

    pub fn detail(&self) -> String {
        match self {
            Situation::Game(name) => name.clone(),
            Situation::Desktop => DESKTOP_NAME.into(),
            Situation::Paused => "Resume from the tray or the app".into(),
            Situation::Waiting => "Not recording".into(),
            Situation::Draining { secs } => format!("Last clip kept for {}", crate::record_drain::clock(*secs)),
            Situation::Encoder { gave_up: false } => "Clips are not being recorded".into(),
            Situation::Encoder { gave_up: true } => "It failed repeatedly (see recall-hdr.log)".into(),
        }
    }

    /// Announced with the failure tone instead of the tick.
    pub fn is_failure(&self) -> bool {
        matches!(self, Situation::Encoder { .. })
    }
}

/// The `session` `notice` event body.
pub fn notice_json(s: &Situation) -> String {
    json!({"type": "notice", "kind": s.kind(), "title": s.title(), "detail": s.detail()}).to_string()
}

/// Follows the session situation and decides which changes are announced.
#[derive(Debug, Default)]
pub struct NoticeTracker {
    /// The last situation seen (`None` until the first one after start-up).
    current: Option<Situation>,
    /// Situations announced within the last [`REPEAT_GAP`].
    recent: Vec<(Situation, Instant)>,
}

impl NoticeTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed the latest situation (`None` = starting); returns the one to
    /// announce now, if any.
    pub fn observe(&mut self, next: Option<Situation>, now: Instant) -> Option<Situation> {
        let next = next?;
        if self.current.as_ref() == Some(&next) {
            return None;
        }
        let first = self.current.is_none();
        let drain_ended = matches!(self.current, Some(Situation::Draining { .. }));
        self.current = Some(next.clone());
        if first && next == Situation::Waiting {
            return None; // controller start-up: nothing has changed for the player
        }
        if drain_ended && next == Situation::Waiting {
            return None; // "Game closed" already said it; the countdown ending is not news
        }
        self.recent.retain(|(_, at)| now.duration_since(*at) < REPEAT_GAP);
        if self.recent.iter().any(|(s, _)| *s == next) {
            return None;
        }
        self.recent.push((next.clone(), now));
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record_target::Target;
    use serde_json::Value;

    fn game(name: &str) -> EngineTarget {
        EngineTarget::Window(Target { hwnd: 7, pid: 42, name: name.into(), exe: format!("{name}.exe") })
    }

    /// Drive the tracker through (seconds, state, target) steps; collect the
    /// notice JSON each step produced.
    fn run(steps: &[(u64, &str, EngineTarget)]) -> Vec<Value> {
        let t0 = Instant::now();
        let mut tracker = NoticeTracker::new();
        steps
            .iter()
            .filter_map(|(secs, state, target)| {
                tracker.observe(Situation::of(state, target, false), t0 + Duration::from_secs(*secs))
            })
            .map(|s| serde_json::from_str(&notice_json(&s)).unwrap())
            .collect()
    }

    #[test]
    fn a_game_session_announces_pickup_pause_and_stop() {
        let v = run(&[
            (0, "waiting", EngineTarget::Off),
            (10, "starting", game("VALORANT")),
            (11, "recording", game("VALORANT")),
            (12, "recording", game("VALORANT")),
            (60, "paused", EngineTarget::Off),
            (90, "waiting", EngineTarget::Off),
        ]);
        assert_eq!(
            v,
            vec![
                serde_json::json!({"type":"notice","kind":"game","title":"Recording","detail":"VALORANT"}),
                serde_json::json!({"type":"notice","kind":"paused","title":"Recording paused","detail":"Resume from the tray or the app"}),
                serde_json::json!({"type":"notice","kind":"waiting","title":"Waiting for a game","detail":"Not recording"}),
            ]
        );
    }

    #[test]
    fn start_up_waiting_is_silent_but_a_running_game_at_start_up_is_announced() {
        assert!(run(&[(0, "waiting", EngineTarget::Off), (1, "waiting", EngineTarget::Off)]).is_empty());
        let v = run(&[(0, "starting", game("Apex")), (1, "recording", game("Apex"))]);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0]["detail"], "Apex");
    }

    #[test]
    fn a_settings_restart_back_to_the_same_game_is_silent() {
        let v = run(&[
            (0, "recording", game("VALORANT")),
            (100, "starting", game("VALORANT")),
            (101, "recording", game("VALORANT")),
        ]);
        assert_eq!(v.len(), 1, "{v:?}");
    }

    #[test]
    fn the_same_situation_is_not_repeated_within_five_seconds() {
        let v = run(&[
            (0, "recording", game("VALORANT")),
            (2, "waiting", EngineTarget::Off),
            (3, "recording", game("VALORANT")), // flap: VALORANT again 3 s later
            (4, "waiting", EngineTarget::Off),  // waiting again 2 s later
            (9, "recording", game("VALORANT")), // 9 s after the first: announced
        ]);
        let kinds: Vec<_> = v.iter().map(|n| n["kind"].as_str().unwrap().to_owned()).collect();
        assert_eq!(kinds, ["game", "waiting", "game"]);
    }

    #[test]
    fn desktop_and_encoder_notices() {
        let v = run(&[
            (0, "recording", EngineTarget::Desktop),
            (1, "error", game("VALORANT")),
            (20, "recording", game("VALORANT")),
        ]);
        assert_eq!(v[0], serde_json::json!({"type":"notice","kind":"desktop","title":"Recording","detail":"Desktop"}));
        assert_eq!(v[1]["kind"], "encoder");
        assert_eq!(v[1]["title"], "Encoder problem");
        assert_eq!(v[2]["detail"], "VALORANT", "recovery is announced");
        let gave_up = Situation::of("error", &EngineTarget::Off, true).unwrap();
        assert_eq!(gave_up.title(), "Recorder stopped");
        assert!(gave_up.is_failure() && !Situation::Desktop.is_failure());
        assert_eq!(Situation::of("starting", &EngineTarget::Desktop, false), None);
    }

    #[test]
    fn a_closed_game_announces_its_kept_clip_once_and_the_end_is_silent() {
        let t0 = Instant::now();
        let mut tracker = NoticeTracker::new();
        let at = |s| t0 + Duration::from_secs(s);
        assert!(tracker.observe(Situation::of("recording", &game("sobosuba"), false), at(0)).is_some());
        let closed = tracker.observe(Some(Situation::Draining { secs: 120 }), at(30)).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&notice_json(&closed)).unwrap(),
            serde_json::json!({"type":"notice","kind":"waiting","title":"Game closed","detail":"Last clip kept for 2:00"})
        );
        assert!(tracker.observe(Some(Situation::Draining { secs: 120 }), at(31)).is_none(), "said once");
        assert!(tracker.observe(Situation::of("waiting", &EngineTarget::Off, false), at(150)).is_none(), "the end is silent");
        assert_eq!(Situation::of("draining", &EngineTarget::Off, false), None);
    }

    #[test]
    fn a_different_game_is_a_new_situation() {
        let v = run(&[(0, "recording", game("VALORANT")), (1, "recording", game("Apex"))]);
        assert_eq!(v.len(), 2);
        assert_eq!(v[1]["detail"], "Apex");
    }
}
