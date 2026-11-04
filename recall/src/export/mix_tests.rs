//! Mix-down on export: which audio tracks the file carries, in what order and under
//! what names, read back from the written file (synthetic clip: Game 440 Hz, Microphone 660 Hz).

use std::path::Path;

use super::test_media::{probe, synth, Clip, Spec, RATE};
use super::video_encoder::VideoBackend;
use super::{export_edited_with, ExportRequest};
use crate::edit_model::{AudioSetting, EditPlan, ExportMode, ExportSettings, TimeRange};

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
    let plan = EditPlan { ranges: vec![TimeRange::new(0.5, 2.5)], audio };
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
