//! Clips whose audio timestamps jitter around the sample count (SteelSeries Moments: 1024-sample
//! AAC frames stamped 1000 or 1100 ticks apart) must decode to continuous sound in the editor
//! preview stems and in edited exports. Before 2026-10-27 both followed every stamp and cut a
//! hole or an overlap into each frame boundary: the user heard "very choppy" playback.

use std::path::Path;

use super::test_media::{probe, synth, Clip, Spec, RATE};
use super::video_encoder::VideoBackend;
use super::{export_edited_with, ExportRequest};
use crate::edit_model::{EditPlan, ExportSettings, TimeRange};

/// The first track's tone (`Spec::default().tones[0]`).
const TONE_HZ: f64 = 440.0;

fn moments_clip(tag: &str) -> Option<Clip> {
    let clip = synth(tag, &Spec { moments_audio_stamps: true, ..Spec::default() });
    if clip.is_none() {
        eprintln!("libx264 missing in this FFmpeg build; skipped");
    }
    let clip = clip?;
    // The fixture really is shaped like a Moments clip: steps of 1000 and 1100 ticks, never 1024.
    let steps: Vec<i64> = probe(&clip.path).audio[0].windows(2).map(|w| w[1].pts_us - w[0].pts_us).collect();
    let tick_us = |ticks: i64| ticks * 1_000_000 / RATE as i64;
    assert!(steps.iter().any(|s| (s - tick_us(1000)).abs() <= 1), "no 1000-tick step: {steps:?}");
    assert!(steps.iter().any(|s| (s - tick_us(1100)).abs() <= 1), "no 1100-tick step: {steps:?}");
    Some(clip)
}

/// Worst 1024-sample window of `pcm` (one channel, from `skip` on) measured against the best
/// fitting pure tone: residual energy over signal energy in dB. Clean AAC sits far below -30 dB;
/// a hole or a dropped run of samples inside a window lifts it towards 0 dB.
fn worst_residual_db(pcm: &[f32], skip: usize) -> f64 {
    let w = TONE_HZ * std::f64::consts::TAU / RATE as f64;
    let mut worst = f64::NEG_INFINITY;
    for start in (skip..pcm.len().saturating_sub(1024 + skip)).step_by(512) {
        let window = &pcm[start..start + 1024];
        let (mut ss, mut cc, mut sc, mut ys, mut yc, mut yy) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        for (i, &y) in window.iter().enumerate() {
            let (s, c) = (w * (start + i) as f64).sin_cos();
            let y = f64::from(y);
            ss += s * s; cc += c * c; sc += s * c; ys += y * s; yc += y * c; yy += y * y;
        }
        let det = ss * cc - sc * sc;
        let a = (ys * cc - yc * sc) / det;
        let b = (yc * ss - ys * sc) / det;
        let residual: f64 = window
            .iter()
            .enumerate()
            .map(|(i, &y)| {
                let (s, c) = (w * (start + i) as f64).sin_cos();
                (f64::from(y) - a * s - b * c).powi(2)
            })
            .sum();
        worst = worst.max(10.0 * (residual / yy.max(1e-12)).log10());
    }
    worst
}

/// Runs of at least 8 digital-zero frames inside the tone (a 16-bit WAV), i.e. holes cut into
/// it; leading and trailing silence do not count.
fn zero_holes(wav: &[u8], channels: usize) -> usize {
    let frames: Vec<bool> = wav[44..]
        .chunks_exact(2 * channels)
        .map(|frame| frame.iter().all(|&b| b == 0))
        .collect();
    let (Some(first), Some(last)) = (frames.iter().position(|z| !z), frames.iter().rposition(|z| !z)) else {
        panic!("the stem is silent");
    };
    let mut holes = 0;
    let mut run = 0;
    for &zero in &frames[first..=last] {
        if zero {
            run += 1;
        } else {
            holes += usize::from(run >= 8);
            run = 0;
        }
    }
    holes
}

fn first_channel(wav: &[u8], channels: usize) -> Vec<f32> {
    wav[44..]
        .chunks_exact(2 * channels)
        .map(|frame| f32::from(i16::from_le_bytes([frame[0], frame[1]])) / 32768.0)
        .collect()
}

/// Worst tone residual (dB) of the first track's preview stem, and its silence holes.
fn stem_quality(clip: &Clip) -> (f64, usize) {
    let cache = clip.dir.join("cache").to_string_lossy().into_owned();
    let info = crate::editor_media::prepare_clip(&clip.path.to_string_lossy(), &cache).unwrap();
    let track = &info.audio[0];
    // The stem as 16-bit PCM behind a 44-byte gap, the layout the helpers below read.
    let (pcm, _) = crate::editor_media::read_stem(Path::new(&track.preview_path)).unwrap();
    let wav: Vec<u8> = [0u8; 44].into_iter().chain(pcm.iter().flat_map(|s| s.to_le_bytes())).collect();
    let channels = usize::from(track.channels);
    (worst_residual_db(&first_channel(&wav, channels), 4800), zero_holes(&wav, channels))
}

/// Worst tone residual (dB) of the first track of an edited export (0.5 s to 4.5 s).
fn export_quality(clip: &Clip) -> f64 {
    let request = ExportRequest { input: clip.path.to_string_lossy().into_owned(), output: clip.output("edited.mp4"), fps: 60 };
    let plan = EditPlan { ranges: vec![TimeRange::new(0.5, 4.5)], audio: vec![] };
    export_edited_with(&request, &plan, &ExportSettings::default(), VideoBackend::X264).unwrap();
    worst_residual_db(&probe(Path::new(&request.output)).audio_pcm[0], 4800)
}

fn regular_clip(tag: &str) -> Option<Clip> {
    synth(tag, &Spec::default())
}

/// The jittered clip must sound as clean as the same clip with exact stamps (AAC noise only).
#[test]
fn preview_stems_of_jittered_stamps_are_continuous() {
    let (Some(jittered), Some(regular)) = (moments_clip("stamps-stem"), regular_clip("stamps-stem-ref")) else { return };
    let (worst, holes) = stem_quality(&jittered);
    let (reference, _) = stem_quality(&regular);
    eprintln!("stem: jittered worst {worst:.1} dB, {holes} holes; exact stamps {reference:.1} dB");
    assert_eq!(holes, 0, "silence holes cut into the tone");
    assert!(worst < reference + 3.0 && worst < -30.0, "stem is not a clean tone: {worst:.1} dB vs {reference:.1} dB");
}

#[test]
fn edited_exports_of_jittered_stamps_are_continuous() {
    let (Some(jittered), Some(regular)) = (moments_clip("stamps-export"), regular_clip("stamps-export-ref")) else { return };
    let worst = export_quality(&jittered);
    let reference = export_quality(&regular);
    eprintln!("export: jittered worst {worst:.1} dB; exact stamps {reference:.1} dB");
    assert!(worst < reference + 3.0, "export is not a clean tone: {worst:.1} dB vs {reference:.1} dB with exact stamps");
}
