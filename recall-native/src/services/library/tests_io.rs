//! Tests that touch the filesystem — only inside fresh temp folders — plus one
//! read-only check of a real clip.
use std::io::Cursor;
use std::path::{Path, PathBuf};

use super::mp4::{duration_of, last_frame_of, last_presented, video_duration_of};
use super::store::meta_path;
use super::*;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("recall-native-lib-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn store_round_trip_prunes_and_validates_duration_identity() {
    let dir = temp_dir("store");
    let path = dir.join("Recall").join("library.json");
    let mut store = MetaStore::load_from(path.clone());
    assert_eq!(store.duration("a.mp4", "a.mp4", 1, 2), None);
    store.set_favourite("a.mp4", true).unwrap();
    store.set_title("a.mp4", Some("  Ace  ")).unwrap();
    store.set_title("b.mp4", Some("temp")).unwrap();
    store.set_title("b.mp4", Some("   ")).unwrap();
    store.set_duration("c.mp4", 100, 200, 119.146);
    store.set_duration("bad.mp4", 1, 1, f64::NAN);
    store.flush().unwrap();
    let reloaded = MetaStore::load_from(path.clone());
    assert!(reloaded.meta("a.mp4").unwrap().favourite);
    assert_eq!(reloaded.meta("a.mp4").unwrap().title.as_deref(), Some("Ace"));
    assert!(reloaded.meta("b.mp4").is_none(), "cleared title leaves no entry");
    assert!(reloaded.meta("bad.mp4").is_none());
    assert_eq!(reloaded.duration("c.mp4", "c.mp4", 100, 200), Some(119.146));
    assert_eq!(reloaded.duration("c.mp4", "c.mp4", 101, 200), None, "size changed");
    assert_eq!(reloaded.duration("c.mp4", "c.mp4", 100, 201), None, "mtime changed");
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(json["a.mp4"], serde_json::json!({"favourite": true, "title": "Ace"}));
    assert_eq!(json["c.mp4"]["duration"], serde_json::json!({"bytes": 100, "mtime": 200, "secs": 119.146}));
    let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).collect();
    assert!(leftovers.is_empty(), "atomic write leaves no temp file");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn store_keeps_the_played_stamp_even_alone() {
    let dir = temp_dir("played");
    let path = dir.join("library.json");
    let mut store = MetaStore::load_from(path.clone());
    store.set_played("a.mp4", 1_700_000_123).unwrap();
    store.set_favourite("b.mp4", true).unwrap();
    store.set_played("b.mp4", 5).unwrap();
    let reloaded = MetaStore::load_from(path.clone());
    assert_eq!(reloaded.meta("a.mp4").unwrap().played, Some(1_700_000_123), "an entry with only a stamp is kept");
    assert_eq!(reloaded.meta("b.mp4").unwrap().played, Some(5));
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(json["a.mp4"], serde_json::json!({"played": 1_700_000_123}));
    assert_eq!(json["b.mp4"], serde_json::json!({"favourite": true, "played": 5}));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn corrupt_store_is_backed_up_and_replaced() {
    let dir = temp_dir("corrupt");
    let path = dir.join("library.json");
    std::fs::write(&path, b"{ not json").unwrap();
    let mut store = MetaStore::load_from(path.clone());
    assert!(store.meta("x.mp4").is_none());
    let bad = crate::services::durable::bad_path(&path, std::time::SystemTime::now());
    assert!(bad.file_name().unwrap().to_string_lossy().starts_with("library.json.bad-"));
    assert_eq!(std::fs::read(&bad).unwrap(), b"{ not json");
    store.set_favourite("x.mp4", true).unwrap();
    assert!(MetaStore::load_from(path).meta("x.mp4").unwrap().favourite);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn store_drops_only_malformed_entries() {
    let dir = temp_dir("entries");
    let path = dir.join("library.json");
    std::fs::write(&path, br#"{"a.mp4":{"favourite":"yes"},"b.mp4":{"favourite":true,"future":1},"c.mp4":{}}"#).unwrap();
    let store = MetaStore::load_from(path.clone());
    assert!(store.meta("a.mp4").is_none());
    assert!(store.meta("b.mp4").unwrap().favourite);
    assert!(store.meta("c.mp4").is_none());
    assert!(!crate::services::durable::bad_path(&path, std::time::SystemTime::now()).exists(), "a valid object is not quarantined");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn store_path_resolution_and_in_memory_default() {
    assert_eq!(meta_path(Some("D:\\x\\meta.json".into()), Some("C:\\AppData".into())), Some(PathBuf::from("D:\\x\\meta.json")));
    assert_eq!(meta_path(Some("".into()), Some("C:\\AppData".into())), Some(PathBuf::from("C:\\AppData\\Recall\\library.json")));
    assert_eq!(meta_path(None, None), None);
    let mut memory = MetaStore::default();
    memory.set_favourite("a.mp4", true).unwrap();
    assert!(memory.meta("a.mp4").unwrap().favourite);
    memory.flush().unwrap();
}

fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

fn large_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = 1u32.to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(&((payload.len() + 16) as u64).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn mvhd(version: u8, timescale: u32, duration: u64) -> Vec<u8> {
    let mut payload = vec![version, 0, 0, 0];
    if version == 1 {
        payload.extend_from_slice(&[0u8; 16]);
        payload.extend_from_slice(&timescale.to_be_bytes());
        payload.extend_from_slice(&duration.to_be_bytes());
    } else {
        payload.extend_from_slice(&[0u8; 8]);
        payload.extend_from_slice(&timescale.to_be_bytes());
        payload.extend_from_slice(&(duration as u32).to_be_bytes());
    }
    payload.extend_from_slice(&[0u8; 80]);
    mp4_box(b"mvhd", &payload)
}

fn moov(children: &[Vec<u8>]) -> Vec<u8> {
    mp4_box(b"moov", &children.concat())
}

#[test]
fn mp4_parser_finds_moov_after_a_64_bit_mdat() {
    let ftyp = mp4_box(b"ftyp", b"isom\0\0\x02\0isomiso2avc1mp41");
    let mdat = large_box(b"mdat", &vec![0xAB; 4096]);
    let trak = mp4_box(b"trak", &[0u8; 32]);
    let file = [ftyp, mp4_box(b"free", &[]), mdat, moov(&[trak, mvhd(0, 1000, 119_146)])].concat();
    let seconds = duration_of(&mut Cursor::new(file)).unwrap();
    assert!((seconds - 119.146).abs() < 1e-9, "{seconds}");
}

#[test]
fn mp4_parser_versions_size_zero_and_fragments() {
    let v1 = [moov(&[mvhd(1, 90_000, 90_000 * 3600)]), mp4_box(b"mdat", &[0; 16])].concat();
    assert_eq!(duration_of(&mut Cursor::new(v1)), Some(3600.0));
    // A size-0 mdat runs to end of file; moov before it.
    let mut tail = [mp4_box(b"ftyp", b"isom"), moov(&[mvhd(0, 600, 1200)])].concat();
    tail.extend_from_slice(&0u32.to_be_bytes());
    tail.extend_from_slice(b"mdat");
    tail.extend_from_slice(&[0; 64]);
    assert_eq!(duration_of(&mut Cursor::new(tail)), Some(2.0));
    let mehd = mp4_box(b"mehd", &[[0u8, 0, 0, 0].as_slice(), &5000u32.to_be_bytes()].concat());
    let fragmented = moov(&[mvhd(0, 1000, 0), mp4_box(b"mvex", &mehd)]);
    assert_eq!(duration_of(&mut Cursor::new(fragmented)), Some(5.0));
}

#[test]
fn mp4_parser_rejects_broken_input() {
    assert_eq!(duration_of(&mut Cursor::new(Vec::new())), None);
    assert_eq!(duration_of(&mut Cursor::new(b"garbage bytes that are not boxes".to_vec())), None);
    let no_moov = [mp4_box(b"ftyp", b"isom"), mp4_box(b"mdat", &[0; 8])].concat();
    assert_eq!(duration_of(&mut Cursor::new(no_moov)), None);
    let zero_scale = moov(&[mvhd(0, 0, 100)]);
    assert_eq!(duration_of(&mut Cursor::new(zero_scale)), None);
    let unknown = moov(&[mvhd(0, 1000, u64::from(u32::MAX))]);
    assert_eq!(duration_of(&mut Cursor::new(unknown)), None);
    let mut truncated = moov(&[mvhd(0, 1000, 5000)]);
    truncated.truncate(30);
    assert_eq!(duration_of(&mut Cursor::new(truncated)), None);
}

fn trak(handler: &[u8; 4], timescale: u32, duration: u64) -> Vec<u8> {
    let hdlr = mp4_box(b"hdlr", &[[0u8; 8].as_slice(), handler, &[0u8; 12]].concat());
    let mut mdhd = mvhd(0, timescale, duration);
    mdhd[4..8].copy_from_slice(b"mdhd");
    mp4_box(b"trak", &mp4_box(b"mdia", &[hdlr, mdhd].concat()))
}

#[test]
fn mp4_video_track_length_skips_sound_tracks() {
    // Sound runs to 10.453 s (mvhd); the picture stops at 10.437942 s.
    let file = moov(&[mvhd(0, 1000, 10_453), trak(b"soun", 48_000, 501_744), trak(b"vide", 1_000_000, 10_437_942)]);
    let seconds = video_duration_of(&mut Cursor::new(file)).unwrap();
    assert!((seconds - 10.437942).abs() < 1e-9, "{seconds}");
    let sound_only = moov(&[mvhd(0, 1000, 5000), trak(b"soun", 1000, 5000)]);
    assert_eq!(video_duration_of(&mut Cursor::new(sound_only)), None);
    assert_eq!(video_duration_of(&mut Cursor::new(b"not an mp4".to_vec())), None);
}

#[test]
fn mp4_last_frame_is_where_the_samples_say_not_where_the_track_ends() {
    let run = |entries: &[(u32, u32)]| {
        let mut payload = [[0u8; 4].as_slice(), &(entries.len() as u32).to_be_bytes()].concat();
        entries.iter().for_each(|(count, value)| payload.extend([count.to_be_bytes(), value.to_be_bytes()].concat()));
        payload
    };
    // A stream-copied cut: 3 frames of 100 units, the last one held for 533.
    let stts = mp4_box(b"stts", &run(&[(3, 100), (1, 533)]));
    let stbl = mp4_box(b"stbl", &stts);
    let minf = mp4_box(b"minf", &stbl);
    let hdlr = mp4_box(b"hdlr", &[[0u8; 8].as_slice(), b"vide", &[0u8; 12]].concat());
    let mut mdhd = mvhd(0, 1000, 833);
    mdhd[4..8].copy_from_slice(b"mdhd");
    let track = mp4_box(b"trak", &mp4_box(b"mdia", &[hdlr, mdhd, minf].concat()));
    let file = moov(&[mvhd(0, 1000, 833), track]);
    assert_eq!(last_frame_of(&mut Cursor::new(file)), Some(0.3));
    let no_tables = moov(&[trak(b"vide", 1000, 833)]);
    assert_eq!(last_frame_of(&mut Cursor::new(no_tables)), None);
}

#[test]
fn mp4_last_presented_follows_display_order() {
    assert_eq!(last_presented(&[(4, 10)], &[]), Some(30));
    // B-frames: decode 0 10 20 30, shown 10 40 20 30.
    assert_eq!(last_presented(&[(4, 10)], &[(1, 10), (1, 30), (2, 0)]), Some(40));
    assert_eq!(last_presented(&[(3, 100), (1, 533)], &[(4, 0)]), Some(300));
    assert_eq!(last_presented(&[], &[]), None);
}

/// Read-only: the real recording; ffprobe reports format duration 119.146000 s.
#[test]
fn mp4_duration_matches_ffprobe_on_a_real_clip() {
    let clip = Path::new(r"E:\Clips\VALORANT__2026-10-17__22-21-36-788__adaa986dc538484698bbabf313f2fa1e.mp4");
    if !clip.is_file() {
        eprintln!("skipped: {} not present", clip.display());
        return;
    }
    let seconds = mp4_duration(clip).expect("mvhd duration");
    eprintln!("mp4_duration(real clip) = {seconds:.6}");
    assert!((seconds - 119.146).abs() < 0.01, "{seconds}");
}

#[test]
fn library_delete_refuses_files_their_bin_cannot_take() {
    let dir = temp_dir("refuse");
    let clip = dir.join("RecallNativeTest__2026-10-22__11-00-00.mp4");
    std::fs::write(&clip, b"x").unwrap();
    fn no_bin(_: &Path, _: u64) -> Result<(), String> {
        Err("the drive has no Recycle Bin".into())
    }
    let err = win::check_recyclable(std::slice::from_ref(&clip), no_bin, crate::win_recycle::file_size).unwrap_err();
    assert!(err.contains("Nothing was deleted"), "{err}");
    assert!(clip.exists(), "the clip stays");
    assert!(win::check_recyclable(std::slice::from_ref(&clip), |_, _| Ok(()), crate::win_recycle::file_size).is_ok());
    // The clip and its companions are summed: two 600 MB files do not fit 1 GB of room.
    let pair = [clip.clone(), dir.join("RecallNativeTest__2026-10-22__11-00-00.mp4.recall-edit.json")];
    fn one_gb_room(_: &Path, bytes: u64) -> Result<(), String> {
        if bytes > 1_000_000_000 { Err("no room left".into()) } else { Ok(()) }
    }
    let err = win::check_recyclable(&pair, one_gb_room, |_| Some(600_000_000)).unwrap_err();
    assert!(err.contains("no room left"), "{err}");
    // A size that cannot be read refuses instead of counting 0.
    let err = win::check_recyclable(&pair, |_, _| Ok(()), |p| (!p.to_string_lossy().ends_with(".json")).then_some(1)).unwrap_err();
    assert!(err.contains("couldn't read the size"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recycle_bin_moves_clip_thumbs_sidecar_and_marks_only() {
    let dir = temp_dir("recycle");
    let stem = "RecallNativeTest__2026-10-22__10-00-00";
    let clip = dir.join(format!("{stem}.mp4"));
    let thumbs = dir.join("thumbs");
    std::fs::create_dir_all(&thumbs).unwrap();
    std::fs::write(&clip, b"not really a video").unwrap();
    let thumb_a = thumbs.join(format!("{stem}-1783463424-thumb.jpg"));
    let thumb_b = thumbs.join(format!("{stem}-1783463999-thumb.jpg"));
    let other_thumb = thumbs.join("RecallNativeTest__2026-10-22__10-00-01-1783463424-thumb.jpg");
    let sidecar = dir.join(format!("{stem}.mp4.recall-edit.json"));
    let marks = dir.join(format!("{stem}.recall-marks.json"));
    let other_clip = dir.join("RecallNativeTest__2026-10-22__10-00-01.mp4");
    let other_marks = dir.join("RecallNativeTest__2026-10-22__10-00-01.recall-marks.json");
    for file in [&thumb_a, &thumb_b, &other_thumb, &sidecar, &marks, &other_clip, &other_marks] {
        std::fs::write(file, b"x").unwrap();
    }
    let (clips, files) = win::recycle_targets(std::slice::from_ref(&clip));
    assert_eq!(clips.len(), 1);
    assert_eq!(files.len(), 5, "{files:?}");
    assert_eq!(delete_to_recycle_bin(std::slice::from_ref(&clip)).unwrap(), 1);
    for gone in [&clip, &thumb_a, &thumb_b, &sidecar, &marks] {
        assert!(!gone.exists(), "{} still present", gone.display());
    }
    assert!(other_thumb.exists() && other_clip.exists() && other_marks.exists(), "neighbours untouched");
    assert_eq!(delete_to_recycle_bin(&[dir.join("missing.mp4")]).unwrap(), 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn drive_space_reports_the_volume() {
    let dir = temp_dir("space");
    let (free, total) = drive_space(&dir).expect("temp volume");
    assert!(total > 0 && free <= total);
    assert!(drive_space(&dir.join("not").join("yet")).is_some(), "walks up to an existing folder");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn local_time_is_a_whole_zone_offset_from_utc() {
    let secs = 1_792_256_671u64; // 2026-10-17 17:04:31 UTC
    let local = local_from_unix(secs);
    let local_days = calendar::days_from_civil(local.year, local.month, local.day);
    let local_secs = local_days * 86_400 + i64::from(local.hour * 3600 + local.minute * 60 + local.second);
    let offset = local_secs - secs as i64;
    assert_eq!(offset % 900, 0, "offset {offset}s");
    assert!(offset.abs() <= 14 * 3600);
    eprintln!("local offset = {offset}s; local = {local:?}");
    assert!(today().year >= 2026);
}

#[test]
fn build_records_reads_names_store_and_sidecars() {
    let dir = temp_dir("records");
    let names = ["VALORANT__2026-10-17__22-34-31-275__3d260146faca4115835ba29741d71fe3.mp4", "Rocket-League®__2026-10-16__10-00-00.mp4", "lethal company moment.mp4"];
    std::fs::write(dir.join(format!("{}.recall-edit.json", names[0])), b"{}").unwrap();
    std::fs::write(dir.join("Rocket-League®__2026-10-16__10-00-00.recall-edit.json"), b"{}").unwrap();
    let entries: Vec<ClipEntry> = names
        .iter()
        .map(|name| ClipEntry { name: (*name).into(), path: dir.join(name).to_string_lossy().into_owned(), bytes: 1000, mtime: 1_792_256_671, thumb: None, folder: None })
        .collect();
    let mut store = MetaStore::default();
    store.set_favourite(names[1], true).unwrap();
    store.set_title(names[0], Some("Clutch")).unwrap();
    store.set_duration(names[0], 1000, 1_792_256_671, 119.146);
    let records = build_records(&entries, &store);
    assert_eq!(records[0].game, "VALORANT");
    assert_eq!(records[0].started, LocalStamp { year: 2026, month: 10, day: 17, hour: 22, minute: 34, second: 31 });
    assert_eq!(records[0].duration, Some(119.146));
    assert_eq!(records[0].display_title(), "Clutch");
    assert!(records[0].edited, "<file>.recall-edit.json");
    assert_eq!((records[1].game_key.as_str(), records[1].game.as_str()), ("rocketleague", "Rocket League"));
    assert!(records[1].edited, "<stem>.recall-edit.json");
    assert!(records[1].favourite);
    assert_eq!(records[2].game, "lethal company moment");
    assert_eq!(records[2].started, local_from_unix(1_792_256_671), "undated → mtime");
    assert!(!records[2].edited && !records[2].favourite && records[2].duration.is_none());
    let _ = std::fs::remove_dir_all(dir);
}

/// Generates a 2-second test clip with ffmpeg in a temp folder (skips without ffmpeg),
/// then checks 8 scrub frames, the cache hit, and the parser on a real muxer's output.
#[test]
fn scrub_frames_extract_then_hit_cache() {
    let Ok(ffmpeg) = scrub::binary("RECALL_FFMPEG", "ffmpeg.exe") else {
        eprintln!("skipped: ffmpeg not found");
        return;
    };
    let dir = temp_dir("scrub");
    let clip = dir.join("clip.mp4");
    let status = std::process::Command::new(&ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-threads", "2", "-f", "lavfi", "-i", "testsrc=duration=2:size=160x90:rate=10", "-c:v", "mpeg4", "-y"])
        .arg(&clip)
        .status()
        .unwrap();
    assert!(status.success());
    let parsed = mp4_duration(&clip).unwrap();
    assert!((parsed - 2.0).abs() < 0.05, "{parsed}");
    let cache = dir.join("cache");
    let frames = scrub::frames_in(&cache, &clip.to_string_lossy()).unwrap();
    assert_eq!(frames.len(), scrub::SCRUB_FRAMES);
    for (index, frame) in frames.iter().enumerate() {
        assert!(frame.ends_with(&format!("scrub-{index}.jpg")), "{frame}");
        assert!(std::fs::metadata(frame).unwrap().len() > 100);
    }
    let modified = std::fs::metadata(&frames[0]).unwrap().modified().unwrap();
    assert_eq!(scrub::frames_in(&cache, &clip.to_string_lossy()).unwrap(), frames);
    assert_eq!(std::fs::metadata(&frames[0]).unwrap().modified().unwrap(), modified, "served from cache");
    let _ = std::fs::remove_dir_all(dir);
}

/// MKV (OBS's default) has no MP4 index: its length comes from ffprobe (skips without ffmpeg).
#[test]
fn an_mkv_clip_gets_its_length_from_ffprobe() {
    assert_eq!(resolve_duration(Path::new("x.mkv"), |_| None, |_| Some(12.5)), Some(12.5));
    assert_eq!(resolve_duration(Path::new("x.mp4"), |_| Some(3.0), |_| panic!("the index answered")), Some(3.0));
    assert_eq!(resolve_duration(Path::new("x.mp4"), |_| None, |_| Some(f64::NAN)), None);
    let Ok(ffmpeg) = scrub::binary("RECALL_FFMPEG", "ffmpeg.exe") else {
        eprintln!("skipped: ffmpeg not found");
        return;
    };
    let dir = temp_dir("mkv-duration");
    let clip = dir.join("obs.mkv");
    let status = std::process::Command::new(&ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-threads", "2", "-f", "lavfi", "-i", "testsrc=duration=2:size=160x90:rate=10", "-c:v", "mpeg4", "-y"])
        .arg(&clip)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(mp4_duration(&clip), None, "not an MP4");
    let secs = clip_duration(&clip).expect("ffprobe duration");
    assert!((secs - 2.0).abs() < 0.1, "{secs}");
    let _ = std::fs::remove_dir_all(dir);
}
