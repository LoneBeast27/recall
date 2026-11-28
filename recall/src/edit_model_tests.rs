//! Tests of `edit_model` (a file of its own: the model is included by path in two crates).
use super::*;

#[test]
fn version_one_project_files_still_load() {
    let plan: EditPlan = serde_json::from_str(r#"{"ranges":[{"start":1.0,"end":3.5}],"audio":[{"stream_index":1,"gain_db":0.0,"muted":false}]}"#).unwrap();
    assert_eq!(plan.ranges[0], TimeRange::new(1.0, 3.5));
    let info: ClipInfo = serde_json::from_str(r#"{"path":"a.mp4","duration":2.0,"width":4,"height":2,"video_fps":60.0,"source_bytes":1,"source_modified_ms":1,"audio":[]}"#).unwrap();
    assert!(info.keyframes.is_empty());
    let settings: ExportSettings = serde_json::from_str("{}").unwrap();
    assert_eq!(settings, ExportSettings::default());
    assert_eq!(settings.mode, ExportMode::Reencode);
}

fn setting(stream_index: usize, muted: bool, mix: bool) -> AudioSetting {
    AudioSetting { stream_index, gain_db: 0.0, muted, mix }
}

fn plan(audio: Vec<AudioSetting>, mix_down: Option<bool>) -> EditPlan {
    EditPlan { mix_level: Default::default(), mix_down, ranges: vec![TimeRange::new(0.0, 1.0)], audio }
}

#[test]
fn in_mix_tracks_merge_first_out_of_mix_tracks_follow_and_muted_are_dropped() {
    let both = plan(vec![setting(1, false, true), setting(2, false, true)], None);
    assert_eq!(both.audio_outputs(&[1, 2]), vec![AudioOutput::Mix(vec![1, 2])]);
    let one_out = plan(vec![setting(1, false, true), setting(2, false, false), setting(3, false, true)], Some(true));
    assert_eq!(one_out.audio_outputs(&[1, 2, 3]), vec![AudioOutput::Mix(vec![1, 3]), AudioOutput::Track(2)], "a track switched out follows the Mix");
    let muted = plan(vec![setting(1, false, true), setting(2, true, true), setting(3, false, true)], Some(true));
    assert_eq!(muted.audio_outputs(&[1, 2, 3]), vec![AudioOutput::Mix(vec![1, 3])]);
    let lone = plan(vec![setting(1, false, false), setting(2, false, true)], Some(true));
    assert_eq!(lone.audio_outputs(&[1, 2]), vec![AudioOutput::Track(2), AudioOutput::Track(1)], "a lone in-mix track stays itself and leads");
    let none_in = plan(vec![setting(1, false, false), setting(2, false, false)], Some(true));
    assert_eq!(none_in.audio_outputs(&[1, 2]), vec![AudioOutput::Track(1), AudioOutput::Track(2)]);
    assert!(plan(vec![setting(1, true, true)], Some(true)).audio_outputs(&[1]).is_empty());
}

#[test]
fn an_empty_or_partial_saved_track_list_merges_every_track() {
    assert_eq!(plan(vec![], None).audio_outputs(&[1, 2, 3]), vec![AudioOutput::Mix(vec![1, 2, 3])], "no settings: one Mix track");
    let partial = plan(vec![setting(2, false, true)], Some(true));
    assert_eq!(partial.audio_outputs(&[1, 2, 3]), vec![AudioOutput::Mix(vec![1, 2, 3])], "a stream with no setting is in the mix");
    let partial_out = plan(vec![setting(2, false, false)], Some(true));
    assert_eq!(partial_out.audio_outputs(&[1, 2, 3]), vec![AudioOutput::Mix(vec![1, 3]), AudioOutput::Track(2)]);
}

#[test]
fn every_source_kind_starts_in_the_mix() {
    let game = info(&[SOURCE_GAME_PROCESS, SOURCE_MIC, "app:Chat.exe"]);
    let p = plan(game.default_audio(), Some(true));
    assert_eq!(p.audio_outputs(&[1, 2, 3]), vec![AudioOutput::Mix(vec![1, 2, 3])], "game + mic + voice chat app");
    let desktop = info(&[SOURCE_DESKTOP, SOURCE_MIC, SOURCE_VOICE, "app:a.exe"]);
    let p = plan(desktop.default_audio(), Some(true));
    assert_eq!(p.audio_outputs(&[1, 2, 3, 4]), vec![AudioOutput::Mix(vec![1, 2, 3, 4])], "desktop + mic + voice + app");
    let exported = info(&[SOURCE_MIX, "app:a.exe"]);
    assert_eq!(plan(exported.default_audio(), None).audio_outputs(&[1, 2]), vec![AudioOutput::Mix(vec![1, 2])]);
    let untagged = info(&["", ""]);
    for clip in [&game, &desktop, &exported, &untagged] {
        assert!(clip.default_audio().iter().all(|s| s.mix && !s.muted && s.gain_db == 0.0), "defaults: in the mix at 0 dB");
    }
    // A missing setting and the default setting export the same way.
    assert_eq!(plan(vec![], Some(true)).audio_outputs(&[1, 2, 3]), plan(game.default_audio(), Some(true)).audio_outputs(&[1, 2, 3]));
}

#[test]
fn with_merging_off_every_kept_track_is_its_own_track() {
    let audio = vec![setting(1, false, true), setting(2, false, false), setting(3, true, true)];
    let mut p = plan(audio, Some(false));
    assert_eq!(p.audio_outputs(&[1, 2, 3, 4]), vec![AudioOutput::Track(1), AudioOutput::Track(2), AudioOutput::Track(4)], "muted still dropped, in stream order");
    p.mix_down = None;
    assert_eq!(p.audio_outputs(&[1, 2, 3, 4]), vec![AudioOutput::Mix(vec![1, 4]), AudioOutput::Track(2)], "no choice: on");
    let old: EditPlan = serde_json::from_str(r#"{"ranges":[{"start":0.0,"end":1.0}]}"#).unwrap();
    assert_eq!(old.mix_down, None, "saved before the choice existed");
    assert!(!serde_json::to_string(&old).unwrap().contains("mix_down"), "None is not written");
}

#[test]
fn old_edits_with_mix_down_and_per_track_mix_off_load_unchanged() {
    let json = r#"{"ranges":[{"start":0.0,"end":1.0}],"audio":[{"stream_index":1,"gain_db":-3.0,"muted":false,"mix":false},{"stream_index":2,"gain_db":0.0,"muted":false}],"mix_down":false}"#;
    let old: EditPlan = serde_json::from_str(json).unwrap();
    assert_eq!(old.mix_down, Some(false));
    assert!(!old.audio[0].mix && old.audio[1].mix, "a missing mix field reads as on");
    let again: EditPlan = serde_json::from_str(&serde_json::to_string(&old).unwrap()).unwrap();
    assert_eq!(again, old, "the fields round-trip as saved");
    assert_eq!(old.audio_outputs(&[1, 2]), vec![AudioOutput::Track(1), AudioOutput::Track(2)], "its own Separate choice");
    assert_eq!(EditPlan { mix_down: Some(true), ..old }.audio_outputs(&[1, 2]), vec![AudioOutput::Track(2), AudioOutput::Track(1)], "its track 1 stays out of the mix");
}

fn info(sources: &[&str]) -> ClipInfo {
    let audio = sources
        .iter()
        .enumerate()
        .map(|(i, s)| AudioTrackInfo { stream_index: i + 1, name: format!("T{i}"), channels: 2, sample_rate: 48_000, preview_path: String::new(), waveform: Vec::new(), source: (*s).into() })
        .collect();
    ClipInfo { path: "a.mp4".into(), duration: 1.0, width: 4, height: 2, video_fps: 60.0, source_bytes: 1, source_modified_ms: 1, audio, keyframes: Vec::new(), ..Default::default() }
}

#[test]
fn audio_source_tags_parse_and_count_match() {
    assert_eq!(parse_audio_sources(Some(r#"["desktop-loopback","app:a.exe"]"#)), ["desktop-loopback", "app:a.exe"]);
    assert!(audio_sources_for(Some(r#"["mic"]"#), 2).is_empty(), "a tag for another track count is no tag");
    assert_eq!(audio_sources_for(Some(r#"["mic","app:a.exe"]"#), 2), ["mic", "app:a.exe"]);
    assert!(parse_audio_sources(Some("not json")).is_empty() && parse_audio_sources(None).is_empty());
    let saved: AudioSetting = serde_json::from_str(r#"{"stream_index":1,"gain_db":0.0,"muted":false}"#).unwrap();
    assert!(saved.mix, "edits saved before the mix field existed load");
}

#[test]
fn repeated_source_kinds_are_taken_as_they_are() {
    // The source manager may record several outputs and mics.
    let tag = r#"["game-process","desktop-loopback","desktop-loopback","mic","mic","app:x.exe"]"#;
    let sources = audio_sources_for(Some(tag), 6);
    assert_eq!(sources.len(), 6, "count matching takes repeats as they are");
    assert!(audio_sources_for(Some(tag), 5).is_empty());
    let refs: Vec<&str> = sources.iter().map(String::as_str).collect();
    let clip = info(&refs);
    assert_eq!(plan(clip.default_audio(), None).audio_outputs(&[1, 2, 3, 4, 5, 6]), vec![AudioOutput::Mix(vec![1, 2, 3, 4, 5, 6])]);
}

#[test]
fn an_export_tags_its_own_tracks_in_output_order() {
    let sources = vec![SOURCE_DESKTOP.to_owned(), SOURCE_MIC.to_owned(), "app:a.exe".to_owned()];
    let outputs = [AudioOutput::Mix(vec![1, 2]), AudioOutput::Track(3)];
    assert_eq!(export_sources(&sources, &[1, 2, 3], &outputs), Some(vec![SOURCE_MIX.into(), "app:a.exe".into()]));
    assert_eq!(export_sources(&sources, &[1, 2, 3], &[AudioOutput::Track(3), AudioOutput::Track(1)]), Some(vec!["app:a.exe".into(), SOURCE_DESKTOP.into()]));
    assert_eq!(export_sources(&[], &[1, 2], &[AudioOutput::Track(1)]), None, "untagged input: untagged output");
    assert_eq!(export_sources(&sources, &[1, 2], &[AudioOutput::Track(1)]), None, "count mismatch: untagged");
}

#[test]
fn duration_is_output_length_after_speed() {
    let plan = EditPlan { mix_level: Default::default(), mix_down: None, ranges: vec![TimeRange { start: 0.0, end: 2.0, speed: 0.5 }, TimeRange { start: 3.0, end: 5.0, speed: 2.0 }], audio: vec![] };
    assert!((plan.duration() - 5.0).abs() < 1e-9);
    assert!((plan.source_duration() - 4.0).abs() < 1e-9);
    assert!(plan.validate(6.0, &[]).is_ok());
}

#[test]
fn speed_and_lossless_combinations_are_validated() {
    let mut plan = EditPlan { mix_level: Default::default(), mix_down: None, ranges: vec![TimeRange { start: 0.0, end: 1.0, speed: 0.2 }], audio: vec![] };
    assert!(plan.validate(2.0, &[]).unwrap_err().contains("speed"));
    plan.ranges[0].speed = 0.5;
    let lossless = ExportSettings { mode: ExportMode::Lossless, ..Default::default() };
    assert!(lossless.validate(&plan).unwrap_err().contains("speed"));
    plan.ranges[0].speed = 1.0;
    assert!(lossless.validate(&plan).is_ok());
    let capped = ExportSettings { max_bytes: Some(1), ..lossless.clone() };
    assert!(capped.validate(&plan).unwrap_err().contains("size cap"));
    assert!(ExportSettings { height: Some(100), ..Default::default() }.validate(&plan).is_err());
}

#[test]
fn snapping_picks_the_keyframe_at_or_before() {
    let keys = [0.0, 0.5, 1.0];
    assert_eq!(snap_to_keyframe(&keys, 0.7), 0.5);
    assert_eq!(snap_to_keyframe(&keys, 1.0), 1.0);
    assert_eq!(snap_to_keyframe(&keys, 9.0), 1.0);
    assert_eq!(snap_to_keyframe(&[0.2], 0.1), 0.2);
    assert_eq!(snap_to_keyframe(&[], 0.3), 0.3);
}

#[test]
fn export_settings_round_trip_as_snake_case() {
    let settings = ExportSettings { mode: ExportMode::Lossless, ..ExportSettings::default() };
    let text = serde_json::to_string(&settings).unwrap();
    assert_eq!(text, r#"{"mode":"lossless","max_bytes":null,"height":null}"#);
    assert_eq!(serde_json::from_str::<ExportSettings>(&text).unwrap(), settings);
}

#[test]
fn mix_level_defaults_to_earlier_builds_and_is_not_written() {
    let old: EditPlan = serde_json::from_str(r#"{"ranges":[{"start":0.0,"end":1.0}],"mix_down":true}"#).unwrap();
    assert_eq!(old.mix_level, MixLevel { offset_db: 0.0, limiter: true });
    assert_eq!(old.mix_level.gain(), 1.0);
    assert!(!serde_json::to_string(&old).unwrap().contains("mix_level"), "the default leaves the file as it was");
    let set = EditPlan { mix_level: MixLevel { offset_db: -3.5, limiter: false }, ..old.clone() };
    let back: EditPlan = serde_json::from_str(&serde_json::to_string(&set).unwrap()).unwrap();
    assert_eq!(back, set);
    assert!((MixLevel { offset_db: 6.0, limiter: true }.gain() - 1.995).abs() < 0.001);
}

#[test]
fn a_mix_level_offset_outside_12_db_is_refused() {
    let mut plan = plan(vec![], None);
    for ok in [-12.0, 0.0, 12.0] {
        plan.mix_level.offset_db = ok;
        assert!(plan.validate(2.0, &[]).is_ok(), "{ok}");
    }
    for bad in [12.5, -13.0, f32::NAN] {
        plan.mix_level.offset_db = bad;
        assert!(plan.validate(2.0, &[]).is_err(), "{bad}");
    }
}

fn tagged(sources: &[&str]) -> ClipInfo {
    let audio = sources
        .iter()
        .enumerate()
        .map(|(n, s)| AudioTrackInfo { stream_index: n + 1, name: String::new(), channels: 2, sample_rate: 48_000, preview_path: String::new(), waveform: Vec::new(), source: (*s).to_owned() })
        .collect();
    ClipInfo { audio, ..Default::default() }
}

#[test]
fn the_engine_keeps_an_app_another_track_carries_out_of_the_mix() {
    let sources = |s: &[&str]| s.iter().map(|x| (*x).to_owned()).collect::<Vec<String>>();
    let all_in = plan(vec![], None);
    let desktop = sources(&[SOURCE_DESKTOP, SOURCE_MIC, "app:a.exe"]);
    assert_eq!(all_in.with_carried_apps_apart(&[1, 2, 3], &desktop).audio_outputs(&[1, 2, 3]), vec![AudioOutput::Mix(vec![1, 2]), AudioOutput::Track(3)]);
    let exported = sources(&[SOURCE_MIX, "app:a.exe"]);
    assert_eq!(all_in.with_carried_apps_apart(&[1, 2], &exported).audio_outputs(&[1, 2]), vec![AudioOutput::Track(1), AudioOutput::Track(2)]);
    let game = sources(&[SOURCE_GAME_PROCESS, "app:a.exe"]);
    assert_eq!(all_in.with_carried_apps_apart(&[1, 2], &game).audio_outputs(&[1, 2]), vec![AudioOutput::Mix(vec![1, 2])], "a game's own track holds no other app");
    assert_eq!(all_in.with_carried_apps_apart(&[1, 2, 3], &[]), all_in, "untagged: unchanged");
    // A muted carried app stays muted; its gain is kept.
    let set = plan(vec![AudioSetting { stream_index: 3, gain_db: -3.0, muted: true, mix: true }], None);
    let apart = set.with_carried_apps_apart(&[1, 2, 3], &desktop);
    assert_eq!(apart.audio[0], AudioSetting { stream_index: 3, gain_db: -3.0, muted: true, mix: false });
}

#[test]
fn a_saved_edit_missing_a_track_gets_0_1_11s_starting_setting() {
    let desktop = tagged(&[SOURCE_DESKTOP, SOURCE_MIC, "app:a.exe"]);
    let saved: Vec<bool> = desktop.audio.iter().map(|t| desktop.saved_default_setting(t).mix).collect();
    assert_eq!(saved, [true, true, false], "the app Desktop carries started out of the mix");
    assert!(desktop.default_audio().iter().all(|s| s.mix), "a new edit shows every track merging");
    let game = tagged(&[SOURCE_GAME_PROCESS, "app:a.exe"]);
    assert!(game.audio.iter().all(|t| game.saved_default_setting(t).mix));
}
