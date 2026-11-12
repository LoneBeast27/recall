//! Re-encoded edited exports on synthetic clips, driven through the production
//! pipeline with libx264 standing in for NVENC.

use super::test_media::{probe, synth, tone, Clip, Spec, RATE, TRACK_NAMES};
use super::video_encoder::VideoBackend;
use super::{export_edited_with, EditedExport, ExportRequest};
use crate::edit_model::{EditPlan, ExportPriority, ExportSettings, TimeRange};

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
    EditPlan { mix_down: None, ranges, audio: vec![] }
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
fn fit_priorities_change_the_encoded_shape_and_keep_audio_under_the_cap() {
    let Some(clip) = synth("cap-priority", &Spec { width: 1920, height: 1080, ..Spec::default() }) else {
        eprintln!("libx264 missing in this FFmpeg build; skipped");
        return;
    };
    let plan = plan(vec![TimeRange::new(0.0, 4.5)]);
    // Room for 720p at 60 fps but not 1080p: Smooth motion keeps 60 fps at its 720p floor,
    // Sharp detail drops to 30 fps for a taller picture.
    let cap = 3_200_000;
    let encode = |name, priority| {
        let settings = ExportSettings { max_bytes: Some(cap), priority, ..ExportSettings::default() };
        let report = run(&clip, name, &plan, &settings).unwrap();
        let file = clip.output(name);
        let probed = probe(std::path::Path::new(&file));
        assert_eq!((report.width, report.height), (probed.width, probed.height));
        assert_eq!(report.frames, probed.video.len() as u64);
        assert_eq!(probed.audio.len(), 2);
        assert!(within(report.seconds, 4.5, 0.01));
        assert_eq!(report.bytes, std::fs::metadata(file).unwrap().len());
        assert!(report.bytes <= cap, "{report:?}");
        report
    };
    let motion = encode("motion.mp4", ExportPriority::Motion);
    let detail = encode("detail.mp4", ExportPriority::Detail);
    assert_eq!((motion.fps, detail.fps), (Some(60), Some(30)));
    assert!(detail.height > motion.height && motion.height >= 720, "{motion:?} vs {detail:?}");
    eprintln!("Motion {}×{} at 60 fps, {} bytes. Detail {}×{} at 30 fps, {} bytes.",
        motion.width, motion.height, motion.bytes, detail.width, detail.height, detail.bytes);
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

#[test]
fn edited_export_is_faststart_and_reports_its_bytes() {
    let Some(clip) = clip("faststart") else { return };
    let report = run(&clip, "fast.mp4", &plan(vec![TimeRange::new(0.5, 2.0)]), &ExportSettings::default()).unwrap();
    let path = std::path::Path::new(&clip.output("fast.mp4")).to_owned();
    super::test_media::assert_faststart(&path);
    assert_eq!(report.bytes, std::fs::metadata(&path).unwrap().len());
}

/// Dev check, run on purpose: `cargo test ffprobe_sees_faststart -- --ignored`. Uses the
/// ffprobe in `FFMPEG_DIR\bin` (else PATH): the trace must open `moov` before `mdat`, and
/// every frame of both export kinds must decode with no error.
#[test]
#[ignore]
fn ffprobe_sees_faststart_and_a_clean_decode() {
    let ffprobe = std::env::var_os("FFMPEG_DIR")
        .map(|dir| std::path::Path::new(&dir).join("bin").join("ffprobe.exe"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| "ffprobe".into());
    let Some(clip) = clip("ffprobe") else { return };
    let edited = ExportSettings::default();
    let lossless = ExportSettings { mode: crate::edit_model::ExportMode::Lossless, ..Default::default() };
    for (name, settings) in [("edited.mp4", edited), ("lossless.mp4", lossless)] {
        run(&clip, name, &plan(vec![TimeRange::new(1.0, 3.0)]), &settings).unwrap();
        let path = clip.output(name);
        let trace = std::process::Command::new(&ffprobe).args(["-v", "trace", "-i", &path]).output().unwrap();
        let log = String::from_utf8_lossy(&trace.stderr);
        let (moov, mdat) = (log.find("type:'moov'").expect("moov in trace"), log.find("type:'mdat'").expect("mdat in trace"));
        assert!(moov < mdat, "{name}: ffprobe read mdat before moov");
        let decode = std::process::Command::new(&ffprobe)
            .args(["-v", "error", "-count_frames", "-show_entries", "stream=nb_read_frames", "-of", "csv=p=0", &path])
            .output()
            .unwrap();
        assert!(decode.status.success() && decode.stderr.is_empty(), "{name}: {}", String::from_utf8_lossy(&decode.stderr));
        eprintln!("{name}: frames per stream {}", String::from_utf8_lossy(&decode.stdout).trim().replace('\n', " "));
    }
}

fn low_clip(tag: &str) -> Option<Clip> {
    // A 320x240 recording of a 480-line source (the Recording resolution setting).
    synth(tag, &Spec { native_height: Some(480), ..Spec::default() })
}

#[test]
fn an_upscale_is_honoured_only_up_to_the_tagged_native_height() {
    let Some(clip) = low_clip("upscale") else { return };
    let ranges = || plan(vec![TimeRange::new(1.0, 2.0)]);
    let up = ExportSettings { upscale_height: Some(480), ..Default::default() };
    let report = run(&clip, "up.mp4", &ranges(), &up).unwrap();
    assert_eq!((report.width, report.height), (640, 480));
    let probed = probe(std::path::Path::new(&clip.output("up.mp4")));
    assert_eq!((probed.width, probed.height), (640, 480));

    let above = ExportSettings { upscale_height: Some(720), ..Default::default() };
    let report = run(&clip, "above.mp4", &ranges(), &above).unwrap();
    assert_eq!((report.width, report.height), (320, 240), "never above native");

    let Some(native) = synth("native-up", &Spec::default()) else { return };
    let report = run(&native, "untagged.mp4", &ranges(), &up).unwrap();
    assert_eq!((report.width, report.height), (320, 240), "a clip without the tag is never upscaled");
}

/// NVENC + GPU path, each upscaler: `RECALL_UPSCALER=ai|fsr|plain cargo test
/// gpu_upscaled_export -- --ignored --nocapture` (needs an NVIDIA GPU).
#[test]
#[ignore]
fn gpu_upscaled_export() {
    let Some(clip) = low_clip("gpu-upscale") else { return };
    let request = ExportRequest { input: clip.path.to_string_lossy().into_owned(), output: clip.output("gpu-up.mp4"), fps: 60 };
    let up = ExportSettings { upscale_height: Some(480), ..Default::default() };
    let report = export_edited_with(&request, &plan(vec![TimeRange::new(1.0, 3.0)]), &up, VideoBackend::Nvenc).unwrap();
    assert_eq!((report.width, report.height), (640, 480));
    let probed = probe(std::path::Path::new(&clip.output("gpu-up.mp4")));
    assert_eq!((probed.width, probed.height), (640, 480));
}

/// Each upscaler really changes a real export, not just its size: the same 720p clip
/// exported at 1440p through RTX Video Super Resolution, FSR 1 and the plain scale gives
/// three different encodes (NVENC is deterministic for identical frames). Needs an RTX GPU
/// with the driver switch on, and one test thread (it sets `RECALL_UPSCALER`):
/// `cargo test upscalers_change_a_real_export -- --ignored --test-threads=1`
#[test]
#[ignore]
fn upscalers_change_a_real_export() {
    let spec = Spec { width: 1280, height: 720, seconds: 2.0, native_height: Some(1440), ..Spec::default() };
    let Some(clip) = synth("upscalers", &spec) else { return };
    let up = ExportSettings { upscale_height: Some(1440), ..Default::default() };
    let export = |tier: &str| {
        // SAFETY (edition 2021: safe): one test thread, see the doc comment.
        std::env::set_var("RECALL_UPSCALER", tier);
        let name = format!("up-{tier}.mp4");
        let request = ExportRequest { input: clip.path.to_string_lossy().into_owned(), output: clip.output(&name), fps: 60 };
        let report = export_edited_with(&request, &plan(vec![TimeRange::new(0.5, 1.5)]), &up, VideoBackend::Nvenc).unwrap();
        assert_eq!((report.width, report.height), (2560, 1440), "{tier}");
        probe(std::path::Path::new(&clip.output(&name))).video.iter().map(|p| p.hash).collect::<Vec<_>>()
    };
    let (ai, fsr, plain) = (export("ai"), export("fsr"), export("plain"));
    std::env::remove_var("RECALL_UPSCALER");
    assert_ne!(ai, plain, "RTX Video Super Resolution left the export as the plain scale");
    assert_ne!(fsr, plain, "FSR 1 left the export as the plain scale");
    assert_ne!(ai, fsr);
}

/// A wide clip upscaled past H.264's 4096 a side is saved as HEVC, not refused at encoder
/// open: 2520x1080 recorded from a 2160-line game, exported at 2160p (5040x2160). NVIDIA GPU:
/// `cargo test a_wide_upscale_past_h264_saves_as_hevc -- --ignored`
#[test]
#[ignore]
fn a_wide_upscale_past_h264_saves_as_hevc() {
    let spec = Spec { width: 2520, height: 1080, seconds: 1.5, native_height: Some(2160), ..Spec::default() };
    let Some(clip) = synth("wide-up", &spec) else { return };
    let request = ExportRequest { input: clip.path.to_string_lossy().into_owned(), output: clip.output("wide.mp4"), fps: 60 };
    let up = ExportSettings { upscale_height: Some(2160), ..Default::default() };
    let report = export_edited_with(&request, &plan(vec![TimeRange::new(0.2, 1.2)]), &up, VideoBackend::Nvenc).unwrap();
    assert_eq!((report.width, report.height), (5040, 2160));
    use ffmpeg_next as ff;
    let input = ff::format::input(&clip.output("wide.mp4")).unwrap();
    let video = input.streams().best(ff::media::Type::Video).unwrap();
    assert_eq!(video.parameters().id(), ff::codec::Id::HEVC);
}
