//! Mix-down on export: which audio tracks the file carries, in what order and under
//! what names, read back from the written file (synthetic clip: Game 440 Hz, Microphone 660 Hz).

use std::path::Path;

use ffmpeg_next as ff;

use super::test_media::{probe, synth, Clip, Spec, RATE};
use super::video_encoder::VideoBackend;
use super::{export_edited_with, ExportRequest};
use crate::edit_model::{AudioSetting, AudioTrackInfo, EditPlan, ExportMode, ExportSettings, TimeRange};

const GAME: usize = 1;
const MIC: usize = 2;

fn clip(tag: &str) -> Option<Clip> {
    let clip = synth(tag, &Spec::default());
    if clip.is_none() {
        eprintln!("libx264 missing in this FFmpeg build; skipped");
    }
    clip
}

fn track(stream_index: usize, muted: bool, mix: bool) -> AudioSetting {
    AudioSetting { stream_index, gain_db: 0.0, muted, mix }
}

/// Exports `audio` choices in `mode`; returns (handler names, first-channel PCM) per track.
fn export(clip: &Clip, name: &str, mode: ExportMode, audio: Vec<AudioSetting>) -> (Vec<String>, Vec<Vec<f32>>) {
    export_with(clip, name, mode, audio, None)
}

/// `export` with the plan's "Mix tracks" choice.
fn export_with(clip: &Clip, name: &str, mode: ExportMode, audio: Vec<AudioSetting>, mix_down: Option<bool>) -> (Vec<String>, Vec<Vec<f32>>) {
    let plan = EditPlan { mix_down, ranges: vec![TimeRange::new(0.5, 2.5)], audio };
    let request = ExportRequest { input: clip.path.to_string_lossy().into_owned(), output: clip.output(name), fps: 60 };
    let settings = ExportSettings { mode, ..Default::default() };
    export_edited_with(&request, &plan, &settings, VideoBackend::X264).unwrap();
    let probed = probe(Path::new(&clip.output(name)));
    let names = probed.audio_handlers.iter().map(|n| n.clone().unwrap_or_default()).collect();
    (names, probed.audio_pcm)
}

