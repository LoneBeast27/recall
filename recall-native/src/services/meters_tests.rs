//! Meter targets, endpoint matching and level maths (no WASAPI).
use super::*;

fn source(kind: Kind, device: &str, exe: &str) -> Source {
    Source { id: format!("{}-1", kind.token()), kind, device: device.into(), device_id: String::new(), exe: exe.into(), name: "S".into(), enabled: true, gain_db: 0.0, mic_mute: None }
}

#[test]
fn each_source_is_metered_the_way_the_recorder_captures_it() {
    let device = |render: bool, d: &str| MeterTarget::Device { render, device: d.into(), device_id: String::new() };
    assert_eq!(spec_for(&source(Kind::Input, " Microphone (USB Audio Device) ", ""), None).target, device(false, "Microphone (USB Audio Device)"));
    assert_eq!(spec_for(&source(Kind::Output, "", ""), None).target, device(true, ""));
    assert_eq!(spec_for(&source(Kind::Game, "Speakers (USB Audio Device)", ""), None).target, device(true, "Speakers (USB Audio Device)"), "desktop: the output");
    assert_eq!(spec_for(&source(Kind::Game, "", ""), Some(42)).target, MeterTarget::Process { pid: 42 }, "in game: its process");
    assert_eq!(spec_for(&source(Kind::App, "", "player.exe"), None).target, MeterTarget::App { exe: "player.exe".into() });
    let specs = vec![spec_for(&source(Kind::Game, "", ""), None), spec_for(&source(Kind::Input, "", ""), None)];
    let labels: Vec<_> = Shared::new(specs).readings.lock().unwrap().iter().map(|r| r.label.clone()).collect();
    assert_eq!(labels, ["game-1", "input-1"], "readings are keyed by source id");
}

/// 2026-11-07 merge-gate A4 rule. Same table as `recall` `audio_tests.rs`: the meter picks what
/// the recorder records. No word, case or bracket-index matching any more.
#[test]
fn only_a_leading_index_is_dropped_and_the_rest_matches_exactly() {
    let names: Vec<String> = ["Virtual B (Mixer)", "Virtual A (Mixer)", "Line (2- Box One)", "System (MØNITOR)"].map(String::from).to_vec();
    assert_eq!(match_endpoint(&names, "System (MØNITOR)"), Some(3), "exact, non-ASCII");
    assert_eq!(match_endpoint(&names, "virtual a"), None, "words are not a name");
    assert_eq!(match_endpoint(&names, "System (MONITOR)"), None, "ø is not o");
    assert_eq!(match_endpoint(&names, "SYSTEM (mønitor)"), None, "case included");
    assert_eq!(match_endpoint(&names, "Line (Box One)"), None, "an index inside a bracket is part of the name");
    assert_eq!(match_endpoint(&names, "Virtual A"), None, "a prefix is not a name");
    assert_eq!(match_endpoint(&names, "  "), None, "empty = Windows default");
}

/// 2026-11-07 merge-gate A4 rule: a saved "3- X" opens the one present "4- X"; with two present
/// endpoints that both read "X" once the index goes, neither.
#[test]
fn a_renumbered_endpoint_opens_only_when_it_is_the_one_match() {
    let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<String>>();
    assert_eq!(match_endpoint(&names(&["Speakers (Box)", "4- Line (Box One)"]), "3- Line (Box One)"), Some(1), "renumbered");
    assert_eq!(match_endpoint(&names(&["Line (Box One)"]), "3- Line (Box One)"), Some(0), "the index went");
    assert_eq!(match_endpoint(&names(&["4- Line (Box One)"]), "Line (Box One)"), Some(0), "the index came");
    let two = names(&["4- Line (Box One)", "5- Line (Box One)"]);
    assert_eq!(match_endpoint(&two, "3- Line (Box One)"), None, "two matches: wait, never a sibling");
    assert_eq!(match_endpoint(&two, "Line (Box One)"), None);
    assert_eq!(match_endpoint(&two, "5- Line (Box One)"), Some(1), "its own exact name is itself, not a sibling");
    assert_eq!(match_endpoint(&names(&["4- line (box one)"]), "3- Line (Box One)"), None, "case included");
    // R3-deep-audio-2: two endpoints with the same exact name: wait, never the first one listed.
    assert_eq!(match_endpoint(&names(&["Line (Box One)", "Line (Box One)"]), "Line (Box One)"), None, "two exact matches");
    assert_eq!(match_endpoint(&names(&["Line (Box One)", "Speakers (Box)", "Line (Box One)"]), "3- Line (Box One)"), None);
}

/// 2026-11-07 audit A5: the meter shows what the recorder records. A named device that is not
/// connected is metered as missing (the recorder records silence), never as the default.
#[test]
fn a_missing_named_device_is_metered_as_missing_not_as_the_default() {
    let names = vec!["Mic (Realtek)".to_owned(), "Headset (USB)".to_owned()];
    assert_eq!(meter_pick(&names[..1], "Headset (USB)"), None, "not connected: no meter");
    assert_eq!(meter_pick(&names, "Headset (USB)"), Some(DevicePick::Named(1)));
    assert_eq!(meter_pick(&names, " "), Some(DevicePick::Default), "only a source with no device meters the default");
    assert!(default_moved(Some("{a}"), Some("{b}")), "the Windows default moved: reopen on the new one");
    assert!(!default_moved(Some("{a}"), Some("{a}")));
    assert!(!default_moved(Some("{a}"), None), "no default now: the stream reports the loss");
    assert!(!default_moved(None, Some("{a}")), "no id to compare");
}

