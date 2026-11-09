//! What the capture engine should record, and when to (re)start it.
//!
//! - [`wanted`]: an env-pinned window wins, then the auto-detected game (if
//!   `auto_record`), then the idle desktop (if `record_desktop_when_idle`).
//! - [`EnginePlanner`]: when the game goes away the engine -- and its replay
//!   ring -- is kept while the ring still holds game footage: the buffered
//!   length at that moment ([`EnginePlanner::set_drain_len`]; [`EXIT_LINGER`]
//!   when it is unknown). A save in that window still has the last clip, and
//!   the status counts it down (`draining`). Nothing new is captured: the
//!   game's window is gone. A new game retargets immediately -- unless a
//!   save is in flight: the retarget is then held ([`Step::Held`]) until the
//!   save lands (bounded by `SAVE_STALE`). A running long recording is never
//!   held on: it is stopped ([`Step::StopLongRec`]) when the drain ends or at
//!   once for a new game, and the retarget follows its stop.
//! - [`engine_env`]: the per-spawn environment the engine reads at startup.

use std::time::{Duration, Instant};

use crate::record_config::RecordingCfg;
use crate::record_target::{Target, DESKTOP_NAME};

/// How long the engine outlives its game when its buffered length is unknown.
pub const EXIT_LINGER: Duration = Duration::from_secs(30);

/// What the engine should be capturing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineTarget {
    /// No engine (state "waiting").
    Off,
    Window(Target),
    /// Primary-monitor capture named [`DESKTOP_NAME`].
    Desktop,
}

/// Why a capture action (save, bookmark, screenshot, long recording) refuses
/// right now: paused, waiting for a game, or the engine is still starting.
pub const PAUSED_REASON: &str = "Recording is paused";
/// No target: also what a line sent while the engine stops for Off is told (`Sent::Waiting`).
pub const WAITING_REASON: &str = "Recall is waiting for a game; nothing is being recorded";

pub fn capture_refusal(paused: bool, engine_running: bool, wanted: &EngineTarget) -> Option<&'static str> {
    if paused {
        return Some(PAUSED_REASON);
    }
    if engine_running {
        return None;
    }
    Some(match wanted {
        EngineTarget::Off => WAITING_REASON,
        _ => "the recording engine is still starting; try again in a moment",
    })
}

/// Resolve the wanted engine target (see the module docs). While `paused`
/// nothing is captured: the tracker keeps following the foreground so a
/// resume picks up the current game at once.
pub fn wanted(detected: Option<&Target>, pinned: Option<&Target>, rec: &RecordingCfg, paused: bool) -> EngineTarget {
    if paused {
        return EngineTarget::Off;
    }
    if let Some(p) = pinned {
        return EngineTarget::Window(p.clone());
    }
    if rec.auto_record {
        if let Some(t) = detected {
            return EngineTarget::Window(t.clone());
        }
    }
    if rec.record_desktop_when_idle {
        EngineTarget::Desktop
    } else {
        EngineTarget::Off
    }
}

/// What [`EnginePlanner::step`] decided this tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Nothing to do: on target, or lingering after the game went away.
    Keep,
    /// Retarget the engine now.
    Apply(EngineTarget),
    /// A retarget is due but a save is in flight: it waits for the first
    /// idle tick.
    Held,
    /// A retarget is due while the long recording runs: stop it (once) and
    /// retarget after its stop. `game_closed` = the drain ended (no new game).
    StopLongRec { game_closed: bool },
}

/// What the running engine has in flight, for [`EnginePlanner::step`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Busy {
    /// A replay save or a screenshot not yet reported (holds a retarget, bounded).
    pub saving: bool,
    /// The long recording is starting or running and has not been given up
    /// on (its stop not yet timed out).
    pub long_rec: bool,
}

/// Turns the wanted target into engine (re)starts, holding a game's engine for
/// the linger window after the game goes away.
pub struct EnginePlanner {
    applied: EngineTarget,
    /// When the game went away and how long its engine is kept (the drain).
    lost: Option<(Instant, Duration)>,
    /// The linger the next loss uses (the buffered length, kept current).
    next_linger: Duration,
    /// [`Self::cut_drain`]'s stop is held for a save: no new save (R2-detect-r10-2).
    privacy_hold: bool,
}

