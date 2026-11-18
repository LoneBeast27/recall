//! Five virtual sources through every export path (the maintainer, 2026-11-18: "have an audio
//! test where you have 5 virtual sources and see if test tones pass through all on export").
//!
//! The synthetic clip carries five audio tracks shaped like a real recording: each its own
//! tone, sample rate and channel count (a stereo game, a mono 48 kHz mic, a 44.1 kHz desktop
//! output, a 16 kHz mono headset app and a 32 kHz app), tagged with the recorder's sources.
//! Every case reads the written file back and checks, per output track, that each expected
//! tone is there at its own pitch and level (Goertzel at the exact frequency, so a wrong rate
//! fails too), that no other tone leaked in, and that the track lasts as long as the export.
//! CPU only (libx264 stands in for NVENC); the whole-clip copy needs NVENC and is skipped
//! without it. Run: `cargo test -p recall five_tone -- --nocapture` for the level table.

use std::path::Path;

use super::test_media::{probe, synth, Clip, Probe, Spec};
use super::video_encoder::VideoBackend;
use super::{export_edited_with, ExportRequest};
use crate::edit_model::{AudioSetting, EditPlan, ExportMode, ExportSettings, TimeRange};

/// (tone Hz, sample rate, channels, recorder source) per track, in stream order (video is 0).
const TRACKS: [(f32, u32, u16, &str); 5] = [
    (300.0, 48_000, 2, "game-process"),
    (500.0, 48_000, 1, "mic"),
    (700.0, 44_100, 2, "desktop-loopback"),
    (1100.0, 16_000, 1, "app:discord.exe"),
    (1700.0, 32_000, 2, "app:spotify.exe"),
];
const SOURCES: &str = r#"["game-process","mic","desktop-loopback","app:discord.exe","app:spotify.exe"]"#;
const AMPLITUDE: f32 = 0.2;
/// A tone counts as passed through when it keeps at least -6 dB of its source level...
const MIN_RATIO: f64 = 0.5;
/// ...and no more than +3 dB; a tone that should not be there stays below -26 dB.
const MAX_RATIO: f64 = 1.41;
const LEAK_RATIO: f64 = 0.05;

fn clip(tag: &str, native_height: Option<u32>) -> Clip {
    let spec = Spec {
        seconds: 5.0,
        tones: TRACKS.iter().map(|t| t.0).collect(),
        formats: TRACKS.iter().map(|t| (t.1, t.2)).collect(),
        amplitude: AMPLITUDE,
        sources: Some(SOURCES),
        native_height,
        ..Spec::default()
    };
    synth(tag, &spec).expect("the libx264 fixture (a tone check must not pass without it)")
}

/// The editor's starting choices: every track in the mix except the app tracks.
fn defaults() -> Vec<AudioSetting> {
    TRACKS.iter().enumerate().map(|(n, t)| AudioSetting { stream_index: n + 1, gain_db: 0.0, muted: false, mix: !t.3.starts_with("app:") }).collect()
}

fn all_mixed() -> Vec<AudioSetting> {
    defaults().into_iter().map(|s| AudioSetting { mix: true, ..s }).collect()
}

