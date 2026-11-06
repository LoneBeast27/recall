//! Free-space choice and the saver's fallbacks (injected free space, temp dirs; no disk is filled).

use super::*;
use crate::encoder::events::Events;
use crate::encoder::saver::{spawn_checked, GalleryFn, MuxFn, Muxed, SaverControl, SaverQueue, Shutdown, Timing};
use crate::encoder::{mover, staging, StillSpec};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MIB: u64 = 1 << 20;
const CLIP: &str = "G__2026-11-07__10-00-00-000__ab12.mp4";
const FULL: &str = "write_header: There is not enough space on the disk. (os error 112)";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("recall-space-{tag}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn job(path: &Path, secs: f64) -> SaveJob {
    SaveJob {
        path: path.to_string_lossy().into_owned(),
        seconds: 2,
        requested: Instant::now(),
        secs,
        video_end_us: None,
        video: VideoSrc::Still {
            frame: (2, 2, Arc::new(vec![0; 16])),
            spec: StillSpec {
                fps: 60,
                bitrate: 8_000_000,
                codec: crate::encoder::settings::Codec::H264,
                quality: crate::encoder::settings::Quality::High,
            },
        },
        backend: "test",
        audio: Vec::new(),
        thumb: None,
        timing: Timing::default(),
    }
}

/// Folders tagged "short" have 1 MiB free; everything else is unknown (room).
fn short_tagged(dir: &Path) -> Option<u64> {
    dir.to_string_lossy().contains("short").then_some(MIB)
}

fn no_room_anywhere(_: &Path) -> Option<u64> {
    Some(MIB)
}

fn plenty(_: &Path) -> Option<u64> {
    Some(1 << 40)
}

fn staged_setup(stage: &Path, out: &Path) -> staging::Setup {
    staging::Setup {
        dir: Some(stage.to_path_buf()),
        recover: None,
        out_dir: out.to_path_buf(),
        mover: mover::Config {
            chunk: 4096,
            sync_every: 16_384,
            retry_after: Duration::from_millis(50),
            same_volume: |_, _| false,
            background: false,
            stamp: mover::stamp_mtime,
        },
    }
}

fn spawn_with(stage: &Path, out: &Path, mux: MuxFn, free: FreeSpace) -> (SaverQueue, SaverControl, Arc<Mutex<Vec<String>>>) {
    let (events, sink) = Events::capture(true);
    let gallery: GalleryFn = Box::new(|_, _, _, _| {});
    let (queue, control) = spawn_checked(events, mux, gallery, Some(staged_setup(stage, out)), free);
    (queue, control, sink)
}

