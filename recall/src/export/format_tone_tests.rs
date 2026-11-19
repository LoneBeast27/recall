//! Every audio format a clip can hold, through Mix, Separate and lossless exports (the
//! maintainer, 2026-11-18: "ensure all audio codec supports"). Recall records AAC at each
//! device's own rate and channel count; imported clips can carry anything FFmpeg reads. Each
//! batch is one synthetic clip with five tracks of different codecs, rates (8 to 96 kHz) and
//! layouts (mono, stereo, 5.1), checked tone by tone like `five_tone_tests`.

use std::path::Path;

use ffmpeg_next as ff;

use super::five_tone_tests::judge;
use super::test_media::{probe, synth, Clip, Spec};
use super::video_encoder::VideoBackend;
use super::{export_edited_with, ExportRequest};
use crate::edit_model::{AudioSetting, EditPlan, ExportMode, ExportSettings, TimeRange};

/// (encoder, rate, channels) per track; the tones are the same five in every batch.
type Track = (&'static str, u32, u16);
const TONES: [f32; 5] = [300.0, 500.0, 700.0, 1100.0, 1700.0];
/// The sample rates FFmpeg's AAC encoder accepts.
const AAC_RATES: [u32; 13] = [96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000, 7_350];
const MIX_ALL: &[&[usize]] = &[&[0, 1, 2, 3, 4]];
const SEPARATE: &[&[usize]] = &[&[0], &[1], &[2], &[3], &[4]];

const BATCHES: [(&str, &str, [Track; 5]); 5] = [
    // As Recall records (AAC), at the rates and layouts devices use, plus Opus and MP3.
    ("mp4-lossy", "mp4", [("aac", 8_000, 1), ("aac", 96_000, 2), ("aac", 22_050, 6), ("libopus", 48_000, 2), ("libmp3lame", 44_100, 2)]),
    // Lossless and broadcast codecs MP4 carries (FLAC is MP4-only, not MOV).
    ("mp4-lossless", "mp4", [("flac", 96_000, 2), ("alac", 32_000, 1), ("ac3", 48_000, 6), ("eac3", 32_000, 2), ("aac", 64_000, 2)]),
    // Uncompressed PCM as editors and capture cards write it, plus MP2.
    ("mov-pcm", "mov", [("pcm_s16le", 48_000, 1), ("pcm_f32le", 44_100, 2), ("pcm_s24le", 24_000, 2), ("mp2", 44_100, 2), ("aac", 11_025, 1)]),
    ("mkv-mixed", "mkv", [("libvorbis", 44_100, 2), ("libopus", 16_000, 1), ("flac", 8_000, 1), ("pcm_s16le", 12_000, 2), ("ac3", 44_100, 6)]),
    // Studio rates above AAC's 96 kHz ceiling (gate re-gate of e1bbc17d: 192 kHz failed Separate).
    ("mkv-hires", "mkv", [("pcm_s16le", 192_000, 8), ("pcm_f32le", 192_000, 2), ("pcm_s24le", 176_400, 2), ("flac", 192_000, 1), ("aac", 96_000, 2)]),
];

fn clip(name: &str, container: &'static str, tracks: &[Track; 5]) -> Option<Clip> {
    let spec = Spec {
        tones: TONES.to_vec(),
        formats: tracks.iter().map(|t| (t.1, t.2)).collect(),
        codecs: tracks.iter().map(|t| t.0).collect(),
        container,
        amplitude: 0.2,
        ..Spec::default()
    };
    synth(&format!("formats-{name}"), &spec)
}

fn settings(stream_index: usize) -> AudioSetting {
    AudioSetting { stream_index, gain_db: 0.0, muted: false, mix: true }
}

#[test]
fn every_audio_format_passes_mix_separate_and_lossless() {
    let mut lines = Vec::new();
    let mut failures = Vec::new();
    for (name, container, tracks) in &BATCHES {
        let Some(clip) = clip(name, container, tracks) else {
            failures.push(format!("{name}: the fixture could not be built (an encoder is missing)"));
            continue;
        };
        let source = probe(&clip.path);
        let described: Vec<String> = tracks.iter().map(|t| format!("{} {} Hz {} ch", t.0, t.1, t.2)).collect();
        lines.push(format!("{name} ({container}): {}", described.join(", ")));
        let audio: Vec<AudioSetting> = (1..=5).map(settings).collect();
        let runs = [
            ("mix", ExportMode::Reencode, None, MIX_ALL),
            ("separate", ExportMode::Reencode, Some(false), SEPARATE),
            ("lossless-separate", ExportMode::Lossless, Some(false), SEPARATE),
            ("lossless-mix", ExportMode::Lossless, None, MIX_ALL),
        ];
        for (run, mode, mix_down, expected) in runs {
            let case = format!("{name}/{run}");
            let output = clip.output(&format!("{run}.mp4"));
            let plan = EditPlan { mix_down, ranges: vec![TimeRange::new(0.5, 2.5)], audio: audio.clone() };
            let request = ExportRequest { input: clip.path.to_string_lossy().into_owned(), output: output.clone(), fps: 60 };
            let settings = ExportSettings { mode, ..Default::default() };
            match export_edited_with(&request, &plan, &settings, VideoBackend::X264) {
                // Lossless starts on the keyframe at or before the cut: the export says how long it is.
                Ok(report) => {
                    let out = probe(Path::new(&output));
                    if mode == ExportMode::Reencode {
                        // Every AAC track at a rate AAC takes: the source's own up to 96 kHz, else
                        // 96 kHz (88.2 kHz for the 44.1 kHz family).
                        for (k, &(rate, _)) in out.audio_formats.iter().enumerate() {
                            if !AAC_RATES.contains(&rate) || out.audio_codecs[k] != ff::codec::Id::AAC {
                                failures.push(format!("{case}: track {k} is {:?} at {rate} Hz", out.audio_codecs[k]));
                            }
                        }
                    }
                    if mode == ExportMode::Lossless && mix_down == Some(false) {
                        // Copied when MP4 carries the codec, else FLAC: never lossy in a lossless export.
                        for (k, (&from, &to)) in source.audio_codecs.iter().zip(&out.audio_codecs).enumerate() {
                            let want = if super::lossless::mp4_copies(from) { from } else { ff::codec::Id::FLAC };
                            // MP4 tags MPEG audio layer 2 like layer 3; the same bytes read back as MP3.
                            let same = to == want || (from == ff::codec::Id::MP2 && to == ff::codec::Id::MP3);
                            if !same {
                                failures.push(format!("{case}: track {k} {from:?} written as {to:?}, want {want:?}"));
                            }
                        }
                    }
                    let v = judge(&case, &source, &out, &TONES, expected, report.seconds);
                    lines.extend(v.lines);
                    failures.extend(v.failures);
                }
                Err(e) => failures.push(format!("{case}: export failed: {e}")),
            }
        }
    }
    eprintln!("level per track, dB against the source:\n{}", lines.join("\n"));
    assert!(failures.is_empty(), "{} failure(s):\n{}", failures.len(), failures.join("\n"));
}
