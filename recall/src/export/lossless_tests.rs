//! Lossless trims and editor keyframes on synthetic clips.

use std::path::Path;

use ffmpeg_next as ff;

use super::keyframes::scan_keyframes;
use super::test_media::{probe, synth, Clip, Spec, RATE, TRACK_NAMES};
use super::timeline::timeline_origin_us;
use super::video_encoder::VideoBackend;
use super::{export_edited_with, EditedExport, ExportRequest};
use crate::edit_model::{snap_to_keyframe, AudioSetting, EditPlan, ExportMode, ExportSettings, TimeRange};

fn clip(tag: &str) -> Option<Clip> {
    let clip = synth(tag, &Spec::default());
    if clip.is_none() {
        eprintln!("libx264 missing in this FFmpeg build; skipped");
    }
    clip
}

fn lossless(clip: &Clip, name: &str, plan: &EditPlan) -> Result<EditedExport, String> {
    let request = ExportRequest {
        input: clip.path.to_string_lossy().into_owned(),
        output: clip.output(name),
        fps: 60,
    };
    let settings = ExportSettings { mode: ExportMode::Lossless, ..Default::default() };
    export_edited_with(&request, plan, &settings, VideoBackend::X264)
}

fn origin_us(path: &Path) -> i64 {
    let input = ff::format::input(path).unwrap();
    let video = input.streams().best(ff::media::Type::Video).unwrap().index();
    timeline_origin_us(&input, video)
}

fn us(seconds: f64) -> i64 {
    (seconds * 1e6).round() as i64
}

#[test]
fn lossless_trim_is_packet_identical_and_snapped_to_keyframes() {
    let Some(clip) = clip("lossless") else { return };
    let plan = EditPlan { mix_down: None,
        ranges: vec![TimeRange::new(1.2, 2.3), TimeRange::new(3.1, 4.05)],
        audio: vec![
            AudioSetting { stream_index: 1, gain_db: 0.0, muted: false, mix: false },
            AudioSetting { stream_index: 2, gain_db: -6.0, muted: false, mix: false },
        ],
    };
    let keyframes = scan_keyframes(&clip.path).unwrap();
    let report = lossless(&clip, "trim.mp4", &plan).unwrap();
    assert_eq!(report.mode, ExportMode::Lossless);
    assert_eq!(report.ranges.len(), 2);
    for (exported, requested) in report.ranges.iter().zip(&plan.ranges) {
        assert_eq!(exported.start, snap_to_keyframe(&keyframes, requested.start));
        assert!(exported.start <= requested.start && requested.start - exported.start < 0.51);
        assert!(exported.end >= requested.end && exported.end - requested.end < 0.02);
    }

    let source = probe(&clip.path);
    let origin = origin_us(&clip.path);
    let expected: Vec<u64> = report
        .ranges
        .iter()
        .flat_map(|r| {
            source.video.iter().filter(move |p| {
                let rel = p.pts_us - origin;
                rel >= us(r.start) && rel < us(r.end)
            })
        })
        .map(|p| p.hash)
        .collect();
    let output = probe(Path::new(&clip.output("trim.mp4")));
    let copied: Vec<u64> = output.video.iter().map(|p| p.hash).collect();
    assert!(!expected.is_empty());
    assert_eq!(copied, expected, "copied video payloads differ from the source window");
    assert_eq!(report.frames as usize, copied.len());
    assert!(output.video[0].key);
    assert_eq!((output.width, output.height), (320, 240));
    let names: Vec<_> = TRACK_NAMES.iter().map(|n| Some(n.to_string())).collect();
    assert_eq!(output.audio_handlers, names, "copied and re-encoded tracks keep their names");

    let total = report.ranges.iter().map(|r| r.end - r.start).sum::<f64>();
    assert!((report.seconds - total).abs() < 1e-6);
    // Track 1 (neutral) is stream-copied: every output packet is a source packet.
    let source_audio: std::collections::HashSet<_> = source.audio[0].iter().map(|p| p.hash).collect();
    assert!(output.audio[0].iter().all(|p| source_audio.contains(&p.hash)));
    let copied_seconds = output.audio[0].len() as f64 * 1024.0 / RATE as f64;
    assert!((copied_seconds - total).abs() < 0.05, "copied audio {copied_seconds} s for {total} s");
    // Track 2 (gain) is re-encoded over exactly the snapped ranges.
    let reencoded = output.audio_samples[1] as f64 / RATE as f64;
    assert!((reencoded - total).abs() <= total * 0.01, "re-encoded audio {reencoded} s for {total} s");
    let shared = output.audio[1]
        .iter()
        .filter(|p| source.audio[1].iter().any(|s| s.hash == p.hash))
        .count();
    assert!(shared * 10 < output.audio[1].len(), "gain track was copied instead of re-encoded");
    eprintln!(
        "lossless: {} video packets copied, ranges {:?}",
        copied.len(),
        report.ranges.iter().map(|r| (r.start, r.end)).collect::<Vec<_>>()
    );
}