impl EnginePlanner {
    pub fn new(linger: Duration) -> Self {
        EnginePlanner { applied: EngineTarget::Off, lost: None, next_linger: linger, privacy_hold: false }
    }

    /// How long the engine outlives the game if it went away now. Read once,
    /// at the loss: the engine's buffered length keeps growing with wall time.
    pub fn set_drain_len(&mut self, linger: Duration) {
        self.next_linger = linger;
    }

    /// The running drain: the game that went away, when, and for how long its
    /// last clip stays saveable. `None` while on target or with no engine.
    pub fn drain(&self) -> Option<(&Target, Instant, Duration)> {
        match (&self.applied, self.lost) {
            (EngineTarget::Window(t), Some((at, len))) => Some((t, at, len)),
            _ => None,
        }
    }

    pub fn applied(&self) -> &EngineTarget {
        &self.applied
    }

    /// When the current linger ends (for the pump's wait timeout).
    pub fn linger_deadline(&self) -> Option<Instant> {
        self.lost.map(|(t, len)| t + len)
    }

    /// `Some(new)` when an idle engine must be retargeted now (test shorthand).
    #[cfg(test)]
    pub fn update(&mut self, wanted: &EngineTarget, now: Instant) -> Option<EngineTarget> {
        match self.step(wanted, now, Busy::default()) {
            Step::Apply(target) => Some(target),
            Step::Keep | Step::Held | Step::StopLongRec { .. } => None,
        }
    }

    /// Plan one tick. A due retarget first stops a running long recording
    /// ([`Step::StopLongRec`], each tick until it reports stopping or its stop
    /// times out), then waits for a save in flight ([`Step::Held`]); the
    /// linger clock keeps running meanwhile, and a closed game's long
    /// recording is stopped only when its drain ends. With no engine (`Off`
    /// applied) there is nothing to protect.
    pub fn step(&mut self, wanted: &EngineTarget, now: Instant, busy: Busy) -> Step {
        if *wanted == self.applied {
            self.lost = None;
            self.privacy_hold = false;
            return Step::Keep;
        }
        let game_went_away = matches!(self.applied, EngineTarget::Window(_))
            && !matches!(wanted, EngineTarget::Window(_));
        if game_went_away {
            let (since, len) = *self.lost.get_or_insert((now, self.next_linger));
            if now.duration_since(since) < len {
                return Step::Keep;
            }
        }
        if self.applied != EngineTarget::Off {
            if busy.long_rec {
                return Step::StopLongRec { game_closed: game_went_away };
            }
            if busy.saving {
                return Step::Held;
            }
        }
        self.lost = None;
        self.privacy_hold = false;
        self.applied = wanted.clone();
        Step::Apply(wanted.clone())
    }

    /// Record that `target` was just applied out of band (a settings change
    /// restarted the engine with the wanted target, ending any linger).
    pub fn force(&mut self, target: EngineTarget) {
        self.applied = target;
        self.lost = None;
        self.privacy_hold = false;
    }

    /// A game's engine stops with no drain linger: at once, or when the save in flight
    /// lands ([`Step::Held`]). Nothing to do with no game applied.
    pub fn cut_drain(&mut self, now: Instant) {
        if matches!(self.applied, EngineTarget::Window(_)) {
            self.lost = Some((now, Duration::ZERO));
            self.privacy_hold = true;
        }
    }

    /// While [`Self::cut_drain`]'s stop waits for a save: why a new save is refused (it would
    /// save the private app's ring, captured after the stop too, and hold the stop again).
    pub fn privacy_refusal(&self) -> Option<String> {
        match &self.applied {
            EngineTarget::Window(t) if self.privacy_hold => Some(format!("{} is no longer recorded", t.exe)),
            _ => None,
        }
    }

    /// Pause: stop now, skipping the linger. `true` = an engine must stop.
    pub fn pause(&mut self) -> bool {
        let running = self.applied != EngineTarget::Off;
        self.force(EngineTarget::Off);
        running
    }
}

/// Recording keys the engine reads at spawn: a change needs a restart.
pub fn engine_settings_changed(old: &RecordingCfg, new: &RecordingCfg) -> bool {
    old.buffer_secs != new.buffer_secs
        || old.output_dir != new.output_dir
        || old.game_folders != new.game_folders
        || old.cpu_mask != new.cpu_mask
        || old.codec != new.codec
        || old.fps != new.fps
        || old.bitrate_mbps != new.bitrate_mbps
        || old.quality != new.quality
        || crate::record_audio_sources::changed(&old.audio_sources, &new.audio_sources)
}

