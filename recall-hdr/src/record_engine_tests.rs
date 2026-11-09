//! `record_engine` tests (split out for file size).

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

        RecordingCfg { buffer_secs: 60, ..base.clone() },
        RecordingCfg { audio_sources: crate::record_audio_sources::default_sources()[..1].to_vec(), ..base.clone() },
        RecordingCfg { output_dir: "D:\\c".into(), ..base.clone() },
        RecordingCfg { game_folders: true, ..base.clone() },
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
fn a_save_in_flight_holds_retargets_until_idle() {
    let t0 = Instant::now();
    let saving = Busy { saving: true, long_rec: false };
    let idle = Busy::default();
    let mut p = EnginePlanner::new(EXIT_LINGER);
    let a = EngineTarget::Window(target(1, 1, "A"));
    let b = EngineTarget::Window(target(2, 2, "B"));
    assert_eq!(p.step(&a, t0, saving), Step::Apply(a.clone()), "no engine yet: nothing to protect");
    // A new game while a save is in flight: held, engine untouched.
    assert_eq!(p.step(&b, t0 + Duration::from_secs(1), saving), Step::Held);
    assert_eq!(*p.applied(), a, "still recording the old target");
    assert_eq!(p.step(&a, t0 + Duration::from_secs(2), saving), Step::Keep, "the game came back: nothing pending");
    assert_eq!(p.step(&b, t0 + Duration::from_secs(3), saving), Step::Held);
    assert_eq!(p.step(&b, t0 + Duration::from_secs(4), idle), Step::Apply(b.clone()), "idle: applies at once");
    // The game exits mid-save: linger first, then held past it.
    assert_eq!(p.step(&EngineTarget::Off, t0 + Duration::from_secs(10), saving), Step::Keep);
    let past_linger = t0 + Duration::from_secs(10) + EXIT_LINGER;
    assert_eq!(p.step(&EngineTarget::Off, past_linger, saving), Step::Held);
    assert_eq!(p.step(&EngineTarget::Off, past_linger + Duration::from_secs(60), idle), Step::Apply(EngineTarget::Off));
    assert_eq!(p.step(&a, past_linger + Duration::from_secs(61), saving), Step::Apply(a), "Off -> start never waits");
}

#[test]
fn a_long_recording_is_stopped_for_a_retarget_never_held_on() {
    let t0 = Instant::now();
    let s = Duration::from_secs;
    let rec = Busy { saving: false, long_rec: true };
    let mut p = EnginePlanner::new(EXIT_LINGER);
    let a = EngineTarget::Window(target(1, 1, "A"));
    let b = EngineTarget::Window(target(2, 2, "B"));
    assert_eq!(p.step(&a, t0, rec), Step::Apply(a.clone()));
    // The game closes: its drain runs first, the recording is not cut short.
    p.set_drain_len(s(40));
    assert_eq!(p.step(&EngineTarget::Off, t0 + s(1), rec), Step::Keep);
    assert_eq!(p.step(&EngineTarget::Off, t0 + s(40), rec), Step::Keep, "not before the drain ends");
    assert_eq!(p.step(&EngineTarget::Off, t0 + s(41), rec), Step::StopLongRec { game_closed: true });
    assert_eq!(*p.applied(), a, "the engine runs until the recording is saved");
    // Stopped (or its stop timed out): the retarget applies.
    assert_eq!(p.step(&EngineTarget::Off, t0 + s(42), Busy::default()), Step::Apply(EngineTarget::Off));
    // A new game while the recording runs: stop at once, then retarget.
    assert_eq!(p.step(&a, t0 + s(50), rec), Step::Apply(a.clone()));
    assert_eq!(p.step(&b, t0 + s(51), rec), Step::StopLongRec { game_closed: false });
    // Recording stopped, a save still landing: held, then applied.
    let saving = Busy { saving: true, long_rec: false };
    assert_eq!(p.step(&b, t0 + s(52), saving), Step::Held);
    assert_eq!(p.step(&b, t0 + s(53), Busy::default()), Step::Apply(b));
}