/// Strength of `hz` in the steady middle of `pcm` at `rate` (Goertzel, about amplitude / 2).
pub(super) fn strength(pcm: &[f32], rate: u32, hz: f64) -> f64 {
    let middle = &pcm[pcm.len() / 8..pcm.len() * 7 / 8];
    let w = 2.0 * std::f64::consts::PI * hz / rate as f64;
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &x in middle {
        let s0 = x as f64 + 2.0 * w.cos() * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    (s1 * s1 + s2 * s2 - 2.0 * w.cos() * s1 * s2).sqrt() / middle.len().max(1) as f64
}

fn db(ratio: f64) -> String {
    if ratio <= 0.0 { "-inf".into() } else { format!("{:+.1}", 20.0 * ratio.log10()) }
}

/// One export's verdict: a line per output track, and every failure found.
pub(super) struct Verdict {
    pub lines: Vec<String>,
    pub failures: Vec<String>,
}

/// Check `out` against `expected` (per output track, the source track numbers it must carry);
/// source track `n` holds `tones[n]`.
pub(super) fn judge(case: &str, source: &Probe, out: &Probe, tones: &[f32], expected: &[&[usize]], seconds: f64) -> Verdict {
    let mut v = Verdict { lines: Vec::new(), failures: Vec::new() };
    let source_level = |n: usize| strength(&source.audio_pcm[n], source.audio_formats[n].0, tones[n] as f64);
    if out.audio_pcm.len() != expected.len() {
        v.failures.push(format!("{case}: {} audio tracks, expected {}", out.audio_pcm.len(), expected.len()));
    }
    for (k, pcm) in out.audio_pcm.iter().enumerate() {
        let (rate, channels) = out.audio_formats[k];
        let name = out.audio_handlers[k].clone().unwrap_or_default();
        let want = expected.get(k).copied().unwrap_or(&[]);
        let length = pcm.len() as f64 / rate as f64;
        let mut cells = Vec::new();
        for (n, &hz) in tones.iter().enumerate() {
            let track = (hz,);
            let ratio = strength(pcm, rate, track.0 as f64) / source_level(n);
            cells.push(format!("{}Hz {}", track.0, db(ratio)));
            let present = want.contains(&n);
            if present && !(MIN_RATIO..=MAX_RATIO).contains(&ratio) {
                let shifted = [1.0 / 3.0, 0.5, 2.0 / 3.0, 0.9187, 1.0884, 1.5, 2.0, 3.0]
                    .iter()
                    .map(|m| (m, strength(pcm, rate, track.0 as f64 * m) / source_level(n)))
                    .fold((&1.0, 0.0), |best, x| if x.1 > best.1 { x } else { best });
                v.failures.push(format!(
                    "{case}: track {k} '{name}': {} Hz at {} dB (want -6..+3); strongest nearby: x{:.3} at {} dB",
                    track.0, db(ratio), shifted.0, db(shifted.1)
                ));
            }
            if !present && ratio > LEAK_RATIO {
                v.failures.push(format!("{case}: track {k} '{name}': {} Hz leaked in at {} dB", track.0, db(ratio)));
            }
        }
        if (length - seconds).abs() > 0.1 {
            v.failures.push(format!("{case}: track {k} '{name}' lasts {length:.3} s, the export {seconds:.3} s"));
        }
        v.lines.push(format!("{case:<22} #{k} {name:<12} {rate:>5} Hz {channels} ch {length:.2} s | {}", cells.join("  ")));
    }
    v
}

struct Case {
    name: &'static str,
    ranges: Vec<TimeRange>,
    audio: Vec<AudioSetting>,
    mix_down: Option<bool>,
    settings: ExportSettings,
    expected: &'static [&'static [usize]],
}

fn edited(clip: &Clip, case: &Case) -> Result<Probe, String> {
    let plan = EditPlan { mix_down: case.mix_down, ranges: case.ranges.clone(), audio: case.audio.clone() };
    let output = clip.output(&format!("{}.mp4", case.name));
    let request = ExportRequest { input: clip.path.to_string_lossy().into_owned(), output: output.clone(), fps: 60 };
    export_edited_with(&request, &plan, &case.settings, VideoBackend::X264)?;
    Ok(probe(Path::new(&output)))
}

/// Default Mix: game, mic and desktop in one Mix track, each app on its own.
const MIX_DEFAULT: &[&[usize]] = &[&[0, 1, 2], &[3], &[4]];
const MIX_ALL: &[&[usize]] = &[&[0, 1, 2, 3, 4]];
const SEPARATE: &[&[usize]] = &[&[0], &[1], &[2], &[3], &[4]];