/// 2026-11-07 audit A8: each meter capture closes its event handle (wasapi 0.19's `Handle`
/// has no `Drop`; a page left open leaked one per reopen).
#[test]
fn a_meter_capture_closes_its_event_handle() {
    let source = include_str!("meters.rs");
    let bare = ["let event = client", ".set_get_eventhandle()"].concat();
    assert!(!source.contains(&bare), "meters.rs keeps a bare wasapi::Handle");
}

#[test]
fn levels_compute_rms_and_peak_over_both_channels() {
    let bytes = |samples: &[f32]| samples.iter().flat_map(|s| s.to_le_bytes()).collect::<Vec<u8>>();
    let mut levels = Levels::default();
    levels.push_bytes(&bytes(&[0.5, -0.5, 0.5, -0.5]));
    let (rms, peak) = levels.take();
    assert!((rms - -6.0206).abs() < 0.01 && (peak - -6.0206).abs() < 0.01, "{rms} {peak}");
    levels.push_bytes(&bytes(&[1.0, 0.0, 1.0, 0.0]));
    let (rms, peak) = levels.take();
    assert!((rms - -3.0103).abs() < 0.01 && peak.abs() < 1e-6, "{rms} {peak}");
    assert_eq!(levels.take(), (FLOOR_DB, FLOOR_DB), "empty window is silence");
    levels.push_bytes(&[0, 0, 0]);
    assert_eq!(levels.take(), (FLOOR_DB, FLOOR_DB), "partial sample ignored");
}

#[test]
fn db_floor_and_bar_fraction() {
    assert_eq!(db(0.0), -90.0);
    assert_eq!(db(1e-12), -90.0);
    assert_eq!(db(1.0), 0.0);
    assert!((db(0.1) - -20.0).abs() < 1e-4);
    assert_eq!(bar_fraction(-90.0), 0.0);
    assert_eq!(bar_fraction(-30.0), 0.5);
    assert_eq!(bar_fraction(3.0), 1.0);
}

#[test]
#[ignore = "live hardware: opens shared-mode WASAPI loopback/capture on the default endpoints"]
fn live_meters_report_every_endpoint() {
    let meters = Meters::spawn(vec![spec_for(&source(Kind::Output, "", ""), None), spec_for(&source(Kind::Input, "", ""), None)]);
    std::thread::sleep(Duration::from_millis(1500));
    let snapshot = meters.snapshot();
    for r in &snapshot {
        eprintln!("{:<10} {:?} rms {:.1} dB peak {:.1} dB", r.label, r.state, r.rms_db, r.peak_db);
    }
    assert_eq!(snapshot.len(), 2);
    assert!(meters.revision() > 0, "threads published readings");
}

#[test]
fn gain_moves_bar_hold_and_number_together_and_keeps_silence() {
    assert_eq!(with_gain(-30.0, 6.0), -24.0);
    assert_eq!(with_gain(-30.0, -12.5), -42.5);
    assert_eq!(with_gain(FLOOR_DB, 12.0), FLOOR_DB, "silence is not lifted to -78 dB");
    assert_eq!(with_gain(-80.0, -60.0), FLOOR_DB, "clamped at the floor");
    assert_eq!(with_gain(-2.0, 12.0), 10.0);
    assert_eq!(with_gain(3.0, 12.0), 12.0, "clamped at the top");
    // A small fader step never makes the bar or the hold line jump: the change is the step.
    for gain in [-60.0_f32, -20.0, 0.0, 6.5, 12.0] {
        let (rms, hold) = (-35.0, -18.0);
        let step = 0.5;
        assert!((bar_fraction(with_gain(rms, gain + step)) - bar_fraction(with_gain(rms, gain))).abs() <= step / 60.0 + 1e-6);
        assert!(with_gain(hold, gain) >= with_gain(rms, gain), "hold stays at or above the bar for gain {gain}");
    }
}

#[test]
fn set_bumps_revision_only_on_visible_change() {
    let shared = Shared::new(vec![spec_for(&source(Kind::Output, "", ""), None)]);
    shared.set(0, -90.0, -90.0, MeterState::Missing);
    assert_eq!(shared.revision.load(Ordering::Acquire), 0);
    shared.set(0, -20.04, -10.0, MeterState::Live);
    shared.set(0, -20.01, -10.0, MeterState::Live);
    assert_eq!(shared.revision.load(Ordering::Acquire), 1);
    assert_eq!(shared.readings.lock().unwrap()[0].rms_db, -20.0);
}

/// 2026-11-07 audit R6-deep-audio-1/2: the page's copy of the recorder's `opened_own` (`recall`
/// `audio/own_device_tests.rs`, the same table): the saved id alone decides.
#[test]
fn opened_own_is_the_saved_id_alone() {
    let long = "x".repeat(257);
    let table = [
        (Some("{a}"), "Mic (Box)", "{a}", true),
        (Some(" {a} "), "3- Mic (Box)", "{a}", true),
        (Some("{a}"), "Mic (Box)", "", false),
        (Some("{b}"), "Mic (Box)", "{a}", false),
        (None, "Mic (Box)", "{a}", false),
        (Some("{a}"), "", "{a}", false),
        (Some(long.as_str()), "Mic (Box)", long.as_str(), false),
        (Some("{a}\nb"), "Mic (Box)", "{a}\nb", false),
    ];
    for (id, want, want_id, own) in table {
        assert_eq!(super::opened_own(id, want, want_id), own, "{id:?} for {want:?} {want_id:?}");
    }
    let names = |list: &[&str]| list.iter().map(|n| (*n).to_owned()).collect::<Vec<String>>();
    assert_eq!(super::stripped_hits(&names(&["4- X", "5- X", "Y"]), "3- X"), 2);
}