#[test]
fn engine_env_sets_or_removes_every_target_variable() {
    let rec = RecordingCfg::default();
    let get = |env: &[(String, Option<String>)], k: &str| env.iter().find(|(n, _)| n == k).unwrap().1.clone();
    let win = engine_env(&EngineTarget::Window(target(42, 0x1234, "VALORANT")), &rec, Some("ffff0000"), false);
    assert_eq!(get(&win, "RECALL_CPU_MASK").as_deref(), Some("ffff0000"));
    assert_eq!(get(&win, "RECALL_CAPTURE_HWND").as_deref(), Some("4660"));
    assert_eq!(get(&win, "RECALL_CAPTURE_NAME").as_deref(), Some("VALORANT"));
    assert_eq!(get(&win, "RECALL_AUDIO_GAME_PID").as_deref(), Some("42"));
    assert_eq!(get(&win, "RECALL_BUFFER_SECS").as_deref(), Some("120"));
    assert_eq!(get(&win, "RECALL_OUT_DIR"), Some(rec.output_dir.clone()));
    assert_eq!(get(&win, "RECALL_GAME_FOLDERS").as_deref(), Some("0"), "off by default, set so an inherited value cannot turn it on");
    let sorted = engine_env(&EngineTarget::Desktop, &RecordingCfg { game_folders: true, ..rec.clone() }, None, false);
    assert_eq!(get(&sorted, "RECALL_GAME_FOLDERS").as_deref(), Some("1"));
    let desk = engine_env(&EngineTarget::Desktop, &rec, None, false);
    assert_eq!(get(&desk, "RECALL_CPU_MASK"), None, "unapplied mask is not passed (engine would exit)");
    assert_eq!(get(&desk, "RECALL_CAPTURE_HWND"), None, "removed, not inherited");
    assert_eq!(get(&desk, "RECALL_AUDIO_GAME_PID"), None);
    assert_eq!(get(&desk, "RECALL_CAPTURE_NAME").as_deref(), Some("Desktop"));
    // Quality plumbing: defaults, then every key changed.
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
        ..RecordingCfg::default()
    };
    let env = engine_env(&EngineTarget::Desktop, &tuned, None, false);
    assert_eq!(get(&env, "RECALL_CODEC").as_deref(), Some("av1"));
    assert_eq!(get(&env, "RECALL_FPS").as_deref(), Some("120"));
    assert_eq!(get(&env, "RECALL_BITRATE").as_deref(), Some("60000000"));
    assert_eq!(get(&env, "RECALL_QUALITY").as_deref(), Some("max"), "Quality = engine max (p7)");
}

/// 2026-11-07 audit A2: a new engine learns the mute from its environment, before its first
/// track opens (the `set_mic_mute` after `hello` came after the tracks were recording).
#[test]
fn the_mute_reaches_a_new_engine_in_its_environment() {
    let get = |env: &[(String, Option<String>)]| env.iter().find(|(n, _)| n == "RECALL_MIC_MUTED").and_then(|(_, v)| v.clone());
    let rec = RecordingCfg::default();
    assert_eq!(get(&engine_env(&EngineTarget::Desktop, &rec, None, true)).as_deref(), Some("1"));
    assert_eq!(get(&engine_env(&EngineTarget::Desktop, &rec, None, false)).as_deref(), Some("0"), "set either way, never inherited");
}

#[test]
fn the_sources_list_reaches_the_engine_and_old_audio_variables_are_removed() {
    use crate::record_audio_sources::{AudioSource, SOURCES_ENV};
    let has = |env: &[(String, Option<String>)], k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let plain = engine_env(&EngineTarget::Desktop, &RecordingCfg::default(), None, false);
    let value = has(&plain, SOURCES_ENV).flatten().expect("always passed");
    assert!(value.contains(r#""id":"game""#) && value.contains(r#""id":"mic""#), "{value}");
    for key in ["RECALL_AUDIO_APPS", "RECALL_AUDIO_VOICE_EXE", "RECALL_AUDIO_GAME", "RECALL_AUDIO_MIC_OFF"] {
        assert_eq!(has(&plain, key), Some(None), "{key}: removed, so an inherited value never adds a track");
    }
    assert_eq!(has(&plain, "RECALL_AUDIO_MIC"), None, "inherited: names the test endpoint under RECALL_AUDIO_EXACT");
    let rec = RecordingCfg {
        audio_sources: vec![AudioSource { id: "in".into(), kind: "input".into(), device: "Microphone (USB Audio Device)".into(), name: "Voice".into(), ..AudioSource::default() }],
        ..RecordingCfg::default()
    };
    let value = has(&engine_env(&EngineTarget::Desktop, &rec, None, false), SOURCES_ENV).flatten().unwrap();
    assert!(value.contains("Microphone (USB Audio Device)") && !value.contains(r#""id":"game""#), "{value}");
}

#[test]
fn a_closed_game_drains_for_its_buffered_length_then_stops() {
    let t0 = Instant::now();
    let s = Duration::from_secs;
    let mut p = EnginePlanner::new(EXIT_LINGER);
    let game = target(1, 1, "sobosuba");
    let a = EngineTarget::Window(game.clone());
    assert_eq!(p.update(&a, t0), Some(a.clone()));
    assert!(p.drain().is_none(), "on target: no drain");
    p.set_drain_len(s(40));
    assert_eq!(p.update(&EngineTarget::Off, t0 + s(1)), None, "the engine is kept");
    let (t, at, len) = p.drain().expect("draining");
    assert_eq!((t, at, len), (&game, t0 + s(1), s(40)));
    p.set_drain_len(s(120)); // the buffer clock keeps going: the drain is fixed at the loss
    assert_eq!(p.linger_deadline(), Some(t0 + s(41)));
    assert_eq!(p.update(&EngineTarget::Off, t0 + s(40)), None);
    assert_eq!(p.update(&EngineTarget::Off, t0 + s(41)), Some(EngineTarget::Off), "at 0 the engine stops");
    assert!(p.drain().is_none());
    // A new game during a drain retargets at once and ends it.
    assert_eq!(p.update(&a, t0 + s(50)), Some(a.clone()));
    assert_eq!(p.update(&EngineTarget::Off, t0 + s(51)), None);
    let b = EngineTarget::Window(target(2, 2, "B"));
    assert_eq!(p.update(&b, t0 + s(52)), Some(b));
    assert!(p.drain().is_none());
}
