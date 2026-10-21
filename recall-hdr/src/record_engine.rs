//! What the capture engine should record, and when to (re)start it.
//!
//! - [`wanted`]: an env-pinned window wins, then the auto-detected game (if
//!   `auto_record`), then the idle desktop (if `record_desktop_when_idle`).
//! - [`EnginePlanner`]: when the game goes away the engine -- and its replay
//!   ring -- is kept for [`EXIT_LINGER`], so a save pressed right after the
//!   final round still works. A new game retargets immediately.
//! - [`engine_env`]: the per-spawn environment the engine reads at startup.

use std::time::{Duration, Instant};

use crate::record_config::RecordingCfg;
use crate::record_target::{Target, DESKTOP_NAME};

/// How long the engine outlives its game so a late save still has a buffer.
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

pub fn capture_refusal(paused: bool, engine_running: bool, wanted: &EngineTarget) -> Option<&'static str> {
    if paused {
        return Some(PAUSED_REASON);
    }
    if engine_running {
        return None;
    }
    Some(match wanted {
        EngineTarget::Off => "Recall is waiting for a game; nothing is being recorded",
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

/// Turns the wanted target into engine (re)starts, holding a game's engine for
/// the linger window after the game goes away.
pub struct EnginePlanner {
    applied: EngineTarget,
    lost_since: Option<Instant>,
    linger: Duration,
}

impl EnginePlanner {
    pub fn new(linger: Duration) -> Self {
        EnginePlanner { applied: EngineTarget::Off, lost_since: None, linger }
    }

    pub fn applied(&self) -> &EngineTarget {
        &self.applied
    }

    /// When the current linger ends (for the pump's wait timeout).
    pub fn linger_deadline(&self) -> Option<Instant> {
        self.lost_since.map(|t| t + self.linger)
    }

    /// `Some(new)` when the engine must be retargeted now.
    pub fn update(&mut self, wanted: &EngineTarget, now: Instant) -> Option<EngineTarget> {
        if *wanted == self.applied {
            self.lost_since = None;
            return None;
        }
        let game_went_away = matches!(self.applied, EngineTarget::Window(_))
            && !matches!(wanted, EngineTarget::Window(_));
        if game_went_away {
            let since = *self.lost_since.get_or_insert(now);
            if now.duration_since(since) < self.linger {
                return None;
            }
        }
        self.lost_since = None;
        self.applied = wanted.clone();
        Some(wanted.clone())
    }

    /// Record that `target` was just applied out of band (a settings change
    /// restarted the engine with the wanted target, ending any linger).
    pub fn force(&mut self, target: EngineTarget) {
        self.applied = target;
        self.lost_since = None;
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
        || old.cpu_mask != new.cpu_mask
        || old.codec != new.codec
        || old.fps != new.fps
        || old.bitrate_mbps != new.bitrate_mbps
        || old.quality != new.quality
        || old.voice_app.trim() != new.voice_app.trim()
}

/// Environment for one engine spawn: `Some` sets, `None` removes. Every target
/// variable is set or removed explicitly so values the controller inherited
/// (e.g. from `Start-Recall.ps1`) never leak into a different target.
///
/// `cpu_mask` is passed only when the controller applied it to itself: the
/// engine exits if it cannot apply `RECALL_CPU_MASK`, and without it the
/// engine still inherits the controller's affinity + priority.
pub fn engine_env(target: &EngineTarget, rec: &RecordingCfg, cpu_mask: Option<&str>) -> Vec<(String, Option<String>)> {
    let voice = rec.voice_app.trim();
    let mut env = vec![
        ("RECALL_BUFFER_SECS".to_string(), Some(rec.buffer_secs.to_string())),
        ("RECALL_OUT_DIR".to_string(), Some(rec.output_dir.clone())),
        ("RECALL_CPU_MASK".to_string(), cpu_mask.map(str::to_owned)),
        // Quality knobs (the engine side owns their meaning).
        ("RECALL_CODEC".to_string(), Some(rec.codec.clone())),
        ("RECALL_FPS".to_string(), Some(rec.fps.to_string())),
        ("RECALL_BITRATE".to_string(), Some((rec.bitrate_mbps as u64 * 1_000_000).to_string())),
        ("RECALL_QUALITY".to_string(), Some(rec.quality.clone())),
        ("RECALL_AUDIO_VOICE_EXE".to_string(), (!voice.is_empty()).then(|| voice.to_string())),
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
    env.push(("RECALL_CAPTURE_HWND".into(), hwnd));
    env.push(("RECALL_CAPTURE_NAME".into(), name));
    env.push(("RECALL_AUDIO_GAME_PID".into(), pid));
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(pid: u32, hwnd: isize, name: &str) -> Target {
        Target { hwnd, pid, name: name.into(), exe: format!("{name}.exe") }
    }

    #[test]
    fn wanted_prefers_pin_then_game_then_desktop() {
        let mut rec = RecordingCfg::default();
        let game = target(1, 1, "A");
        let pin = target(9, 9, "VALORANT");
        assert_eq!(wanted(Some(&game), Some(&pin), &rec, false), EngineTarget::Window(pin.clone()));
        assert_eq!(wanted(Some(&game), None, &rec, false), EngineTarget::Window(game.clone()));
        assert_eq!(wanted(None, None, &rec, false), EngineTarget::Off);
        rec.record_desktop_when_idle = true;
        assert_eq!(wanted(None, None, &rec, false), EngineTarget::Desktop);
        rec.auto_record = false;
        assert_eq!(wanted(Some(&game), None, &rec, false), EngineTarget::Desktop, "auto_record off ignores games");
    }

    #[test]
    fn pause_ignores_every_target_and_resume_picks_the_current_game_at_once() {
        let mut rec = RecordingCfg::default();
        rec.record_desktop_when_idle = true;
        let game = target(1, 1, "A");
        let pin = target(9, 9, "VALORANT");
        assert_eq!(wanted(Some(&game), Some(&pin), &rec, true), EngineTarget::Off, "pin ignored while paused");
        assert_eq!(wanted(Some(&game), None, &rec, true), EngineTarget::Off, "game ignored while paused");
        assert_eq!(wanted(None, None, &rec, true), EngineTarget::Off, "desktop ignored while paused");

        let t0 = Instant::now();
        let mut p = EnginePlanner::new(EXIT_LINGER);
        let a = EngineTarget::Window(game.clone());
        assert_eq!(p.update(&a, t0), Some(a.clone()));
        assert!(p.pause(), "a running engine must stop");
        assert_eq!(*p.applied(), EngineTarget::Off, "no 30 s linger on pause");
        assert!(p.linger_deadline().is_none());
        assert!(!p.pause(), "pausing twice stops nothing");
        // Paused: the game is still foreground but nothing restarts.
        assert_eq!(p.update(&wanted(Some(&game), None, &rec, true), t0 + Duration::from_secs(5)), None);
        // Resume: the tracked game is wanted again and starts immediately.
        assert_eq!(p.update(&wanted(Some(&game), None, &rec, false), t0 + Duration::from_secs(6)), Some(a));
    }

    #[test]
    fn only_spawn_time_keys_need_an_engine_restart() {
        let base = RecordingCfg::default();
        assert!(!engine_settings_changed(&base, &base));
        let hotkeys_only = RecordingCfg { mic_hotkey: "Alt+J".into(), auto_record: false, ..base.clone() };
        assert!(!engine_settings_changed(&base, &hotkeys_only), "hotkeys rebind, no restart");
        for changed in [
            RecordingCfg { codec: "hevc".into(), ..base.clone() },
            RecordingCfg { fps: 120, ..base.clone() },
            RecordingCfg { bitrate_mbps: 40, ..base.clone() },
            RecordingCfg { quality: "quality".into(), ..base.clone() },
            RecordingCfg { voice_app: "Discord.exe".into(), ..base.clone() },
            RecordingCfg { buffer_secs: 60, ..base.clone() },
            RecordingCfg { output_dir: "D:\\c".into(), ..base.clone() },
        ] {
            assert!(engine_settings_changed(&base, &changed), "{changed:?}");
        }
    }

    #[test]
    fn saves_and_other_capture_actions_are_refused_while_paused() {
        let game = EngineTarget::Window(target(1, 1, "A"));
        assert_eq!(capture_refusal(true, true, &game), Some(PAUSED_REASON));
        assert_eq!(capture_refusal(true, false, &EngineTarget::Off), Some("Recording is paused"));
        assert_eq!(capture_refusal(false, true, &game), None);
        assert!(capture_refusal(false, false, &EngineTarget::Off).unwrap().contains("waiting for a game"));
        assert!(capture_refusal(false, false, &game).unwrap().contains("starting"));
    }

    #[test]
    fn planner_lingers_after_game_exit_but_retargets_new_games_now() {
        let t0 = Instant::now();
        let mut p = EnginePlanner::new(EXIT_LINGER);
        let a = EngineTarget::Window(target(1, 1, "A"));
        assert_eq!(p.update(&a, t0), Some(a.clone()));
        assert_eq!(p.update(&a, t0 + Duration::from_secs(1)), None, "steady state is a no-op");
        // The game exits: the engine is kept for the linger window.
        assert_eq!(p.update(&EngineTarget::Off, t0 + Duration::from_secs(10)), None);
        assert_eq!(p.update(&EngineTarget::Off, t0 + Duration::from_secs(39)), None);
        assert!(p.linger_deadline().is_some());
        assert_eq!(p.update(&EngineTarget::Off, t0 + Duration::from_secs(40)), Some(EngineTarget::Off));
        // A new game during a linger retargets immediately.
        p.update(&a, t0 + Duration::from_secs(50));
        p.update(&EngineTarget::Desktop, t0 + Duration::from_secs(51));
        let b = EngineTarget::Window(target(2, 2, "B"));
        assert_eq!(p.update(&b, t0 + Duration::from_secs(52)), Some(b.clone()));
        assert!(p.linger_deadline().is_none());
        // Off -> Desktop needs no linger.
        let mut q = EnginePlanner::new(EXIT_LINGER);
        assert_eq!(q.update(&EngineTarget::Desktop, t0), Some(EngineTarget::Desktop));
    }

    #[test]
    fn engine_env_sets_or_removes_every_target_variable() {
        let rec = RecordingCfg::default();
        let get = |env: &[(String, Option<String>)], k: &str| env.iter().find(|(n, _)| n == k).unwrap().1.clone();
        let win = engine_env(&EngineTarget::Window(target(42, 0x1234, "VALORANT")), &rec, Some("ffff0000"));
        assert_eq!(get(&win, "RECALL_CPU_MASK").as_deref(), Some("ffff0000"));
        assert_eq!(get(&win, "RECALL_CAPTURE_HWND").as_deref(), Some("4660"));
        assert_eq!(get(&win, "RECALL_CAPTURE_NAME").as_deref(), Some("VALORANT"));
        assert_eq!(get(&win, "RECALL_AUDIO_GAME_PID").as_deref(), Some("42"));
        assert_eq!(get(&win, "RECALL_BUFFER_SECS").as_deref(), Some("120"));
        assert_eq!(get(&win, "RECALL_OUT_DIR").as_deref(), Some(r"V:\ALL RECORDINGS"));
        let desk = engine_env(&EngineTarget::Desktop, &rec, None);
        assert_eq!(get(&desk, "RECALL_CPU_MASK"), None, "unapplied mask is not passed (engine would exit)");
        assert_eq!(get(&desk, "RECALL_CAPTURE_HWND"), None, "removed, not inherited");
        assert_eq!(get(&desk, "RECALL_AUDIO_GAME_PID"), None);
        assert_eq!(get(&desk, "RECALL_CAPTURE_NAME").as_deref(), Some("Desktop"));
        // Quality + voice plumbing: defaults, then every key changed.
        assert_eq!(get(&win, "RECALL_CODEC").as_deref(), Some("h264"));
        assert_eq!(get(&win, "RECALL_FPS").as_deref(), Some("60"));
        assert_eq!(get(&win, "RECALL_BITRATE").as_deref(), Some("25000000"), "bit/s");
        assert_eq!(get(&win, "RECALL_QUALITY").as_deref(), Some("balanced"));
        assert_eq!(get(&win, "RECALL_AUDIO_VOICE_EXE"), None, "voice track off = removed");
        let tuned = RecordingCfg {
            codec: "av1".into(),
            fps: 120,
            bitrate_mbps: 60,
            quality: "quality".into(),
            voice_app: " Discord.exe ".into(),
            ..RecordingCfg::default()
        };
        let env = engine_env(&EngineTarget::Desktop, &tuned, None);
        assert_eq!(get(&env, "RECALL_CODEC").as_deref(), Some("av1"));
        assert_eq!(get(&env, "RECALL_FPS").as_deref(), Some("120"));
        assert_eq!(get(&env, "RECALL_BITRATE").as_deref(), Some("60000000"));
        assert_eq!(get(&env, "RECALL_QUALITY").as_deref(), Some("quality"));
        assert_eq!(get(&env, "RECALL_AUDIO_VOICE_EXE").as_deref(), Some("Discord.exe"));
    }
}
