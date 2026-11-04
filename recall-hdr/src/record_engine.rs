//! What the capture engine should record, and when to (re)start it.
//!
//! - [`wanted`]: an env-pinned window wins, then the auto-detected game (if
//!   `auto_record`), then the idle desktop (if `record_desktop_when_idle`).
//! - [`EnginePlanner`]: when the game goes away the engine -- and its replay
//!   ring -- is kept for [`EXIT_LINGER`], so a save pressed right after the
//!   final round still works. A new game retargets immediately -- unless a
//!   save or the long recording is in flight: the retarget is then held
//!   ([`Step::Held`]) until the engine is idle.
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

/// What [`EnginePlanner::step`] decided this tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Nothing to do: on target, or lingering after the game went away.
    Keep,
    /// Retarget the engine now.
    Apply(EngineTarget),
    /// A retarget is due but the running engine is busy (a save or the long
    /// recording in flight): it waits for the first idle tick.
    Held,
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

    /// `Some(new)` when an idle engine must be retargeted now (test shorthand).
    #[cfg(test)]
    pub fn update(&mut self, wanted: &EngineTarget, now: Instant) -> Option<EngineTarget> {
        match self.step(wanted, now, false) {
            Step::Apply(target) => Some(target),
            Step::Keep | Step::Held => None,
        }
    }

    /// Plan one tick. `busy` = the engine has a save or the long recording in
    /// flight: a due retarget is [`Step::Held`] (not applied) so neither is
    /// cut short; the linger clock keeps running meanwhile. With no engine
    /// (`Off` applied) there is nothing to protect.
    pub fn step(&mut self, wanted: &EngineTarget, now: Instant, busy: bool) -> Step {
        if *wanted == self.applied {
            self.lost_since = None;
            return Step::Keep;
        }
        let game_went_away = matches!(self.applied, EngineTarget::Window(_))
            && !matches!(wanted, EngineTarget::Window(_));
        if game_went_away {
            let since = *self.lost_since.get_or_insert(now);
            if now.duration_since(since) < self.linger {
                return Step::Keep;
            }
        }
        if busy && self.applied != EngineTarget::Off {
            return Step::Held;
        }
        self.lost_since = None;
        self.applied = wanted.clone();
        Step::Apply(wanted.clone())
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
        || old.mic_device.trim() != new.mic_device.trim()
        || old.mic_enabled != new.mic_enabled
        || old.desktop_audio_device.trim() != new.desktop_audio_device.trim()
        || old.moments_gallery != new.moments_gallery
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
        ("RECALL_QUALITY".to_string(), Some(engine_quality(&rec.quality).to_string())),
        ("RECALL_AUDIO_VOICE_EXE".to_string(), (!voice.is_empty()).then(|| voice.to_string())),
        ("RECALL_MOMENTS".to_string(), Some(if rec.moments_gallery { "1" } else { "0" }.to_string())),
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
    // Audio sources: set only when configured, so an inherited test override
    // (RECALL_AUDIO_MIC + RECALL_AUDIO_EXACT) still reaches the engine.
    for (key, name) in [("RECALL_AUDIO_MIC", &rec.mic_device), ("RECALL_AUDIO_GAME", &rec.desktop_audio_device)] {
        if !name.trim().is_empty() {
            env.push((key.into(), Some(name.trim().to_string())));
        }
    }
    if !rec.mic_enabled {
        env.push(("RECALL_AUDIO_MIC_OFF".into(), Some("1".into())));
    }
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
            RecordingCfg { mic_device: "Microphone (USB)".into(), ..base.clone() },
            RecordingCfg { mic_enabled: false, ..base.clone() },
            RecordingCfg { desktop_audio_device: "Speakers (USB)".into(), ..base.clone() },
            RecordingCfg { moments_gallery: true, ..base.clone() },
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
    fn a_busy_engine_holds_retargets_until_idle() {
        let t0 = Instant::now();
        let mut p = EnginePlanner::new(EXIT_LINGER);
        let a = EngineTarget::Window(target(1, 1, "A"));
        let b = EngineTarget::Window(target(2, 2, "B"));
        assert_eq!(p.step(&a, t0, true), Step::Apply(a.clone()), "no engine yet: nothing to protect");
        // A new game while the long recording runs: held, engine untouched.
        assert_eq!(p.step(&b, t0 + Duration::from_secs(1), true), Step::Held);
        assert_eq!(*p.applied(), a, "still recording the old target");
        assert_eq!(p.step(&a, t0 + Duration::from_secs(2), true), Step::Keep, "the game came back: nothing pending");
        assert_eq!(p.step(&b, t0 + Duration::from_secs(3), true), Step::Held);
        assert_eq!(p.step(&b, t0 + Duration::from_secs(4), false), Step::Apply(b.clone()), "idle: applies at once");
        // The game exits mid-recording: linger first, then held past it.
        assert_eq!(p.step(&EngineTarget::Off, t0 + Duration::from_secs(10), true), Step::Keep);
        let past_linger = t0 + Duration::from_secs(10) + EXIT_LINGER;
        assert_eq!(p.step(&EngineTarget::Off, past_linger, true), Step::Held);
        assert_eq!(p.step(&EngineTarget::Off, past_linger + Duration::from_secs(60), false), Step::Apply(EngineTarget::Off));
        assert_eq!(p.step(&a, past_linger + Duration::from_secs(61), true), Step::Apply(a), "Off -> start never waits");
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
        assert_eq!(get(&win, "RECALL_OUT_DIR"), Some(rec.output_dir.clone()));
        let desk = engine_env(&EngineTarget::Desktop, &rec, None);
        assert_eq!(get(&desk, "RECALL_CPU_MASK"), None, "unapplied mask is not passed (engine would exit)");
        assert_eq!(get(&desk, "RECALL_CAPTURE_HWND"), None, "removed, not inherited");
        assert_eq!(get(&desk, "RECALL_AUDIO_GAME_PID"), None);
        assert_eq!(get(&desk, "RECALL_CAPTURE_NAME").as_deref(), Some("Desktop"));
        // Quality + voice plumbing: defaults, then every key changed.
        assert_eq!(get(&win, "RECALL_CODEC").as_deref(), Some("h264"));
        assert_eq!(get(&win, "RECALL_FPS").as_deref(), Some("60"));
        assert_eq!(get(&win, "RECALL_BITRATE").as_deref(), Some("25000000"), "bit/s");
        assert_eq!(get(&win, "RECALL_QUALITY").as_deref(), Some("high"), "Balanced = engine high (p5)");
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
        assert_eq!(get(&env, "RECALL_QUALITY").as_deref(), Some("max"), "Quality = engine max (p7)");
        assert_eq!(get(&env, "RECALL_AUDIO_VOICE_EXE").as_deref(), Some("Discord.exe"));
        assert_eq!(get(&win, "RECALL_MOMENTS").as_deref(), Some("0"), "Moments gallery is opt-in");
        let moments = RecordingCfg { moments_gallery: true, ..RecordingCfg::default() };
        assert_eq!(get(&engine_env(&EngineTarget::Desktop, &moments, None), "RECALL_MOMENTS").as_deref(), Some("1"));
    }

    #[test]
    fn audio_devices_reach_the_engine_only_when_configured() {
        let has = |env: &[(String, Option<String>)], k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        let plain = engine_env(&EngineTarget::Desktop, &RecordingCfg::default(), None);
        for key in ["RECALL_AUDIO_MIC", "RECALL_AUDIO_GAME", "RECALL_AUDIO_MIC_OFF"] {
            assert_eq!(has(&plain, key), None, "{key}: Windows default = inherited, not removed");
        }
        let rec = RecordingCfg {
            mic_device: " Microphone (3- RODECaster Duo Chat) ".into(),
            desktop_audio_device: "Game (RØDECaster)".into(),
            mic_enabled: false,
            ..RecordingCfg::default()
        };
        let env = engine_env(&EngineTarget::Desktop, &rec, None);
        assert_eq!(has(&env, "RECALL_AUDIO_MIC"), Some(Some("Microphone (3- RODECaster Duo Chat)".into())));
        assert_eq!(has(&env, "RECALL_AUDIO_GAME"), Some(Some("Game (RØDECaster)".into())));
        assert_eq!(has(&env, "RECALL_AUDIO_MIC_OFF"), Some(Some("1".into())));
    }
}
