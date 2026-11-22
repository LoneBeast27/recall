use std::path::{Path, PathBuf};

use super::{WAVE_BINS, cached_entry};
use crate::edit_model::{AudioTrackInfo, ClipInfo};
use crate::services::storage;
use crate::test_temp::TempDir;

fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

/// A made-up clip whose index carries `recall_native_height` = `native` (none: no tags).
fn clip_bytes(native: Option<&str>) -> Vec<u8> {
    let mut moov = boxed(b"mvhd", &[0; 100]);
    if let Some(native) = native {
        let key = b"recall_native_height";
        let mut keys = vec![0, 0, 0, 0, 0, 0, 0, 1];
        keys.extend_from_slice(&((key.len() + 8) as u32).to_be_bytes());
        keys.extend_from_slice(b"mdta");
        keys.extend_from_slice(key);
        let mut data = vec![0, 0, 0, 1, 0, 0, 0, 0];
        data.extend_from_slice(native.as_bytes());
        let mut meta = vec![0; 4];
        meta.extend_from_slice(&boxed(b"keys", &keys));
        meta.extend_from_slice(&boxed(b"ilst", &boxed(&1u32.to_be_bytes(), &boxed(b"data", &data))));
        moov.extend_from_slice(&boxed(b"udta", &boxed(b"meta", &meta)));
    }
    let mut out = boxed(b"ftyp", b"isomiso2");
    out.extend_from_slice(&boxed(b"mdat", &[5; 256]));
    out.extend_from_slice(&boxed(b"moov", &moov));
    out
}

struct Setup {
    _dir: TempDir,
    editor: PathBuf,
    clip: PathBuf,
}

/// A clip, its engine entry as `--prepare-editor` publishes it, and one stem. `edit` changes
/// the manifest before it is written.
fn setup(name: &str, native: Option<&str>, edit: impl FnOnce(&mut ClipInfo)) -> Setup {
    let dir = TempDir::new(format!("recall-prepared-{name}-{}", std::process::id()));
    let clip = dir.join("Made-Up-Game__2026-01-01__00-00-00.mp4");
    std::fs::write(&clip, clip_bytes(native)).unwrap();
    let clip = std::fs::canonicalize(&clip).unwrap();
    let metadata = clip.metadata().unwrap();
    let editor = dir.join("editor");
    let entry = storage::engine_cache_dir(&editor, &clip, metadata.len(), super::modified_ms(&metadata));
    std::fs::create_dir_all(&entry).unwrap();
    let stem = entry.join("audio-1.flac");
    std::fs::write(&stem, b"fLaC").unwrap();
    let mut info = ClipInfo {
        path: clip.to_string_lossy().into_owned(),
        duration: 12.5,
        width: 1920,
        height: 1080,
        video_fps: 60.0,
        source_bytes: metadata.len(),
        source_modified_ms: super::modified_ms(&metadata),
        keyframes: vec![0.0, 2.0, 4.0],
        audio: vec![AudioTrackInfo { stream_index: 1, name: "Game".into(), channels: 2, sample_rate: 48_000, preview_path: stem.to_string_lossy().into_owned(), waveform: vec![0.5; WAVE_BINS], source: String::new() }],
        ..Default::default()
    };
    edit(&mut info);
    std::fs::write(entry.join("clip.json"), serde_json::to_vec(&info).unwrap()).unwrap();
    Setup { _dir: dir, editor, clip }
}

fn read(setup: &Setup) -> Option<ClipInfo> {
    cached_entry(&setup.editor, &setup.clip, &setup.clip)
}

#[test]
fn an_entry_the_engine_would_take_opens_without_the_engine() {
    let setup = setup("taken", Some("1440"), |_| {});
    let info = read(&setup).expect("a complete entry is read in the app");
    assert_eq!((info.duration, info.keyframes.len(), info.audio.len()), (12.5, 3, 1));
    assert_eq!(info.native_height, Some(1440), "the native height comes from the clip's tag, as the engine reads it");
}

#[test]
fn the_native_height_tag_counts_only_above_the_recorded_height() {
    assert_eq!(read(&setup("same", Some("1080"), |_| {})).unwrap().native_height, None);
    assert_eq!(read(&setup("untagged", None, |_| {})).unwrap().native_height, None);
    // A stale value in the manifest never survives: the tag is read afresh.
    assert_eq!(read(&setup("stale", None, |info| info.native_height = Some(2160))).unwrap().native_height, None);
}

#[test]
fn the_native_height_is_read_from_the_copy_when_one_is_given() {
    let setup = setup("copy", None, |_| {});
    let copy = setup.clip.with_file_name("copy.mp4");
    std::fs::write(&copy, clip_bytes(Some("2160"))).unwrap();
    assert_eq!(cached_entry(&setup.editor, &setup.clip, &copy).unwrap().native_height, Some(2160));
}

#[test]
fn entries_the_engine_would_redo_go_to_the_engine() {
    type Edit = fn(&mut ClipInfo);
    let cases: [(&str, Edit); 6] = [
        ("size", |info| info.source_bytes += 1),
        ("mtime", |info| info.source_modified_ms += 1),
        ("path", |info| info.path.push('x')),
        ("keyframes", |info| info.keyframes.clear()),
        ("waveform", |info| info.audio[0].waveform.pop().map(drop).unwrap_or(())),
        ("duration", |info| info.duration = f64::NAN),
    ];
    for (name, edit) in cases {
        assert!(read(&setup(name, None, edit)).is_none(), "{name}: the engine decides");
    }
}

#[test]
fn a_missing_stem_or_entry_goes_to_the_engine() {
    let setup = setup("stem", None, |info| info.audio[0].preview_path = "Z:\\gone\\audio-1.flac".into());
    assert!(read(&setup).is_none());
    let other = setup.clip.with_file_name("no-entry.mp4");
    std::fs::write(&other, clip_bytes(None)).unwrap();
    assert!(cached_entry(&setup.editor, &std::fs::canonicalize(&other).unwrap(), Path::new(&other)).is_none());
}

#[test]
fn only_an_open_that_wrote_to_the_cache_runs_a_budget_pass() {
    use super::super::adds_to_cache;
    assert!(!adds_to_cache(false, false, true), "a clip opened from its copy with stems and frames ready adds nothing");
    assert!(adds_to_cache(true, false, true), "a new SSD copy");
    assert!(adds_to_cache(false, true, true), "the engine wrote stems");
    assert!(adds_to_cache(false, false, false), "frames were extracted");
}