/// The first `saved` / `save_failed` event as JSON.
fn outcome(sink: &Arc<Mutex<Vec<String>>>) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let all = sink.lock().unwrap().clone();
        if let Some(line) = all.iter().find(|l| l.starts_with(r#"{"type":"saved""#) || l.starts_with(r#"{"type":"save_failed""#)) {
            return serde_json::from_str(line).unwrap();
        }
        assert!(Instant::now() < deadline, "no save outcome: {all:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Writes 10 bytes, except into a folder named like `fail_in` (disk full there).
fn mux_full_in(fail_in: &'static str, calls: Arc<AtomicUsize>) -> MuxFn {
    Box::new(move |part, _job, _abort, _timing| {
        calls.fetch_add(1, SeqCst);
        if part.to_string_lossy().contains(fail_in) {
            return Err(FULL.into());
        }
        std::fs::write(part, b"0123456789").unwrap();
        Ok(Muxed { frames: 3, secs: 1.5 })
    })
}

fn finish(queue: SaverQueue, control: SaverControl, dirs: &[&Path]) {
    drop(queue);
    assert_eq!(control.shutdown(Duration::from_secs(3)), Shutdown::Drained);
    for d in dirs {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn estimate_is_video_plus_audio_plus_margin() {
    let j = job(Path::new(r"E:\Clips\a.mp4"), 2.0);
    // 8 Mbit/s still for 2 s = 2 MB, no audio tracks.
    assert_eq!(estimate(&j), 2_000_000 + MARGIN);
}

#[test]
fn disk_full_errors_are_recognised() {
    assert!(is_disk_full(FULL));
    assert!(is_disk_full("finalize E:\\a.mp4: (os error 39)"));
    assert!(is_disk_full("av_interleaved_write_frame: No space left on device"));
    assert!(!is_disk_full("replay buffer empty (no keyframe yet)"));
    assert!(!is_disk_full("open x: Access is denied. (os error 5)"));
}

#[test]
fn choice_falls_back_in_place_and_names_both_drives() {
    let staged = PathBuf::from(r"F:\Recall\cache\staging").join(CLIP);
    let clip = PathBuf::from(r"E:\Clips").join(CLIP);
    let need = 300 * MIB;
    assert_eq!(choose(Some(staged.clone()), &clip, need, plenty).unwrap(), (Some(staged.clone()), None));
    let (to, note) = choose(Some(staged.clone()), &clip, need, |d| d.starts_with("F:").then_some(MIB)).unwrap();
    assert_eq!(to, None, "staging short -> in place");
    assert!(note.unwrap().contains("staging drive F:"));
    assert_eq!(choose(Some(staged), &clip, need, no_room_anywhere).unwrap_err(), "Not enough space on F: or E: (need 300 MB)");
    assert_eq!(choose(None, &clip, need, no_room_anywhere).unwrap_err(), "Not enough space on E: (need 300 MB)");
    assert_eq!(choose(None, &clip, need, |_| None).unwrap(), (None, None), "unknown counts as room");
}

#[test]
fn staging_drive_short_writes_the_clip_in_place() {
    let (stage, out) = (temp_dir("short-stage"), temp_dir("out"));
    let calls = Arc::new(AtomicUsize::new(0));
    let (queue, control, sink) = spawn_with(&stage, &out, mux_full_in("never", calls.clone()), short_tagged);
    let final_path = out.join(CLIP);
    assert!(queue.submit(job(&final_path, 1.5)));
    let saved = outcome(&sink);
    assert_eq!(saved["type"], "saved", "{saved}");
    assert_eq!(saved["staged"], false, "written in place: {saved}");
    assert_eq!(std::fs::read(&final_path).unwrap(), b"0123456789");
    assert_eq!(calls.load(SeqCst), 1, "staging was skipped up front");
    assert!(sink.lock().unwrap().iter().any(|l| l.contains("short of space")));
    finish(queue, control, &[&stage, &out]);
}

#[test]
fn disk_full_staged_mux_is_retried_in_place() {
    let (stage, out) = (temp_dir("stage"), temp_dir("out"));
    let calls = Arc::new(AtomicUsize::new(0));
    let (queue, control, sink) = spawn_with(&stage, &out, mux_full_in("recall-space-stage", calls.clone()), plenty);
    let final_path = out.join(CLIP);
    assert!(queue.submit(job(&final_path, 1.5)));
    let saved = outcome(&sink);
    assert_eq!(saved["type"], "saved", "{saved}");
    assert_eq!(saved["staged"], false, "{saved}");
    assert_eq!(std::fs::read(&final_path).unwrap(), b"0123456789");
    assert_eq!(calls.load(SeqCst), 2, "staged try, then in place");
    assert_eq!(std::fs::read_dir(&stage).unwrap().count(), 0, "no .part left on the staging drive");
    finish(queue, control, &[&stage, &out]);
}

#[test]
fn both_drives_short_fails_with_the_space_reason() {
    let (stage, out) = (temp_dir("stage"), temp_dir("out"));
    let calls = Arc::new(AtomicUsize::new(0));
    let (queue, control, sink) = spawn_with(&stage, &out, mux_full_in("never", calls.clone()), no_room_anywhere);
    let final_path = out.join(CLIP);
    assert!(queue.submit(job(&final_path, 1.5)));
    let failed = outcome(&sink);
    assert_eq!(failed["type"], "save_failed", "{failed}");
    let reason = failed["reason"].as_str().unwrap();
    let drive = drive(&out);
    assert_eq!(reason, format!("Not enough space on {drive} (need {})", size(1_500_000 + MARGIN)));
    assert_eq!(calls.load(SeqCst), 0, "nothing written");
    assert!(!final_path.exists());
    finish(queue, control, &[&stage, &out]);
}

#[test]
fn disk_full_staged_mux_with_a_short_clip_drive_fails_with_the_space_reason() {
    let (stage, out) = (temp_dir("stage"), temp_dir("short-out"));
    let calls = Arc::new(AtomicUsize::new(0));
    let (queue, control, sink) = spawn_with(&stage, &out, mux_full_in("recall-space-stage", calls.clone()), short_tagged);
    let final_path = out.join(CLIP);
    assert!(queue.submit(job(&final_path, 1.5)));
    let failed = outcome(&sink);
    assert_eq!(failed["type"], "save_failed", "{failed}");
    assert!(failed["reason"].as_str().unwrap().starts_with("Not enough space on "), "{failed}");
    assert_eq!(calls.load(SeqCst), 1, "no in-place retry onto a short drive");
    assert!(!final_path.exists());
    finish(queue, control, &[&stage, &out]);
}
