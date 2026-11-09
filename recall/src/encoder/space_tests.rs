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

/// `%TEMP%\recall-space-<tag>-<uuid>`, removed when the guard drops, also on a panic.
fn temp_dir(tag: &str) -> crate::encoder::test_scratch::Scratch {
    crate::encoder::test_scratch::scratch("recall-space", tag)
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
            identity: mover::same_identity,
            stat: mover::stat_dest,
            flush_dest: mover::flush_dest,
        },
        refused: Vec::new(),
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
fn estimate_is_video_plus_audio_plus_real_mux_overhead() {
    let j = job(Path::new(r"E:\Clips\a.mp4"), 2.0);
    // 8 Mbit/s still for 2 s = 2 MB; 120 frames at 60 fps x 32 index bytes; 64 KiB headers;
    // no audio tracks, no thumbnail reserve.
    assert_eq!(estimate(&j), 2_000_000 + 120 * 32 + (64 << 10));
}

fn still_job(secs: f64, bitrate: usize) -> SaveJob {
    let mut j = job(Path::new(r"E:\Clips\a.mp4"), secs);
    if let VideoSrc::Still { spec, .. } = &mut j.video {
        spec.bitrate = bitrate;
    }
    j
}

#[test]
fn short_and_low_bitrate_saves_get_a_small_allowance() {
    for bitrate in [8_000_000usize, 25_000_000] {
        let payload = (bitrate / 8 * 5) as u64;
        let overhead = estimate(&still_job(5.0, bitrate)) - payload;
        assert!(overhead * 100 < payload * 5, "{bitrate} bit/s, 5 s: {overhead} bytes over {payload}");
    }
    // A long save reserves its index and headers only.
    let payload = 25_000_000 / 8 * 120;
    let overhead = estimate(&still_job(120.0, 25_000_000)) - payload;
    assert_eq!(overhead, 7200 * 32 + (64 << 10));
}

#[test]
fn a_save_that_just_fits_is_not_refused() {
    // 1.5 MB of video with 4 MiB free on both drives: the old 64 MiB headroom refused this.
    let (stage, out) = (temp_dir("stage"), temp_dir("out"));
    let calls = Arc::new(AtomicUsize::new(0));
    let four_mib: FreeSpace = |_| Some(4 * MIB);
    let (queue, control, sink) = spawn_with(&stage, &out, mux_full_in("never", calls.clone()), four_mib);
    let final_path = out.join(CLIP);
    assert!(queue.submit(job(&final_path, 1.5)));
    let saved = outcome(&sink);
    assert_eq!(saved["type"], "saved", "{saved}");
    assert_eq!(calls.load(SeqCst), 1);
    finish(queue, control, &[&stage, &out]);
}

#[test]
fn sizes_are_decimal_like_the_app() {
    // The app's format_bytes is decimal (1 GB = 10^9 bytes); the label must say what the math does.
    assert_eq!(size(1_500_000_000), "1.5 GB");
    assert_eq!(size(1_100_000_000), "1.1 GB");
    assert_eq!(size(300_000_000), "300 MB");
    assert_eq!(size(1), "1 MB", "rounded up: never claims less than is needed");
}

/// A directory junction at `link` -> `target` (no admin needed; no window).
fn junction(link: &Path, target: &Path) {
    use std::os::windows::process::CommandExt;
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "mklink /J failed");
}

/// A folder in this checkout (L: here) reached through a junction under %TEMP% (C: here).
fn junction_pair() -> (PathBuf, PathBuf) {
    let id = uuid::Uuid::new_v4().simple();
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".tmp").join(format!("junction-target-{id}"));
    std::fs::create_dir_all(&target).unwrap();
    let link = std::env::temp_dir().join(format!("recall-space-junction-{id}"));
    junction(&link, &target);
    (link, std::fs::canonicalize(&target).unwrap())
}

fn drop_pair(link: &Path, target: &Path) {
    let _ = std::fs::remove_dir(link); // removes the junction only
    let _ = std::fs::remove_dir_all(target);
}

#[test]
fn a_junctioned_clip_folder_is_named_by_the_drive_it_really_lands_on() {
    let (link, target) = junction_pair();
    let (via_link, real) = (drive(&link.join(CLIP)), drive(&target.join(CLIP)));
    // Staging decisions follow the real volume too.
    let same_as_target = staging::same_volume(&link, &target);
    let temp = std::env::temp_dir();
    let temp_is_other = drive(&temp.join("x")) != real;
    let same_as_temp = staging::same_volume(&link, &temp);
    drop_pair(&link, &target);
    assert_eq!(via_link, real, "the junction's text drive is not where the clip lands");
    assert!(same_as_target, "a junction to the clip folder's volume is that volume");
    if temp_is_other {
        assert!(!same_as_temp, "temp and the checkout are different volumes here");
    }
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
    let need = 300_000_000;
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
    assert_eq!(reason, format!("Not enough space on {drive} (need {})", size(estimate(&job(&final_path, 1.5)))));
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

#[test]
fn a_panicking_test_still_removes_its_temp_folder() {
    // F-storage-2: a failing test left its %TEMP% folder behind.
    let left = std::panic::catch_unwind(|| {
        let dir = temp_dir("panics");
        std::fs::write(dir.join("clip.mp4"), vec![1u8; 4096]).unwrap();
        std::panic::panic_any(dir.to_path_buf());
    })
    .unwrap_err()
    .downcast::<PathBuf>()
    .unwrap();
    let gone = !left.exists();
    let _ = std::fs::remove_dir_all(&*left);
    assert!(gone, "{} was left behind", left.display());
}