#[test]
fn lossless_merges_cuts_that_snap_onto_the_same_gop() {
    let Some(clip) = clip("lossless-merge") else { return };
    let plan = EditPlan { mix_down: None,
        ranges: vec![TimeRange::new(1.1, 1.2), TimeRange::new(1.3, 1.4)],
        audio: vec![],
    };
    let report = lossless(&clip, "merged.mp4", &plan).unwrap();
    assert_eq!(report.ranges.len(), 1);
    assert_eq!(report.ranges[0].plan_ranges, vec![0, 1]);
    assert!(report.ranges[0].end >= 1.4);
}

#[test]
fn lossless_rejects_speed_changes_without_writing() {
    let Some(clip) = clip("lossless-speed") else { return };
    let plan = EditPlan { mix_down: None, ranges: vec![TimeRange { start: 1.0, end: 2.0, speed: 0.5 }], audio: vec![] };
    let error = lossless(&clip, "speed.mp4", &plan).unwrap_err();
    assert!(error.contains("cannot change speed"), "{error}");
    assert!(!Path::new(&clip.output("speed.mp4")).exists());
}

#[test]
fn prepare_editor_lists_keyframes_and_backfills_old_manifests() {
    let Some(clip) = clip("keyframes") else { return };
    let cache = clip.dir.join("cache");
    let cache_text = cache.to_string_lossy().into_owned();
    let source = clip.path.to_string_lossy().into_owned();
    let info = crate::editor_media::prepare_clip(&source, &cache_text).unwrap();
    let names: Vec<_> = info.audio.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, TRACK_NAMES, "track names come from handler_name");
    assert_eq!(info.keyframes, scan_keyframes(&clip.path).unwrap());
    assert_eq!(info.keyframes.len(), 10, "5 s at one keyframe per 30 frames: {:?}", info.keyframes);
    for pair in info.keyframes.windows(2) {
        assert!((pair[1] - pair[0] - 0.5).abs() < 0.005, "keyframe spacing {:?}", pair);
    }

    // A manifest from before keyframes existed is upgraded in place.
    let manifest = std::fs::read_dir(&cache)
        .unwrap()
        .flatten()
        .map(|e| e.path().join("clip.json"))
        .find(|p| p.is_file())
        .unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    value.as_object_mut().unwrap().remove("keyframes");
    for track in value["audio"].as_array_mut().unwrap() {
        track["name"] = "und".into(); // what title/language-only naming cached before
    }
    std::fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
    let again = crate::editor_media::prepare_clip(&source, &cache_text).unwrap();
    assert_eq!(again.keyframes, info.keyframes);
    let names: Vec<_> = again.audio.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, TRACK_NAMES, "stale cached names are refreshed");
    let stored: crate::edit_model::ClipInfo = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    assert_eq!(stored.keyframes, info.keyframes);
}

#[test]
fn track_names_skip_ffmpeg_placeholder_handler() {
    use crate::editor_media::track_name;
    let mut tags = ff::Dictionary::new();
    tags.set("handler_name", "SoundHandler");
    tags.set("language", "und");
    assert_eq!(track_name(&tags).as_deref(), Some("und"));
    tags.set("handler_name", " Voice chat ");
    assert_eq!(track_name(&tags).as_deref(), Some("Voice chat"));
    tags.set("title", "Discord");
    assert_eq!(track_name(&tags).as_deref(), Some("Discord"));
    assert_eq!(track_name(&ff::Dictionary::new()), None);
}

#[test]
fn lossless_export_is_faststart_and_reports_its_bytes() {
    let Some(clip) = clip("lossless-fast") else { return };
    let report = lossless(&clip, "fast.mp4", &EditPlan { mix_down: None, ranges: vec![TimeRange::new(1.0, 3.0)], audio: vec![] }).unwrap();
    let path = Path::new(&clip.output("fast.mp4")).to_owned();
    super::test_media::assert_faststart(&path);
    assert_eq!(report.bytes, std::fs::metadata(&path).unwrap().len());
}
