//! Re-encoded edited exports on synthetic clips, driven through the production
//! pipeline with libx264 standing in for NVENC.

use super::test_media::{probe, synth, tone, Clip, Spec, RATE, TRACK_NAMES};
use super::video_encoder::VideoBackend;
use super::{export_edited_with, EditedExport, ExportRequest};
use crate::edit_model::{EditPlan, ExportSettings, TimeRange};

fn clip(tag: &str) -> Option<Clip> {
    let clip = synth(tag, &Spec::default());
    if clip.is_none() {
        eprintln!("libx264 missing in this FFmpeg build; skipped");
    }
    clip
}

fn run(clip: &Clip, name: &str, plan: &EditPlan, settings: &ExportSettings) -> Result<EditedExport, String> {
    let request = ExportRequest {
        input: clip.path.to_string_lossy().into_owned(),
        output: clip.output(name),
        fps: 60,
    };
    export_edited_with(&request, plan, settings, VideoBackend::X264)
}

fn plan(ranges: Vec<TimeRange>) -> EditPlan {
    EditPlan { ranges, audio: vec![] }
}

fn within(actual: f64, expected: f64, fraction: f64) -> bool {
    (actual - expected).abs() <= expected * fraction
}

#[test]
fn progress_is_reported_against_the_planned_frames() {
    use std::sync::atomic::{AtomicU64, Ordering};
    // The sink is process-wide; a 1.75 s plan (105 frames) is unique to this test.
    static REPORTS: AtomicU64 = AtomicU64::new(0);
    static LAST: AtomicU64 = AtomicU64::new(0);
    super::set_progress_sink(|frames, total| {
        if total == 105 {
            REPORTS.fetch_add(1, Ordering::Relaxed);
            LAST.fetch_max(frames, Ordering::Relaxed);
        }
    });
    let Some(clip) = clip("progress") else { return };
    let report = run(&clip, "progress.mp4", &plan(vec![TimeRange::new(1.0, 2.75)]), &ExportSettings::default()).unwrap();
    assert_eq!(report.frames, 105);
    assert_eq!(REPORTS.load(Ordering::Relaxed), 3, "one report every 30 frames");
    assert_eq!(LAST.load(Ordering::Relaxed), 90);
}

#[test]
fn size_cap_is_obeyed_and_the_budget_is_used() {
    let Some(clip) = clip("cap") else { return };
    let plan = plan(vec![TimeRange::new(0.0, 4.5)]);
    let uncapped = run(&clip, "uncapped.mp4", &plan, &ExportSettings::default()).unwrap();
    let cap = 1_000_000u64;
    assert!(uncapped.bytes > cap * 2, "fixture must need the cap: {} bytes uncapped", uncapped.bytes);

    let settings = ExportSettings { max_bytes: Some(cap), ..Default::default() };
    let capped = run(&clip, "capped.mp4", &plan, &settings).unwrap();
    let on_disk = std::fs::metadata(clip.output("capped.mp4")).unwrap().len();
    assert_eq!(on_disk, capped.bytes);
    assert!(on_disk <= cap, "{on_disk} bytes exceeds the {cap} byte cap");
    assert!(on_disk >= cap / 2, "cap budget wasted: {on_disk} of {cap} bytes");
    assert!(capped.video_bitrate.unwrap() < uncapped.video_bitrate.unwrap());
    assert_eq!(capped.max_bytes, Some(cap));
    eprintln!(
        "size cap: uncapped {} B, capped {} B of {cap} B cap, video {} bps, {} attempt(s)",
        uncapped.bytes, on_disk, capped.video_bitrate.unwrap(), capped.attempts
    );
}

#[test]
fn impossible_size_cap_fails_with_a_reason_and_no_file() {
    let Some(clip) = clip("cap-impossible") else { return };
    let settings = ExportSettings { max_bytes: Some(100_000), ..Default::default() };
    let error = run(&clip, "tiny.mp4", &plan(vec![TimeRange::new(0.0, 4.5)]), &settings).unwrap_err();
    assert!(error.contains("audio alone"), "{error}");
    assert!(!std::path::Path::new(&clip.output("tiny.mp4")).exists());
    let leftovers: Vec<_> = std::fs::read_dir(&clip.dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains("recall-export"))
        .collect();
    assert!(leftovers.is_empty(), "temporary export file left behind");
}

#[test]
fn downscale_keeps_aspect_and_even_dimensions() {
    let Some(clip) = clip("scale") else { return };
    let settings = ExportSettings { height: Some(150), ..Default::default() };
    let report = run(&clip, "small.mp4", &plan(vec![TimeRange::new(1.0, 2.0)]), &settings).unwrap();
    assert_eq!((report.width, report.height), (200, 150));
    let probed = probe(std::path::Path::new(&clip.output("small.mp4")));
    assert_eq!((probed.width, probed.height), (200, 150));

    let upscale = ExportSettings { height: Some(1080), ..Default::default() };
    let report = run(&clip, "same.mp4", &plan(vec![TimeRange::new(1.0, 2.0)]), &upscale).unwrap();
    assert_eq!((report.width, report.height), (320, 240), "a taller target must not upscale");
}