fn cases() -> Vec<Case> {
    let one = || vec![TimeRange::new(0.5, 2.5)];
    let reencode = ExportSettings::default;
    let lossless = || ExportSettings { mode: ExportMode::Lossless, ..Default::default() };
    vec![
        Case { name: "mix-default", ranges: one(), audio: defaults(), mix_down: None, settings: reencode(), expected: MIX_DEFAULT },
        Case { name: "mix-all", ranges: one(), audio: all_mixed(), mix_down: None, settings: reencode(), expected: MIX_ALL },
        Case { name: "separate", ranges: one(), audio: defaults(), mix_down: Some(false), settings: reencode(), expected: SEPARATE },
        Case { name: "lossless-mix", ranges: one(), audio: defaults(), mix_down: None, settings: lossless(), expected: MIX_DEFAULT },
        Case { name: "lossless-separate", ranges: one(), audio: defaults(), mix_down: Some(false), settings: lossless(), expected: SEPARATE },
        Case { name: "size-limit", ranges: one(), audio: defaults(), mix_down: None, settings: ExportSettings { max_bytes: Some(400_000), ..Default::default() }, expected: MIX_DEFAULT },
        Case { name: "downscale", ranges: one(), audio: defaults(), mix_down: None, settings: ExportSettings { height: Some(160), ..Default::default() }, expected: MIX_DEFAULT },
        Case { name: "two-cuts", ranges: vec![TimeRange::new(0.5, 1.7), TimeRange::new(3.0, 4.2)], audio: defaults(), mix_down: None, settings: reencode(), expected: MIX_DEFAULT },
        Case { name: "speed-2x", ranges: vec![TimeRange { speed: 2.0, ..TimeRange::new(0.5, 4.5) }], audio: defaults(), mix_down: None, settings: reencode(), expected: MIX_DEFAULT },
        Case { name: "speed-half", ranges: vec![TimeRange { speed: 0.5, ..TimeRange::new(1.0, 2.0) }], audio: defaults(), mix_down: Some(false), settings: reencode(), expected: SEPARATE },
    ]
}

#[test]
fn five_tone_sources_pass_through_every_export() {
    let source_clip = clip("five-tone", None);
    let source = probe(&source_clip.path);
    assert_eq!(source.audio_formats, TRACKS.iter().map(|t| (t.1, t.2)).collect::<Vec<_>>(), "the fixture carries five formats");
    let mut lines = Vec::new();
    let mut failures = Vec::new();
    let mut run = |name: &str, result: Result<Probe, String>, expected: &[&[usize]], seconds: f64| match result {
        Ok(out) => {
            let tones: Vec<f32> = TRACKS.iter().map(|t| t.0).collect();
            let v = judge(name, &source, &out, &tones, expected, seconds);
            lines.extend(v.lines);
            failures.extend(v.failures);
        }
        Err(e) => failures.push(format!("{name}: export failed: {e}")),
    };
    for case in cases() {
        let seconds = case.ranges.iter().map(TimeRange::output_len).sum::<f64>();
        let result = edited(&source_clip, &case);
        run(case.name, result, case.expected, seconds);
    }
    // An upscaled export (needs the native-height tag; the CPU path scales with x264).
    let tall = clip("five-tone-up", Some(480));
    let up = Case { name: "upscale", ranges: vec![TimeRange::new(0.5, 2.5)], audio: defaults(), mix_down: None, settings: ExportSettings { upscale_height: Some(480), ..Default::default() }, expected: MIX_DEFAULT };
    let result = edited(&tall, &up);
    if let Ok(out) = &result {
        assert_eq!(out.height, 480, "the upscale ran");
    }
    run(up.name, result, up.expected, 2.0);
    // The whole clip with no edit keeps every track as it is (NVENC only).
    let whole = source_clip.output("whole-clip.mp4");
    match super::cfr::export_to_temp(&source_clip.path, Path::new(&whole), 60) {
        Ok(_) => run("whole-clip", Ok(probe(Path::new(&whole))), SEPARATE, 5.0),
        Err(e) if e.to_lowercase().contains("nvenc") || e.contains("unavailable") => lines.push(format!("whole-clip skipped (no NVENC): {e}")),
        Err(e) => failures.push(format!("whole-clip: export failed: {e}")),
    }
    eprintln!("level per track, dB against the source:\n{}", lines.join("\n"));
    assert!(failures.is_empty(), "{} failure(s):\n{}", failures.len(), failures.join("\n"));
}
