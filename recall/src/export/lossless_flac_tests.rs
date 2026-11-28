//! A lossless export of tracks MP4 can't copy (PCM, Vorbis) writes them as FLAC with the same
//! samples (gate review of ae23e72c, P2: they were re-encoded to AAC). Integer sources up to 24
//! bits come out bit for bit; a float or 32-bit source is rounded to 24 bits, so it is checked
//! against half a 24-bit step and its largest error is printed.

use std::path::Path;

use ffmpeg_next as ff;

use super::test_media::{probe, synth, Spec};
use super::video_encoder::VideoBackend;
use super::{export_edited_with, ExportRequest};
use crate::edit_model::{AudioSetting, EditPlan, ExportMode, ExportSettings, TimeRange};

/// (encoder, rate, channels, bit exact): what each source track must come back as.
const TRACKS: [(&str, u32, u16, bool); 5] = [
    ("pcm_s16le", 48_000, 2, true),
    ("pcm_s24le", 44_100, 2, true),
    ("pcm_f32le", 48_000, 1, false),
    ("pcm_s32le", 32_000, 2, false),
    ("libvorbis", 44_100, 2, false),
];
/// Half of one 24-bit step at full scale (about -144 dB): the most a rounding to 24 bits may
/// move a sample, plus 1% for the read-back, which scales 32-bit samples by 1 / (2^31 - 1).
const HALF_STEP_24: f64 = 1.01 / 16_777_216.0;

/// The offset of `out` inside `source` (samples, within half a second of `near`: a lossless cut
/// starts on a keyframe, and its audio on a packet edge near it) where the samples match best,
/// and the largest difference there over the middle of `out`. A 64-sample probe picks the
/// offset; the whole middle is then compared.
fn best_match(source: &[f32], out: &[f32], near: usize, rate: u32) -> (usize, f64) {
    let middle = out.len() / 10..out.len() * 9 / 10;
    let worst = |offset: usize, span: std::ops::Range<usize>| span.map(|i| (source[offset + i] as f64 - out[i] as f64).abs()).fold(0.0, f64::max);
    let probe = middle.start..middle.start + 64;
    let half = rate as usize / 2;
    let best = (near.saturating_sub(half)..near + half)
        .filter(|offset| offset + middle.end <= source.len())
        .min_by(|a, b| worst(*a, probe.clone()).total_cmp(&worst(*b, probe.clone())));
    best.map_or((near, f64::INFINITY), |offset| (offset, worst(offset, middle)))
}

#[test]
fn lossless_export_keeps_foreign_tracks_lossless_as_flac() {
    let spec = Spec {
        tones: vec![300.0, 500.0, 700.0, 1100.0, 1700.0],
        formats: TRACKS.iter().map(|t| (t.1, t.2)).collect(),
        codecs: TRACKS.iter().map(|t| t.0).collect(),
        container: "mkv",
        amplitude: 0.5,
        ..Spec::default()
    };
    let clip = synth("lossless-flac", &spec).expect("the PCM and Vorbis fixture");
    let source = probe(&clip.path);
    let output = clip.output("lossless.mp4");
    let audio = (1..=5).map(|i| AudioSetting { stream_index: i, gain_db: 0.0, muted: false, mix: true }).collect();
    let plan = EditPlan { mix_level: Default::default(), mix_down: Some(false), ranges: vec![TimeRange::new(1.0, 3.0)], audio };
    let request = ExportRequest { input: clip.path.to_string_lossy().into_owned(), output: output.clone(), fps: 60 };
    let settings = ExportSettings { mode: ExportMode::Lossless, ..Default::default() };
    let report = export_edited_with(&request, &plan, &settings, VideoBackend::X264).expect("lossless export");
    let out = probe(Path::new(&output));
    let start = report.ranges.first().map_or(1.0, |r| r.start);
    let mut lines = Vec::new();
    let mut failures = Vec::new();
    for (k, (codec, rate, channels, exact)) in TRACKS.iter().enumerate() {
        if out.audio_codecs[k] != ff::codec::Id::FLAC {
            failures.push(format!("{codec}: written as {:?}, not FLAC", out.audio_codecs[k]));
            continue;
        }
        assert_eq!(out.audio_formats[k], (source.audio_formats[k].0, *channels), "{codec}: rate and channels kept");
        let near = (start * source.audio_formats[k].0 as f64).round() as usize;
        for c in 0..*channels as usize {
            let (offset, worst) = best_match(&source.audio_planes[k][c], &out.audio_planes[k][c], near, source.audio_formats[k].0);
            let ok = if *exact { worst == 0.0 } else { worst <= HALF_STEP_24 };
            lines.push(format!("{codec} {rate} Hz ch {c}: FLAC, offset {offset}, largest difference {worst:.3e} ({})", if worst == 0.0 { "bit exact" } else { "rounded to 24 bits" }));
            if !ok {
                failures.push(format!("{codec} ch {c}: largest difference {worst:.3e} (allowed {})", if *exact { "0".into() } else { format!("{HALF_STEP_24:.3e}") }));
            }
        }
    }
    eprintln!("{}", lines.join("\n"));
    assert!(failures.is_empty(), "{} failure(s):\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn unlabelled_three_and_seven_channels_keep_every_speaker() {
    use super::track_reader::unlabelled_layout;
    assert_eq!(unlabelled_layout(3), ff::ChannelLayout::SURROUND, "3.0, not 2.1");
    assert_eq!(unlabelled_layout(7), ff::ChannelLayout::_7POINT0, "7.0, not 6.1");
    assert_ne!(unlabelled_layout(3), ff::ChannelLayout::default(3), "FFmpeg's default for 3 is 2.1");
    assert_ne!(unlabelled_layout(7), ff::ChannelLayout::default(7), "FFmpeg's default for 7 is 6.1");
    assert_eq!((unlabelled_layout(3).channels(), unlabelled_layout(7).channels()), (3, 7));
    assert_eq!(unlabelled_layout(6), ff::ChannelLayout::default(6), "6 stays 5.1");
    assert_eq!(unlabelled_layout(2), ff::ChannelLayout::STEREO);
}
