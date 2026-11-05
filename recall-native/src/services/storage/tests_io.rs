//! Filesystem / thread / process tests in temp folders (never the library drive).
use std::collections::VecDeque;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use super::io_priority::{self, IO_VERY_LOW};
use super::lane::{IdleSource, Lane, Priority, Task};
use super::*;

fn temp_root(tag: &str) -> PathBuf {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("recall-storage-{tag}-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

struct ListSource(Mutex<VecDeque<Task>>);
impl IdleSource for ListSource {
    fn next_task(&self) -> Option<Task> {
        self.0.lock().unwrap().pop_front()
    }
}

fn wait_for(log: &Arc<Mutex<Vec<String>>>, count: usize) {
    let start = Instant::now();
    while log.lock().unwrap().len() < count {
        assert!(start.elapsed() < Duration::from_secs(10), "lane stalled: {:?}", log.lock().unwrap());
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn lane_serves_on_demand_before_prefetch_at_very_low_io_priority() {
    let lane = Lane::spawn("test-lane-order", false, Duration::from_millis(60));
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let (started_tx, started_rx) = mpsc::channel();
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let io = Arc::new(Mutex::new(None));
    {
        let (log, io) = (log.clone(), io.clone());
        lane.submit(Priority::Visible, Box::new(move || {
            *io.lock().unwrap() = io_priority::current_thread_io_priority();
            started_tx.send(()).unwrap();
            gate_rx.recv().unwrap();
            log.lock().unwrap().push("first".into());
        }));
    }
    started_rx.recv().unwrap();
    let entry = |name: &'static str| -> Task {
        let log = log.clone();
        Box::new(move || log.lock().unwrap().push(name.into()))
    };
    lane.set_idle(Some(Arc::new(ListSource(Mutex::new(VecDeque::from([entry("p1"), entry("p2")]))))));
    lane.submit(Priority::Visible, entry("v1"));
    lane.submit(Priority::Background, entry("b1"));
    lane.submit(Priority::Interactive, entry("i1"));
    lane.submit(Priority::Visible, entry("v2"));
    gate_tx.send(()).unwrap();
    wait_for(&log, 7);
    assert_eq!(*log.lock().unwrap(), ["first", "i1", "v2", "v1", "b1", "p1", "p2"]);
    assert_eq!(*io.lock().unwrap(), Some(IO_VERY_LOW), "lane thread must run in background I/O mode");
}

#[test]
fn prefetch_waits_for_a_quiet_lane() {
    let grace = Duration::from_millis(250);
    let lane = Lane::spawn("test-lane-grace", false, grace);
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let started = Instant::now();
    let ran_at = Arc::new(Mutex::new(None));
    {
        let log = log.clone();
        lane.submit(Priority::Visible, Box::new(move || log.lock().unwrap().push("demand".into())));
    }
    let (log2, ran) = (log.clone(), ran_at.clone());
    let task: Task = Box::new(move || {
        *ran.lock().unwrap() = Some(started.elapsed());
        log2.lock().unwrap().push("prefetch".into());
    });
    lane.set_idle(Some(Arc::new(ListSource(Mutex::new(VecDeque::from([task]))))));
    wait_for(&log, 2);
    let waited = ran_at.lock().unwrap().unwrap();
    assert!(waited >= grace, "prefetch ran {waited:?} after on-demand work (grace {grace:?})");
}

#[test]
fn blocking_run_returns_values_and_survives_a_panicking_task() {
    let lane = Lane::spawn("test-lane-run", false, Duration::from_millis(10));
    assert_eq!(lane.run(Priority::Interactive, || 41 + 1), Ok(42));
    assert!(lane.run(Priority::Interactive, || -> u8 { panic!("boom") }).is_err());
    assert_eq!(lane.run(Priority::Visible, || "still alive"), Ok("still alive"));
}

#[test]
fn lowered_child_process_starts_at_very_low_io_priority() {
    let ping = |lowered: bool| {
        let mut command = Command::new("ping");
        command.args(["-n", "2", "127.0.0.1"]).stdout(Stdio::null()).stderr(Stdio::null()).stdin(Stdio::null());
        let mut child = io_priority::spawn(&mut command, lowered).expect("spawn ping");
        let priority = io_priority::process_io_priority(&child);
        let _ = child.kill();
        let _ = child.wait();
        priority
    };
    assert_eq!(ping(true), Some(IO_VERY_LOW));
    assert_eq!(ping(false), Some(2), "unlowered children stay at normal I/O priority");
}

fn write_pattern(path: &Path, len: usize, seed: u8) {
    let data: Vec<u8> = (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect();
    std::fs::write(path, data).unwrap();
}

fn stamp(path: &Path, secs: u64) {
    File::options().write(true).open(path).unwrap().set_modified(UNIX_EPOCH + Duration::from_secs(secs)).unwrap();
}

#[test]
fn video_copy_is_exact_and_evicted_least_recently_used() {
    let root = temp_root("video");
    let source = root.join("library");
    std::fs::create_dir_all(&source).unwrap();
    let (a, b, c) = (source.join("a.mp4"), source.join("b.mp4"), source.join("c.mp4"));
    write_pattern(&a, (9 << 20) + 123, 1); // two 8 MiB chunks, unaligned tail
    write_pattern(&b, 3 << 20, 2);
    write_pattern(&c, (3 << 20) + 7, 3);
    for (path, secs) in [(&a, 1_700_000_000), (&b, 1_700_000_100), (&c, 1_700_000_200)] {
        stamp(path, secs);
    }
    // The unbuffered path itself (cache_in would silently fall back to buffered).
    let direct = root.join("direct.mp4");
    let modified = a.metadata().unwrap().modified().unwrap();
    video::copy_unbuffered(&a, &direct, a.metadata().unwrap().len(), modified, &video::FULL_SPEED).expect("unbuffered copy on NTFS");
    assert_eq!(std::fs::read(&direct).unwrap(), std::fs::read(&a).unwrap(), "unbuffered copy trims the padded tail");
    assert_eq!(direct.metadata().unwrap().modified().unwrap(), modified);
    let cache = root.join("cache");
    let copy_a = video::cache_in(&cache, &a, 20 << 20).unwrap();
    assert_eq!(std::fs::read(&copy_a).unwrap(), std::fs::read(&a).unwrap(), "byte-exact copy");
    assert_eq!(copy_a.metadata().unwrap().modified().unwrap(), a.metadata().unwrap().modified().unwrap(), "mtime carried");
    assert_eq!(cached_video_in(&cache, &a), Some(copy_a.to_string_lossy().into_owned()));
    assert_eq!(cached_video_in(&cache, &b), None);
    std::thread::sleep(Duration::from_millis(5));
    let copy_b = video::cache_in(&cache, &b, 14 << 20).unwrap();
    std::thread::sleep(Duration::from_millis(5));
    let copy_c = video::cache_in(&cache, &c, 14 << 20).unwrap();
    assert!(!copy_a.exists(), "a was least recently used and over budget");
    assert!(copy_b.exists() && copy_c.exists());
    assert_eq!(std::fs::read(&copy_c).unwrap(), std::fs::read(&c).unwrap());
    assert!(video::cache_in(&cache, &a, 1 << 20).is_err(), "larger than half the budget");
    let freed = budget::enforce(&cache, 0, 0);
    assert_eq!(freed, (3 << 20) + (3 << 20) + 7);
    assert!(!copy_b.exists() && !copy_c.exists());
    let leftovers: Vec<_> = std::fs::read_dir(cache.join("video")).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(leftovers, ["lru.json"], "no temp files left behind");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn working_copy_written_later_wins_over_the_sidecar() {
    let root = temp_root("projects");
    let working = working_copy_path(&root, "clip.mp4", 10, 20);
    assert!(working.starts_with(root.join("projects")));
    let sidecar = root.join("clip.mp4.recall-edit.json");
    write_atomic(&sidecar, b"{\"sidecar\":1}").unwrap();
    std::thread::sleep(Duration::from_millis(20));
    write_atomic(&working, b"{\"working\":1}").unwrap();
    let order = project_order(projects_modified_ms(&working), projects_modified_ms(&sidecar));
    assert_eq!(order, [ProjectSource::WorkingCopy, ProjectSource::Sidecar]);
    std::thread::sleep(Duration::from_millis(20));
    write_atomic(&sidecar, b"{\"flushed\":1}").unwrap();
    let order = project_order(projects_modified_ms(&working), projects_modified_ms(&sidecar));
    assert_eq!(order[0], ProjectSource::Sidecar);
    let names: Vec<_> = std::fs::read_dir(&root).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert!(names.iter().all(|n| !n.ends_with(".part")), "atomic writes leave no temp files: {names:?}");
    let _ = std::fs::remove_dir_all(root);
}

fn ffmpeg_or_skip() -> Option<PathBuf> {
    match binary("RECALL_FFMPEG", "ffmpeg.exe") {
        Ok(path) => Some(path),
        Err(_) => {
            eprintln!("ffmpeg not found; skipping");
            None
        }
    }
}

fn ffmpeg(args: &[&str]) {
    let status = Command::new(ffmpeg_or_skip().unwrap()).args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).status().unwrap();
    assert!(status.success(), "ffmpeg {args:?}");
}

#[test]
fn thumbnails_import_newest_recorder_thumb_resize_large_and_generate_missing() {
    if ffmpeg_or_skip().is_none() {
        return;
    }
    let root = temp_root("thumbs");
    let library = root.join("library");
    std::fs::create_dir_all(library.join("thumbs")).unwrap();
    let cache = root.join("cache");
    let text = |p: &Path| p.to_string_lossy().into_owned();

    // 1. Display needs 640 px, recorder thumb is 640 px: newest epoch is copied byte-for-byte.
    thumbs::set_wanted_width(640);
    let clip = library.join("game__2026-10-17__22-34-31-275__abc.mp4");
    std::fs::write(&clip, vec![7u8; 4096]).unwrap();
    let old = library.join("thumbs").join("game__2026-10-17__22-34-31-275__abc-1792250000-thumb.jpg");
    let new = library.join("thumbs").join("game__2026-10-17__22-34-31-275__abc-1792256671-thumb.jpg");
    ffmpeg(&["-f", "lavfi", "-i", "color=c=red:s=640x360", "-frames:v", "1", &text(&old)]);
    ffmpeg(&["-f", "lavfi", "-i", "color=c=blue:s=640x360", "-frames:v", "1", &text(&new)]);
    let imported = thumbs::ensure_in(&cache, &clip).unwrap();
    assert_eq!(std::fs::read(&imported).unwrap(), std::fs::read(&new).unwrap());
    let (id, bytes, secs) = identity_of(&clip).unwrap();
    assert!(imported.ends_with(&format!("{id}.jpg")));
    let index = thumbs::index_in(&cache);
    assert_eq!(index.lookup("game__2026-10-17__22-34-31-275__abc.mp4", bytes, secs), Some(PathBuf::from(&imported)));
    assert_eq!(index.lookup("game__2026-10-17__22-34-31-275__abc.mp4", bytes + 1, secs), None, "other revision");

    // 2. Display needs 448 px (a 448-wide tile): a 1920 px recorder thumb is scaled to 448.
    thumbs::set_wanted_width(448);
    let wide = library.join("wide__2026-10-18__10-00-00-000__def.mp4");
    std::fs::write(&wide, vec![8u8; 4096]).unwrap();
    ffmpeg(&["-f", "lavfi", "-i", "color=c=green:s=1920x1080", "-frames:v", "1", &text(&library.join("thumbs").join("wide__2026-10-18__10-00-00-000__def-1792300000-thumb.jpg"))]);
    let scaled = thumbs::ensure_in(&cache, &wide).unwrap();
    assert_eq!(thumbs::jpeg_size(&std::fs::read(&scaled).unwrap()), Some((448, 252)));

    // 3. No recorder thumb: a frame is extracted from the clip at the wanted width.
    let bare = library.join("bare.mp4");
    ffmpeg(&["-f", "lavfi", "-i", "testsrc=size=640x360:rate=30", "-t", "2", "-pix_fmt", "yuv420p", "-c:v", "mpeg4", &text(&bare)]);
    let generated = thumbs::ensure_in(&cache, &bare).unwrap();
    assert_eq!(thumbs::jpeg_size(&std::fs::read(&generated).unwrap()).map(|s| s.0), Some(448));
    assert_eq!(thumbs::ensure_in(&cache, &bare).unwrap(), generated, "second call is a cache hit");
    assert_eq!(thumbs::index_in(&cache).len(), 3);

    // 4. A denser display needs 704 px: the 448 px thumbnail is replaced in place by a larger one,
    //    taken from the clip since the 640 px recorder thumb is too small.
    thumbs::set_wanted_width(700);
    assert_eq!(thumbs::wanted_width(), 704, "rounded up to the 64 px step");
    let sharper = thumbs::ensure_in(&cache, &bare).unwrap();
    assert_eq!(sharper, generated, "same file");
    assert_eq!(thumbs::jpeg_size(&std::fs::read(&sharper).unwrap()).map(|s| s.0), Some(704));
    thumbs::set_wanted_width(thumbs::DEFAULT_WIDTH);

    // Settings usage / clear for the tier.
    let usage = tiers::usage_in(&cache);
    let thumbs_tier = usage.tiers.iter().find(|t| t.kind == CacheKind::Thumbs).unwrap();
    assert_eq!(thumbs_tier.items, 3);
    assert!(tiers::clear_in(&cache, CacheKind::Thumbs).unwrap() >= thumbs_tier.bytes);
    assert_eq!(thumbs::index_in(&cache).len(), 0);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_open_waits_under_half_a_second_for_a_warm_up_copy_on_a_slow_drive() {
    let root = temp_root("warm-yield");
    let clip = root.join("clip.mp4");
    write_pattern(&clip, 96 << 20, 4);
    let cache = root.join("cache");
    // A slowed drive (64 MB/s, the RECALL_SLOW_DRIVE throttle): the copy alone takes ~1.6 s.
    let lane = Lane::spawn("test-lane-warm-yield", false, Duration::from_millis(40));
    let (tx, rx) = mpsc::channel();
    let (warm_clip, warm_cache) = (clip.clone(), cache.clone());
    lane.submit(Priority::Background, Box::new(move || {
        let pace = video::Pace { rate: 64_000_000, yields: &lane::demand_waiting };
        let _ = tx.send(video::copy_in(&warm_cache, &warm_clip, 1 << 40, &|_| true, &pace));
    }));
    std::thread::sleep(Duration::from_millis(300));
    let asked = Instant::now();
    let waited = lane.run(Priority::Interactive, move || asked.elapsed()).unwrap();
    assert!(waited < Duration::from_millis(500), "the open waited {waited:?} behind the warm-up copy");
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap_err(), video::YIELDED);
    let left: Vec<_> = std::fs::read_dir(cache.join("video")).unwrap().flatten().map(|e| e.file_name()).collect();
    assert!(left.is_empty(), "the half copy is deleted: {left:?}");
    // With nobody waiting, the same copy completes.
    let pace = video::Pace { rate: 0, yields: &lane::demand_waiting };
    let copy = video::copy_in(&cache, &clip, 1 << 40, &|_| true, &pace).unwrap();
    assert_eq!(copy.metadata().unwrap().len(), 96 << 20);
    let _ = std::fs::remove_dir_all(&root);
}

/// Thumbnails are never evicted, so the ones no listed clip owns go once they are old enough.
#[test]
fn orphan_thumbnails_go_once_old_and_owned_ones_stay() {
    let root = temp_root("orphan-thumbs");
    let thumbs = root.join("thumbs");
    std::fs::create_dir_all(&thumbs).unwrap();
    let month_ago = SystemTime::now() - std::time::Duration::from_secs(40 * 24 * 3600);
    for (name, old) in [("owned.jpg", true), ("orphan-old.jpg", true), ("orphan-new.jpg", false)] {
        std::fs::write(thumbs.join(name), [0u8; 100]).unwrap();
        if old {
            std::fs::File::options().write(true).open(thumbs.join(name)).unwrap().set_modified(month_ago).unwrap();
        }
    }
    let owned: std::collections::HashSet<String> = ["owned".to_string()].into();
    assert_eq!(budget::sweep_orphan_thumbs(&root, &owned, budget::ORPHAN_THUMB_AGE), 100);
    assert!(thumbs.join("owned.jpg").exists(), "a listed clip keeps its thumbnail");
    assert!(!thumbs.join("orphan-old.jpg").exists(), "an old orphan goes");
    assert!(thumbs.join("orphan-new.jpg").exists(), "a recent orphan stays (its drive may be offline)");
    assert_eq!(budget::sweep_orphan_thumbs(&root, &Default::default(), std::time::Duration::ZERO), 0, "an empty listing deletes nothing");
    assert!(thumbs.join("orphan-new.jpg").exists());
    let _ = std::fs::remove_dir_all(root);
}

/// Only a listing that saw every folder whole may prune thumbnails: a missing recordings folder
/// or a missing (offline) imported folder makes it partial.
#[test]
fn a_listing_is_complete_only_when_every_folder_was_read() {
    let root = temp_root("listing-complete");
    let (recordings, imported) = (root.join("recordings"), root.join("imported"));
    std::fs::create_dir_all(&recordings).unwrap();
    std::fs::create_dir_all(&imported).unwrap();
    std::fs::write(recordings.join("a.mp4"), b"x").unwrap();
    std::fs::write(imported.join("b.mp4"), b"x").unwrap();
    let text = |p: &std::path::Path| p.to_string_lossy().into_owned();
    crate::services::library::folders::forget_scans();
    let (clips, complete) = crate::backend::list_from(&recordings, &[text(&imported)]).unwrap();
    assert_eq!((clips.len(), complete), (2, true), "both folders read whole");
    let (_, complete) = crate::backend::list_from(&root.join("offline-recordings"), &[text(&imported)]).unwrap();
    assert!(!complete, "a missing recordings folder makes the listing partial");
    let (clips, complete) = crate::backend::list_from(&recordings, &[text(&root.join("offline-import"))]).unwrap();
    assert_eq!((clips.len(), complete), (1, false), "a missing imported folder makes the listing partial");
    crate::services::library::folders::forget_scans();
    let _ = std::fs::remove_dir_all(root);
}