/// The stdin line that hands the running engine new "Use your existing <Game> folder?" answers
/// (no restart: `recording.game_folder_choices` is not in [`engine_settings_changed`]).
pub fn game_folder_choices_line(choices: &std::collections::BTreeMap<String, String>) -> String {
    serde_json::json!({ "cmd": "set_game_folder_choices", "choices": choices }).to_string()
}

/// Environment for one engine spawn: `Some` sets, `None` removes. Every target
/// variable is set or removed explicitly so values the controller inherited
/// (e.g. from `Start-Recall.ps1`) never leak into a different target.
///
/// `cpu_mask` is passed only when the controller applied it to itself: the
/// engine exits if it cannot apply `RECALL_CPU_MASK`, and without it the
/// engine still inherits the controller's affinity + priority.
/// Settings tokens → the engine's `RECALL_QUALITY` names (NVENC preset p4 / p5 / p7).
fn engine_quality(token: &str) -> &'static str {
    match token {
        "performance" => "balanced",
        "quality" => "max",
        _ => "high",
    }
}

pub fn engine_env(target: &EngineTarget, rec: &RecordingCfg, cpu_mask: Option<&str>, mic_muted: bool) -> Vec<(String, Option<String>)> {
    let mut env = vec![
        ("RECALL_BUFFER_SECS".to_string(), Some(rec.buffer_secs.to_string())),
        ("RECALL_OUT_DIR".to_string(), Some(rec.output_dir.clone())),
        ("RECALL_GAME_FOLDERS".to_string(), Some(if rec.game_folders { "1" } else { "0" }.to_string())),
        ("RECALL_GAME_FOLDER_CHOICES".to_string(), Some(serde_json::to_string(&rec.game_folder_choices).unwrap_or_else(|_| "{}".into()))),
        ("RECALL_CPU_MASK".to_string(), cpu_mask.map(str::to_owned)),
        // Quality knobs (the engine side owns their meaning).
        ("RECALL_CODEC".to_string(), Some(rec.codec.clone())),
        ("RECALL_FPS".to_string(), Some(rec.fps.to_string())),
        ("RECALL_BITRATE".to_string(), Some((rec.bitrate_mbps as u64 * 1_000_000).to_string())),
        ("RECALL_QUALITY".to_string(), Some(engine_quality(&rec.quality).to_string())),
        // The sources list replaces the old per-track variables; those are removed so an
        // inherited value cannot add a track.
        (crate::record_audio_sources::SOURCES_ENV.to_string(), Some(crate::record_audio_sources::engine_value(&rec.audio_sources))),
        ("RECALL_AUDIO_VOICE_EXE".to_string(), None),
        ("RECALL_AUDIO_GAME".to_string(), None),
        ("RECALL_AUDIO_MIC_OFF".to_string(), None),
        (crate::record_app_audio::APPS_ENV.to_string(), None),
        // The mic mute at spawn: the engine applies it before its first track opens, so a muted
        // mic records nothing of the new run (the `set_mic_mute` after `hello` came too late).
        ("RECALL_MIC_MUTED".to_string(), Some(if mic_muted { "1" } else { "0" }.to_string())),
    ];
    let (hwnd, name, pid) = match target {
        EngineTarget::Window(t) => (
            Some((t.hwnd as usize).to_string()),
            Some(t.name.clone()),
            Some(t.pid.to_string()),
        ),
        EngineTarget::Desktop => (None, Some(DESKTOP_NAME.to_string()), None),
        EngineTarget::Off => (None, None, None),
    };
    // RECALL_AUDIO_MIC is left as inherited: under RECALL_AUDIO_EXACT (tests) it names the
    // only endpoint an input source with no device may open.
    env.push(("RECALL_CAPTURE_HWND".into(), hwnd));
    env.push(("RECALL_CAPTURE_NAME".into(), name));
    env.push(("RECALL_AUDIO_GAME_PID".into(), pid));
    env
}

#[cfg(test)]
#[path = "record_engine_tests.rs"]
mod tests;
