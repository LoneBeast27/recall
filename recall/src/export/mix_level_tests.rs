//! Settings > Audio > "Merge audio": the Mix level offset and "Prevent clipping" on export,
//! read back from the written file (synthetic clip: tones 440, 660 and 880 Hz on 3 tracks).

use std::path::Path;

use super::test_media::{probe, synth, Clip, Spec};
use super::video_encoder::VideoBackend;
use super::{export_edited_with, ExportRequest};
use crate::edit_model::{AudioSetting, EditPlan, ExportMode, ExportSettings, MixLevel, TimeRange};

fn track(stream_index: usize, mix: bool) -> AudioSetting {
    AudioSetting { stream_index, gain_db: 0.0, muted: false, mix }
}

fn export(clip: &Clip, name: &str, mode: ExportMode, audio: Vec<AudioSetting>, mix_level: MixLevel) -> (Vec<String>, Vec<Vec<f32>>) {
    let plan = EditPlan { mix_level, mix_down: Some(true), ranges: vec![TimeRange::new(0.5, 2.5)], audio };
    let request = ExportRequest { input: clip.path.to_string_lossy().into_owned(), output: clip.output(name), fps: 60 };
    let settings = ExportSettings { mode, ..Default::default() };
    export_edited_with(&request, &plan, &settings, VideoBackend::X264).unwrap();
    let probed = probe(Path::new(&clip.output(name)));
    (probed.audio_handlers.iter().map(|n| n.clone().unwrap_or_default()).collect(), probed.audio_pcm)
}

fn rms(pcm: &[f32]) -> f32 {
    let middle = &pcm[pcm.len() / 8..pcm.len() * 7 / 8];
    (middle.iter().map(|s| s * s).sum::<f32>() / middle.len().max(1) as f32).sqrt()
}

fn peak(pcm: &[f32]) -> f32 {
    pcm.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

fn db(a: f32, b: f32) -> f32 {
    20.0 * (a / b).log10()
}

#[test]
fn the_offset_is_applied_once_to_the_mix_track_only() {
    let spec = Spec { amplitude: 0.05, tones: vec![440.0, 660.0, 880.0], ..Spec::default() };
    let Some(clip) = synth("mix-offset", &spec) else {
        eprintln!("libx264 missing in this FFmpeg build; skipped");
        return;
    };
    let source = probe(Path::new(&clip.path));
    let len = source.audio_pcm[0].len().min(source.audio_pcm[1].len());
    let summed: Vec<f32> = (0..len).map(|n| source.audio_pcm[0][n] + source.audio_pcm[1][n]).collect();
    let third = rms(&source.audio_pcm[2]);
    let six = MixLevel { offset_db: 6.0, limiter: true };
    for mode in [ExportMode::Reencode, ExportMode::Lossless] {
        let audio = vec![track(1, true), track(2, true), track(3, false)];
        let (names, pcm) = export(&clip, &format!("offset-{mode:?}.mp4"), mode, audio, six);
        assert_eq!(names.len(), 2, "{mode:?}: Mix + the track kept apart: {names:?}");
        assert_eq!(names[0], "Mix", "{mode:?}");
        let mix = db(rms(&pcm[0]), rms(&summed));
        let apart = db(rms(&pcm[1]), third);
        eprintln!("{mode:?}: Mix {mix:+.2} dB over the plain sum, the separate track {apart:+.2} dB");
        assert!((mix - 6.0).abs() <= 0.5, "{mode:?}: Mix is {mix:+.2} dB, not +6 once");
        assert!(apart.abs() <= 0.5, "{mode:?}: the separate track moved {apart:+.2} dB");
    }
    // One track in the mix is that track, not a Mix: no offset.
    let (names, pcm) = export(&clip, "offset-lone.mp4", ExportMode::Reencode, vec![track(1, true), track(2, false), track(3, false)], six);
    assert_ne!(names[0], "Mix");
    let lone = db(rms(&pcm[0]), rms(&source.audio_pcm[0]));
    assert!(lone.abs() <= 0.5, "a lone track moved {lone:+.2} dB");
}

#[test]
fn with_prevent_clipping_off_a_loud_mix_is_clamped_not_limited() {
    let Some(clip) = synth("mix-nolimit", &Spec { amplitude: 1.0, ..Spec::default() }) else {
        eprintln!("libx264 missing in this FFmpeg build; skipped");
        return;
    };
    let both = vec![track(1, true), track(2, true)];
    for mode in [ExportMode::Reencode, ExportMode::Lossless] {
        let (_, on) = export(&clip, &format!("limit-on-{mode:?}.mp4"), mode, both.clone(), MixLevel::default());
        let (names, off) = export(&clip, &format!("limit-off-{mode:?}.mp4"), mode, both.clone(), MixLevel { offset_db: 0.0, limiter: false });
        assert_eq!(names, ["Mix"], "{mode:?}");
        let (on, off) = (peak(&on[0]), peak(&off[0]));
        eprintln!("{mode:?}: decoded peak limiter on {on:.3}, off {off:.3}");
        assert!(on <= 0.92, "{mode:?}: limiter on holds -1 dBFS ({on})");
        assert!(off > 0.95, "{mode:?}: limiter off reaches full scale ({off})");
        assert!(off <= 1.15, "{mode:?}: clamped at full scale, never wrapped ({off})");
    }
}

#[test]
fn level_mix_scales_and_clamps_only_without_the_limiter() {
    let samples = [0.5f32, -0.9, 0.2];
    assert_eq!(crate::edit_audio::level_mix(&samples, 1.0, true), samples);
    let up = MixLevel { offset_db: 6.0, limiter: false }.gain();
    assert_eq!(crate::edit_audio::level_mix(&samples, up, true), [0.5 * up, -1.0, 0.2 * up]);
    assert_eq!(crate::edit_audio::level_mix(&samples, up, false)[1], -0.9 * up, "the limiter gets the sum as it is");
}