/// Strength of `hz` in the steady middle of `pcm` (Goertzel power, normalised).
fn strength(pcm: &[f32], hz: f64) -> f64 {
    let middle = &pcm[pcm.len() / 8..pcm.len() * 7 / 8];
    let w = 2.0 * std::f64::consts::PI * hz / RATE as f64;
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &x in middle {
        let s0 = x as f64 + 2.0 * w.cos() * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    (s1 * s1 + s2 * s2 - 2.0 * w.cos() * s1 * s2).sqrt() / middle.len().max(1) as f64
}

fn carries(pcm: &[f32], hz: f64) -> bool {
    strength(pcm, hz) > 0.05
}

#[test]
fn both_tracks_mixed_give_one_track_holding_both() {
    let Some(clip) = clip("mix-both") else { return };
    for mode in [ExportMode::Reencode, ExportMode::Lossless] {
        let (names, pcm) = export(&clip, &format!("both-{mode:?}.mp4"), mode, vec![track(GAME, false, true), track(MIC, false, true)]);
        assert_eq!(names, ["Mix"], "{mode:?}");
        assert!(carries(&pcm[0], 440.0) && carries(&pcm[0], 660.0), "{mode:?}: the mix holds Game and Microphone");
    }
}

#[test]
fn one_track_mixed_leads_and_the_other_stays_separate() {
    let Some(clip) = clip("mix-one") else { return };
    for mode in [ExportMode::Reencode, ExportMode::Lossless] {
        let (names, pcm) = export(&clip, &format!("one-{mode:?}.mp4"), mode, vec![track(GAME, false, false), track(MIC, false, true)]);
        assert_eq!(names, ["Microphone", "Game"], "{mode:?}: the mixed track is first, under its own name");
        assert!(carries(&pcm[0], 660.0) && !carries(&pcm[0], 440.0), "{mode:?}");
        assert!(carries(&pcm[1], 440.0) && !carries(&pcm[1], 660.0), "{mode:?}");
    }
}

#[test]
fn a_muted_track_is_dropped() {
    let Some(clip) = clip("mix-muted") else { return };
    for mode in [ExportMode::Reencode, ExportMode::Lossless] {
        let (names, pcm) = export(&clip, &format!("muted-{mode:?}.mp4"), mode, vec![track(GAME, true, true), track(MIC, false, true)]);
        assert_eq!(names, ["Microphone"], "{mode:?}");
        assert!(carries(&pcm[0], 660.0) && !carries(&pcm[0], 440.0), "{mode:?}");
    }
}

/// Peak after AAC decode allowed above the limiter's -1 dBFS ceiling (0.891): measured
/// codec overshoot on this fixture (0.907 re-encode, 0.913 lossless, 2026-11-05) plus a margin.
const DECODED_CEILING: f32 = 0.92;

fn peak(pcm: &[f32]) -> f32 {
    pcm.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

/// RMS of the steady middle of `pcm`.
fn rms(pcm: &[f32]) -> f32 {
    let middle = &pcm[pcm.len() / 8..pcm.len() * 7 / 8];
    (middle.iter().map(|s| s * s).sum::<f32>() / middle.len().max(1) as f32).sqrt()
}

#[test]
fn two_full_scale_tracks_mix_without_clipping() {
    // 2026-11-05 merge-gate review: the sum was hard-clamped at ±1.0, so loud game audio
    // plus the mic flat-topped. A look-ahead limiter now holds peaks under -1 dBFS.
    let Some(clip) = synth("mix-loud", &Spec { amplitude: 1.0, ..Spec::default() }) else {
        eprintln!("libx264 missing in this FFmpeg build; skipped");
        return;
    };
    for mode in [ExportMode::Reencode, ExportMode::Lossless] {
        let (names, pcm) = export(&clip, &format!("loud-{mode:?}.mp4"), mode, vec![track(GAME, false, true), track(MIC, false, true)]);
        assert_eq!(names, ["Mix"], "{mode:?}");
        let mix = &pcm[0];
        let top = peak(mix);
        eprintln!("{mode:?}: decoded mix peak {top:.4}");
        assert!(top <= DECODED_CEILING, "{mode:?}: peak {top} above the ceiling");
        assert!(top >= 0.85, "{mode:?}: peak {top}: the limiter is engaged, not crushing the mix");
        assert!(!mix.windows(3).any(|w| w.iter().all(|s| s.abs() == top)), "{mode:?}: flat top at {top}");
        assert!(carries(mix, 440.0) && carries(mix, 660.0), "{mode:?}: both sources still heard");
    }
}

#[test]
fn a_quiet_mix_is_as_loud_as_its_sources_summed() {
    // Two -20 dBFS tones never reach the limiter, so the mix must equal their plain sum:
    // any hidden attenuation (such as fixed headroom) shows up here.
    let Some(clip) = synth("mix-quiet", &Spec { amplitude: 0.1, ..Spec::default() }) else {
        eprintln!("libx264 missing in this FFmpeg build; skipped");
        return;
    };
    let source = probe(Path::new(&clip.path));
    let len = source.audio_pcm[0].len().min(source.audio_pcm[1].len());
    let summed: Vec<f32> = (0..len).map(|n| source.audio_pcm[0][n] + source.audio_pcm[1][n]).collect();
    let expected = rms(&summed);
    for mode in [ExportMode::Reencode, ExportMode::Lossless] {
        let (names, pcm) = export(&clip, &format!("quiet-{mode:?}.mp4"), mode, vec![track(GAME, false, true), track(MIC, false, true)]);
        assert_eq!(names, ["Mix"], "{mode:?}");
        let db = 20.0 * (rms(&pcm[0]) / expected).log10();
        eprintln!("{mode:?}: quiet mix {db:+.2} dB against the plain sum");
        assert!(db.abs() <= 0.5, "{mode:?}: mix is {db:+.2} dB off its sources' sum");
    }
}

#[test]
fn a_desktop_clip_with_an_app_track_mixes_desktop_only_by_default() {
    // 2026-11-05 merge-gate review: an app track is also inside Desktop audio, so mixing both
    // would play the app twice. The editor's default (from the recorder's sources) leaves it out.
    let spec = Spec { sources: Some(r#"["desktop-loopback","app:tone.exe"]"#), ..Spec::default() };
    let Some(clip) = synth("mix-desktop-app", &spec) else {
        eprintln!("libx264 missing in this FFmpeg build; skipped");
        return;
    };
    let input = ff::format::input(&clip.path).unwrap();
    let sources = crate::editor_media::audio_sources(&input);
    assert_eq!(sources, ["desktop-loopback", "app:tone.exe"], "the fixture carries the recorder's tag");
    let tracks: Vec<AudioTrackInfo> = [GAME, MIC]
        .iter()
        .zip(&sources)
        .map(|(&stream_index, source)| AudioTrackInfo { stream_index, name: String::new(), channels: 2, sample_rate: 48_000, preview_path: String::new(), waveform: Vec::new(), source: source.clone() })
        .collect();
    let defaults: Vec<AudioSetting> = tracks.iter().map(AudioTrackInfo::default_setting).collect();
    for mode in [ExportMode::Reencode, ExportMode::Lossless] {
        let (names, pcm) = export(&clip, &format!("desktop-app-{mode:?}.mp4"), mode, defaults.clone());
        assert_eq!(names, ["Game", "Microphone"], "{mode:?}: Desktop leads on its own, the app stays a track of its own");
        assert!(carries(&pcm[0], 440.0) && !carries(&pcm[0], 660.0), "{mode:?}: the first track is Desktop only");
        assert!(carries(&pcm[1], 660.0), "{mode:?}");
    }
}

/// The sources tag of a written file, read the way the editor reads it.
fn sources_of(path: &str) -> Vec<String> {
    crate::editor_media::audio_sources(&ff::format::input(&Path::new(path)).unwrap())
}

#[test]
fn an_export_tags_its_own_tracks_so_a_reopened_export_keeps_its_mix_switches() {
    // 2026-11-05 merge-gate review: exports copied the tag but wrote a plain header, so MP4
    // dropped it, and the tag would have named the input's tracks, not the output's.
    let spec = Spec { sources: Some(r#"["desktop-loopback","app:tone.exe"]"#), ..Spec::default() };
    let Some(clip) = synth("mix-tag-out", &spec) else {
        eprintln!("libx264 missing in this FFmpeg build; skipped");
        return;
    };
    let cases: [(&str, Vec<AudioSetting>, Vec<&str>); 3] = [
        ("apart", vec![track(GAME, false, true), track(MIC, false, false)], vec!["desktop-loopback", "app:tone.exe"]),
        ("mixed", vec![track(GAME, false, true), track(MIC, false, true)], vec!["mix"]),
        ("muted", vec![track(GAME, false, true), track(MIC, true, false)], vec!["desktop-loopback"]),
    ];
    for mode in [ExportMode::Reencode, ExportMode::Lossless] {
        for (tag, audio, expected) in &cases {
            let name = format!("tag-{tag}-{mode:?}.mp4");
            let (names, _) = export(&clip, &name, mode, audio.clone());
            assert_eq!(sources_of(&clip.output(&name)), *expected, "{mode:?} {tag}: tracks {names:?}");
        }
    }
    let plain = synth("mix-tag-none", &Spec::default()).unwrap();
    let (_, _) = export(&plain, "untagged.mp4", ExportMode::Reencode, vec![track(GAME, false, true), track(MIC, false, false)]);
    assert!(sources_of(&plain.output("untagged.mp4")).is_empty(), "an untagged input gives an untagged export");
}

#[test]
fn the_whole_clip_cfr_file_keeps_the_sources_tag() {
    let spec = Spec { sources: Some(r#"["desktop-loopback","app:tone.exe"]"#), ..Spec::default() };
    let Some(clip) = synth("mix-tag-cfr", &spec) else {
        eprintln!("libx264 missing in this FFmpeg build; skipped");
        return;
    };
    let out = clip.output("cfr.mp4");
    match super::cfr::export_to_temp(&clip.path, Path::new(&out), 60) {
        Ok(_) => assert_eq!(sources_of(&out), ["desktop-loopback", "app:tone.exe"], "every track kept: the tag passes through"),
        Err(e) if e.to_lowercase().contains("nvenc") || e.contains("unavailable") => eprintln!("NVENC unavailable; skipped: {e}"),
        Err(e) => panic!("cfr export failed: {e}"),
    }
}

#[test]
fn every_export_mode_is_faststart_and_keeps_the_sources_tag() {
    // One header writer for all three paths: +faststart, plus +use_metadata_tags when tagged.
    let spec = Spec { sources: Some(r#"["desktop-loopback","app:tone.exe"]"#), ..Spec::default() };
    let Some(clip) = synth("mix-tag-faststart", &spec) else {
        eprintln!("libx264 missing in this FFmpeg build; skipped");
        return;
    };
    let apart = vec![track(GAME, false, true), track(MIC, false, false)];
    for mode in [ExportMode::Reencode, ExportMode::Lossless] {
        let name = format!("faststart-{mode:?}.mp4");
        export(&clip, &name, mode, apart.clone());
        super::test_media::assert_faststart(Path::new(&clip.output(&name)));
        assert_eq!(sources_of(&clip.output(&name)), ["desktop-loopback", "app:tone.exe"], "{mode:?}");
    }
    let cfr = clip.output("faststart-cfr.mp4");
    match super::cfr::export_to_temp(&clip.path, Path::new(&cfr), 60) {
        Ok(_) => {
            super::test_media::assert_faststart(Path::new(&cfr));
            assert_eq!(sources_of(&cfr), ["desktop-loopback", "app:tone.exe"], "CFR");
        }
        Err(e) if e.to_lowercase().contains("nvenc") || e.contains("unavailable") => eprintln!("NVENC unavailable; CFR skipped: {e}"),
        Err(e) => panic!("cfr export failed: {e}"),
    }
}

#[test]
fn with_mixing_off_every_track_is_exported_on_its_own_with_its_source() {
    // The maintainer, 2026-11-05: "have a toggle for mix or not on export". Off: no mixing, whatever the
    // per-track switches say; lossless copies the audio untouched, as before mix-down existed.
    let spec = Spec { sources: Some(r#"["desktop-loopback","app:tone.exe"]"#), ..Spec::default() };
    let Some(clip) = synth("mix-off", &spec) else {
        eprintln!("libx264 missing in this FFmpeg build; skipped");
        return;
    };
    let both = vec![track(GAME, false, true), track(MIC, false, true)];
    for mode in [ExportMode::Reencode, ExportMode::Lossless] {
        let name = format!("off-{mode:?}.mp4");
        let (names, pcm) = export_with(&clip, &name, mode, both.clone(), Some(false));
        assert_eq!(names, ["Game", "Microphone"], "{mode:?}");
        assert_eq!(sources_of(&clip.output(&name)), ["desktop-loopback", "app:tone.exe"], "{mode:?}");
        assert!(carries(&pcm[0], 440.0) && !carries(&pcm[0], 660.0), "{mode:?}");
        assert!(carries(&pcm[1], 660.0) && !carries(&pcm[1], 440.0), "{mode:?}");
        let (on, _) = export_with(&clip, &format!("on-{mode:?}.mp4"), mode, both.clone(), Some(true));
        assert_eq!(on, ["Mix"], "{mode:?}: on is unchanged");
    }
    // Lossless with mixing off copies both tracks byte for byte.
    let source = probe(Path::new(&clip.path));
    let copied = probe(Path::new(&clip.output("off-Lossless.mp4")));
    for track in 0..2 {
        let hashes: Vec<u64> = copied.audio[track].iter().map(|p| p.hash).collect();
        assert!(!hashes.is_empty() && hashes.iter().all(|h| source.audio[track].iter().any(|p| p.hash == *h)), "track {track} was re-encoded");
    }
}