#[test]
fn speed_scales_video_and_audio_durations() {
    let Some(clip) = clip("speed") else { return };
    let plan = plan(vec![
        TimeRange { start: 0.5, end: 1.5, speed: 0.5 },
        TimeRange { start: 2.0, end: 4.0, speed: 2.0 },
    ]);
    let expected = plan.duration();
    assert!((expected - 3.0).abs() < 1e-9);
    let report = run(&clip, "speed.mp4", &plan, &ExportSettings::default()).unwrap();
    assert!(within(report.seconds, expected, 0.01), "video {} s, expected {expected} s", report.seconds);
    assert!(within(report.ranges[0].output_end, 2.0, 1e-6));
    let probed = probe(std::path::Path::new(&clip.output("speed.mp4")));
    let frames = probed.video.len() as f64;
    assert!(within(frames / 60.0, expected, 0.01), "{frames} frames for {expected} s");
    for (track, samples) in probed.audio_samples.iter().enumerate() {
        let seconds = *samples as f64 / RATE as f64;
        assert!(within(seconds, expected, 0.01), "audio track {track}: {seconds} s, expected {expected} s");
    }
    // Pitch survives the stretch: 440 Hz and 660 Hz tones stay put at 0.5x and 2x.
    for (pcm, hz) in probed.audio_pcm.iter().zip([440.0, 660.0]) {
        let slow = &pcm[..(2.0 * RATE as f64) as usize];
        let fast = &pcm[(2.0 * RATE as f64) as usize..];
        for part in [slow, fast] {
            let (rms, measured) = tone(part);
            assert!(rms > 0.15, "stretched audio is too quiet: rms {rms}");
            assert!(within(measured, hz, 0.03), "pitch moved: {measured:.1} Hz for {hz} Hz");
        }
    }
    eprintln!("speed: video {frames} frames, audio samples {:?}", probed.audio_samples);
}

/// Exports `plan` with and without the seek/early stop; returns the decoded
/// frame counts (full, trimmed) after checking the outputs are identical.
fn trimmed_matches_full(clip: &Clip, tag: &str, plan: &EditPlan) -> (u64, u64) {
    use super::edited::DECODED;
    use super::timeline::TRIM_DECODE;
    let pass = |trim: bool| {
        TRIM_DECODE.with(|t| t.set(trim));
        let name = format!("{tag}-{trim}.mp4");
        let report = run(clip, &name, plan, &ExportSettings::default());
        TRIM_DECODE.with(|t| t.set(true));
        let report = report.unwrap();
        (report.frames, DECODED.with(|d| d.get()), probe(std::path::Path::new(&clip.output(&name))))
    };
    let (full_frames, full_decoded, full) = pass(false);
    let (frames, decoded, trimmed) = pass(true);
    assert_eq!(frames, full_frames);
    assert_eq!(trimmed.video, full.video, "{tag}: trimmed decode changed the video");
    assert_eq!(trimmed.audio, full.audio, "{tag}: trimmed decode changed the audio");
    (full_decoded, decoded)
}

#[test]
fn late_range_export_seeks_and_stops_without_changing_frames() {
    let spec = Spec { seconds: 10.0, ..Spec::default() };
    let Some(clip) = synth("seek", &spec) else { return };
    let (full, trimmed) = trimmed_matches_full(&clip, "late", &plan(vec![TimeRange::new(6.1, 7.0)]));
    assert!(full >= 590, "reference decoded only {full} frames");
    // 1 s margin + up to one 0.5 s GOP before 6.1 s, the 0.9 s range, and the
    // frames still in the threaded decoder when reading stops (at most 16).
    assert!(trimmed <= 170, "late range decoded {trimmed} of {full} frames");

    let (full, trimmed) = trimmed_matches_full(
        &clip,
        "split",
        &plan(vec![TimeRange::new(0.5, 1.0), TimeRange { start: 5.0, end: 6.0, speed: 2.0 }]),
    );
    assert!(trimmed <= 400 && trimmed < full, "split ranges decoded {trimmed} of {full} frames");
}

#[test]
fn unit_speed_export_matches_the_kept_length() {
    let Some(clip) = clip("unit") else { return };
    let plan = plan(vec![TimeRange::new(0.25, 1.25), TimeRange::new(3.0, 3.5)]);
    let report = run(&clip, "unit.mp4", &plan, &ExportSettings::default()).unwrap();
    assert_eq!(report.frames, 90);
    let probed = probe(std::path::Path::new(&clip.output("unit.mp4")));
    assert_eq!(probed.video.len(), 90);
    assert_eq!(probed.audio.len(), 2);
    let names: Vec<_> = TRACK_NAMES.iter().map(|n| Some(n.to_string())).collect();
    assert_eq!(probed.audio_handlers, names, "track names must survive a re-encoded export");
    for (samples, (pcm, hz)) in probed.audio_samples.iter().zip(probed.audio_pcm.iter().zip([440.0, 660.0])) {
        assert!(within(*samples as f64, 1.5 * RATE as f64, 0.01), "{samples} samples");
        let (rms, measured) = tone(pcm);
        assert!(rms > 0.15 && within(measured, hz, 0.03), "track tone {measured:.1} Hz rms {rms}");
    }
}
