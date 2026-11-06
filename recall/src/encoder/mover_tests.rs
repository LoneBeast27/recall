//! The clip mover against temp dirs: copy, rename, cleanup, failure, retry,
//! FIFO order, quit, and the background I/O priority it runs at.

use super::*;
use std::sync::Mutex;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("recall-mover-{tag}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn cfg(same_volume: VolumeCheck) -> Config {
    Config {
        chunk: 4096,
        sync_every: 10_000,
        retry_after: Duration::from_millis(50),
        same_volume,
        background: false,
        stamp: stamp_mtime,
        identity: same_identity,
        stat: stat_dest,
    }
}

const ACROSS: VolumeCheck = |_, _| false;

/// A staged clip of `len` patterned bytes (chunk edges are visible if wrong).
fn staged(dir: &Path, name: &str, len: usize) -> (PathBuf, Vec<u8>) {
    let bytes: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
    let path = dir.join(name);
    std::fs::write(&path, &bytes).unwrap();
    (path, bytes)
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn lines(sink: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    sink.lock().unwrap().clone()
}

fn of_type(sink: &Arc<Mutex<Vec<String>>>, ty: &str) -> Vec<serde_json::Value> {
    let tag = format!("{{\"type\":\"{ty}\"");
    lines(sink).iter().filter(|l| l.starts_with(&tag)).map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn wait_for(sink: &Arc<Mutex<Vec<String>>>, ty: &str, n: usize) -> Vec<serde_json::Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let found = of_type(sink, ty);
        if found.len() >= n {
            return found;
        }
        assert!(Instant::now() < deadline, "no {ty}: {:?}", lines(sink));
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn copy_lands_the_exact_bytes_under_the_final_name_and_cleans_up() {
    let (stage, out) = (temp_dir("s"), temp_dir("o"));
    let (src, bytes) = staged(&stage, "G.mp4", 100_003);
    let dest = out.join("G.mp4");
    // A `.part` left by an interrupted move (bigger than the clip) is overwritten.
    std::fs::write(part_of(&dest), vec![9u8; 200_000]).unwrap();
    let job = MoveJob { staged: src.clone(), dest: dest.clone() };
    let moved = move_clip(&job, &cfg(ACROSS), &AtomicBool::new(false)).unwrap();
    assert_eq!((moved.how, moved.bytes, moved.dest.clone()), ("copy", 100_003, dest.clone()));
    assert!(moved.staged_kept.is_none());
    assert_eq!(std::fs::read(&dest).unwrap(), bytes);
    assert!(!src.exists(), "staged copy left the staging folder");
    assert_eq!(moved.kept, Some(stage.join(KEPT_DIR).join("G.mp4")), "kept for the app's video cache");
    assert!(!part_of(&dest).exists(), ".part renamed away");
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&out));
}

fn mtime(path: &Path) -> std::time::SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap()
}

#[test]
fn a_copied_clip_keeps_its_staged_copy_and_its_save_time() {
    let (stage, out) = (temp_dir("s"), temp_dir("o"));
    let (src, bytes) = staged(&stage, "G.mp4", 30_000);
    // The save happened an hour ago: the clip must not take the move's time.
    let saved_at = std::time::SystemTime::now() - Duration::from_secs(3600);
    std::fs::File::options().write(true).open(&src).unwrap().set_modified(saved_at).unwrap();
    let saved_at = mtime(&src);
    let dest = out.join("G.mp4");
    let moved = move_clip(&MoveJob { staged: src.clone(), dest: dest.clone() }, &cfg(ACROSS), &AtomicBool::new(false)).unwrap();
    let kept = moved.kept.clone().expect("staged copy kept");
    assert_eq!(kept, stage.join(KEPT_DIR).join("G.mp4"));
    assert_eq!(std::fs::read(&kept).unwrap(), bytes, "kept copy holds the clip's bytes");
    assert_eq!(mtime(&dest), saved_at, "destination carries the save time");
    assert_eq!(mtime(&kept), saved_at, "kept copy and destination share one identity");
    assert!(!src.exists());
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&out));
}

#[test]
fn kept_cap_is_half_the_app_cache_budget() {
    let cache = temp_dir("cache");
    let staging = cache.join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    assert_eq!(kept_cap(&staging), 1 << 30, "no storage.json: half the default 2 GiB");
    std::fs::write(cache.join("storage.json"), br#"{ "cache_budget_bytes": 4294967296 }"#).unwrap();
    assert_eq!(kept_cap(&staging), 2 << 30, "half of a 4 GiB budget");
    let _ = std::fs::remove_dir_all(&cache);
}

#[test]
fn kept_copies_stop_at_the_cap_while_the_app_is_away() {
    let (stage, out) = (temp_dir("s"), temp_dir("o"));
    let (a, _) = staged(&stage, "A.mp4", 6000);
    let (b, _) = staged(&stage, "B.mp4", 6000);
    assert!(keep_staged_within(&a, &out.join("A.mp4"), 10_000).is_some(), "the first fits");
    assert_eq!(keep_staged_within(&b, &out.join("B.mp4"), 10_000), None, "6000 + 6000 is past the cap");
    assert!(b.exists(), "the caller deletes it, as when keeping fails");
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&out));
}

#[test]
fn a_failed_stamp_still_lands_the_clip_but_keeps_no_copy() {
    let (stage, out) = (temp_dir("s"), temp_dir("o"));
    let (src, bytes) = staged(&stage, "G.mp4", 9000);
    let dest = out.join("G.mp4");
    let refuse: StampFn = |_, _| Err(std::io::Error::other("stamp refused"));
    let failing = Config { stamp: refuse, ..cfg(ACROSS) };
    let (events, sink) = Events::capture(true);
    let (queue, control) = spawn(events, failing, Vec::new()).unwrap();
    assert!(queue.submit(MoveJob { staged: src.clone(), dest: dest.clone() }));
    let moved = wait_for(&sink, "clip_moved", 1);
    assert_eq!(moved[0]["path"], text(&dest));
    assert!(moved[0].get("kept_path").is_none(), "an unstamped clip keeps no copy: {}", moved[0]);
    assert_eq!(std::fs::read(&dest).unwrap(), bytes, "the move itself completed");
    assert!(!src.exists() && !stage.join(KEPT_DIR).join("G.mp4").exists(), "staged copy deleted as before the keep");
    assert!(of_type(&sink, "clip_move_failed").is_empty(), "not a failed move");
    let logged: Vec<String> = lines(&sink).into_iter().filter(|l| l.contains("stamp refused")).collect();
    assert_eq!(logged.len(), 1, "one encoder_log line: {:?}", lines(&sink));
    assert!(logged[0].starts_with("{\"type\":\"encoder_log\""), "{}", logged[0]);
    assert!(control.shutdown(Duration::from_secs(1)));
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&out));
}

#[test]
fn a_staged_copy_that_cannot_be_kept_is_deleted_as_before() {
    let (stage, out) = (temp_dir("s"), temp_dir("o"));
    // A file named `kept` blocks the folder.
    std::fs::write(stage.join(KEPT_DIR), b"not a folder").unwrap();
    let (src, bytes) = staged(&stage, "G.mp4", 5000);
    let dest = out.join("G.mp4");
    let moved = move_clip(&MoveJob { staged: src.clone(), dest: dest.clone() }, &cfg(ACROSS), &AtomicBool::new(false)).unwrap();
    assert_eq!((moved.kept, moved.staged_kept), (None, None));
    assert_eq!(std::fs::read(&dest).unwrap(), bytes);
    assert!(!src.exists(), "staged copy deleted");
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&out));
}

#[test]
fn clip_moved_names_the_kept_copy_only_after_a_copy() {
    let (stage, out, near) = (temp_dir("s"), temp_dir("o"), temp_dir("n"));
    let (events, sink) = Events::capture(true);
    let (queue, control) = spawn(events, cfg(|_, dir| dir.to_string_lossy().contains("recall-mover-n-")), Vec::new()).unwrap();
    assert!(queue.submit(MoveJob { staged: staged(&stage, "A.mp4", 4000).0, dest: out.join("A.mp4") }));
    assert!(queue.submit(MoveJob { staged: staged(&stage, "B.mp4", 4000).0, dest: near.join("B.mp4") }));
    let moved = wait_for(&sink, "clip_moved", 2);
    assert_eq!(moved[0]["kept_path"], text(&stage.join(KEPT_DIR).join("A.mp4")), "{}", moved[0]);
    assert!(moved[1].get("kept_path").is_none(), "a same-volume rename keeps nothing: {}", moved[1]);
    assert!(control.shutdown(Duration::from_secs(1)));
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&out), std::fs::remove_dir_all(&near));
}

#[test]
fn quit_mid_move_removes_the_part_and_keeps_the_staged_copy() {
    let (stage, out) = (temp_dir("s"), temp_dir("o"));
    let (src, bytes) = staged(&stage, "G.mp4", 50_000);
    let dest = out.join("G.mp4");
    let job = MoveJob { staged: src.clone(), dest: dest.clone() };
    assert_eq!(move_clip(&job, &cfg(ACROSS), &AtomicBool::new(true)).unwrap_err(), MoveError::Aborted);
    assert!(!dest.exists(), "never a half file under the final name");
    assert!(!part_of(&dest).exists());
    assert_eq!(std::fs::read(&src).unwrap(), bytes, "staged copy kept for the next start");
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&out));
}

#[test]
fn existing_destinations_are_never_overwritten() {
    let (stage, out) = (temp_dir("s"), temp_dir("o"));
    // Same size: the rename landed before a crash kept the staged copy.
    let (src, bytes) = staged(&stage, "A.mp4", 1000);
    std::fs::write(out.join("A.mp4"), &bytes).unwrap();
    // The landed copy carries the staged file's time (the mover stamps it).
    std::fs::File::options().write(true).open(out.join("A.mp4")).unwrap().set_modified(mtime(&src)).unwrap();
    let job = MoveJob { staged: src.clone(), dest: out.join("A.mp4") };
    let moved = move_clip(&job, &cfg(ACROSS), &AtomicBool::new(false)).unwrap();
    assert_eq!(moved.how, "already in place");
    assert!(!src.exists());
    // Different size: a different file owns the name, so land beside it.
    let (src, bytes) = staged(&stage, "B.mp4", 1000);
    std::fs::write(out.join("B.mp4"), b"someone else's").unwrap();
    let job = MoveJob { staged: src.clone(), dest: out.join("B.mp4") };
    let moved = move_clip(&job, &cfg(ACROSS), &AtomicBool::new(false)).unwrap();
    assert_eq!(moved.dest, out.join("B-recovered.mp4"));
    assert_eq!(std::fs::read(out.join("B-recovered.mp4")).unwrap(), bytes);
    assert_eq!(std::fs::read(out.join("B.mp4")).unwrap(), b"someone else's");
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&out));
}

#[test]
fn same_volume_is_a_rename_and_a_missing_staged_file_is_dropped() {
    let (stage, out) = (temp_dir("s"), temp_dir("o"));
    let (src, bytes) = staged(&stage, "G.mp4", 3000);
    let job = MoveJob { staged: src.clone(), dest: out.join("G.mp4") };
    let moved = move_clip(&job, &cfg(|_, _| true), &AtomicBool::new(false)).unwrap();
    assert_eq!(moved.how, "rename");
    assert_eq!(std::fs::read(out.join("G.mp4")).unwrap(), bytes);
    assert!(!src.exists());
    assert_eq!(move_clip(&job, &cfg(ACROSS), &AtomicBool::new(false)).unwrap_err(), MoveError::Missing);
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&out));
}

#[test]
fn a_failed_move_keeps_the_staged_file_reports_once_and_retries() {
    let (stage, root) = (temp_dir("s"), temp_dir("o"));
    let out = root.join("not-yet"); // the clip folder is missing: create fails
    let (src, bytes) = staged(&stage, "G.mp4", 20_000);
    let (events, sink) = Events::capture(true);
    let (queue, control) = spawn(events, cfg(ACROSS), Vec::new()).unwrap();
    assert!(queue.submit(MoveJob { staged: src.clone(), dest: out.join("G.mp4") }));
    let failed = wait_for(&sink, "clip_move_failed", 1);
    assert_eq!(failed[0]["path"], text(&out.join("G.mp4")));
    assert_eq!(failed[0]["staged_path"], text(&src));
    assert!(failed[0]["reason"].as_str().unwrap().starts_with("create "), "{}", failed[0]);
    assert!(src.exists(), "staged copy kept");
    // Let at least one silent retry fail, then the folder comes back.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !lines(&sink).iter().any(|l| l.contains("clip move retry 2 failed")) {
        assert!(Instant::now() < deadline, "{:?}", lines(&sink));
        std::thread::sleep(Duration::from_millis(5));
    }
    std::fs::create_dir_all(&out).unwrap();
    let moved = wait_for(&sink, "clip_moved", 1);
    assert_eq!(moved[0]["path"], text(&out.join("G.mp4")));
    assert_eq!(std::fs::read(out.join("G.mp4")).unwrap(), bytes);
    assert!(!src.exists());
    assert_eq!(of_type(&sink, "clip_move_failed").len(), 1, "one failure event per clip, not per retry");
    assert!(control.shutdown(Duration::from_secs(1)));
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&root));
}

#[test]
fn moves_run_fifo_recovered_ones_first() {
    let (stage, out) = (temp_dir("s"), temp_dir("o"));
    let names = ["R.mp4", "A.mp4", "B.mp4", "C.mp4"];
    let jobs: Vec<MoveJob> = names
        .iter()
        .map(|n| MoveJob { staged: staged(&stage, n, 9000).0, dest: out.join(n) })
        .collect();
    let (events, sink) = Events::capture(true);
    let (queue, control) = spawn(events, cfg(ACROSS), vec![jobs[0].clone()]).unwrap();
    for job in &jobs[1..] {
        assert!(queue.submit(job.clone()));
    }
    let moved = wait_for(&sink, "clip_moved", names.len());
    let order: Vec<String> = moved.iter().map(|m| m["path"].as_str().unwrap().to_string()).collect();
    let expected: Vec<String> = names.iter().map(|n| text(&out.join(n))).collect();
    assert_eq!(order, expected);
    let left: Vec<_> = std::fs::read_dir(&stage).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(left, vec![std::ffi::OsString::from(KEPT_DIR)], "only the kept copies' folder stays");
    assert!(control.shutdown(Duration::from_secs(1)));
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&out));
}

#[test]
fn shutdown_stops_an_idle_mover_at_once_and_queued_moves_stay_staged() {
    let (stage, out) = (temp_dir("s"), temp_dir("o"));
    let (events, _sink) = Events::capture(true);
    let (queue, control) = spawn(events, cfg(ACROSS), Vec::new()).unwrap();
    let started = Instant::now();
    assert!(control.shutdown(Duration::from_secs(1)));
    assert!(started.elapsed() < Duration::from_millis(500));
    let (src, _) = staged(&stage, "G.mp4", 100);
    assert!(!queue.submit(MoveJob { staged: src.clone(), dest: out.join("G.mp4") }), "mover gone");
    assert!(src.exists() && !out.join("G.mp4").exists());
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&out));
}

#[test]
fn background_mode_drops_this_threads_io_priority_to_very_low() {
    let (before, after, cpu) = std::thread::spawn(|| {
        let before = super::super::bg_io::thread_io_priority();
        super::super::bg_io::enter_background().unwrap();
        (before, super::super::bg_io::thread_io_priority(), super::super::bg_io::thread_priority())
    })
    .join()
    .unwrap();
    assert_eq!(before, Some(2), "a fresh thread starts at normal I/O priority");
    assert_eq!(after, Some(0), "background mode = very low I/O priority");
    assert!(cpu <= -1, "at or below THREAD_PRIORITY_BELOW_NORMAL, got {cpu}");
}

/// `len` patterned bytes with byte `at` flipped.
fn flipped(bytes: &[u8], at: usize) -> Vec<u8> {
    let mut other = bytes.to_vec();
    other[at] ^= 0xff;
    other
}

#[test]
fn a_same_size_destination_with_other_bytes_or_time_is_not_taken_for_the_clip() {
    let (stage, out) = (sandbox("s"), sandbox("o"));
    let len = 3 << 20;
    // Differs only in the last MiB, only in the first MiB, or only in its time.
    for (name, at) in [("Tail.mp4", Some(len - 10)), ("Head.mp4", Some(10)), ("Time.mp4", None)] {
        let (src, bytes) = staged(&stage, name, len);
        let dest = out.join(name);
        std::fs::write(&dest, at.map_or(bytes.clone(), |at| flipped(&bytes, at))).unwrap();
        let when = if at.is_some() { mtime(&src) } else { mtime(&src) - Duration::from_secs(60) };
        std::fs::File::options().write(true).open(&dest).unwrap().set_modified(when).unwrap();
        let before = std::fs::read(&dest).unwrap();
        let moved = move_clip(&MoveJob { staged: src.clone(), dest: dest.clone() }, &cfg(ACROSS), &AtomicBool::new(false)).unwrap();
        assert_ne!(moved.how, "already in place", "{name}");
        assert_eq!(std::fs::read(&moved.dest).unwrap(), bytes, "{name}: the new clip landed beside");
        assert_eq!(std::fs::read(&dest).unwrap(), before, "{name}: the other file is untouched");
    }
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&out));
}

#[test]
fn a_staged_file_that_is_its_own_destination_is_never_deleted() {
    // Staging pointed at the clip folder: recovery queues each clip to itself.
    let out = sandbox("self");
    let (clip, bytes) = staged(&out, "A.mp4", 5000);
    let moved = move_clip(&MoveJob { staged: clip.clone(), dest: clip.clone() }, &cfg(ACROSS), &AtomicBool::new(false)).unwrap();
    assert_eq!(moved.how, "already in place");
    assert_eq!(std::fs::read(&clip).unwrap(), bytes, "the clip is still there");
    assert!(moved.kept.is_none());
    let _ = std::fs::remove_dir_all(&out);
}

/// `<worktree>\.tmp\sandbox\<tag>-<uuid>`: tests that stand in for clip folders stay out of %TEMP%.
fn sandbox(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".tmp").join("sandbox").join(format!("{tag}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_kept_copy_carries_the_engines_mark() {
    // The app's start-up sweep deletes only marked copies, so a file someone else put in
    // `kept\` is never the engine's to throw away.
    let (stage, out) = (sandbox("mark-s"), sandbox("mark-o"));
    let (src, _) = staged(&stage, "G.mp4", 30_000);
    let moved = move_clip(&MoveJob { staged: src, dest: out.join("G.mp4") }, &cfg(ACROSS), &AtomicBool::new(false)).unwrap();
    let kept = moved.kept.expect("staged copy kept");
    assert!(std::fs::metadata(format!("{}:{KEPT_MARK}", kept.display())).is_ok(), "the kept copy is marked");
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&out));
}

#[test]
fn a_hard_link_to_the_staged_file_is_the_same_file() {
    // Two names, one file: deleting the staged name would leave only the link.
    let (stage, out) = (sandbox("link-s"), sandbox("link-o"));
    let (src, bytes) = staged(&stage, "A.mp4", 5000);
    let dest = out.join("A.mp4");
    std::fs::hard_link(&src, &dest).unwrap();
    let moved = move_clip(&MoveJob { staged: src.clone(), dest: dest.clone() }, &cfg(ACROSS), &AtomicBool::new(false)).unwrap();
    assert_eq!(moved.how, "already in place");
    assert!(src.exists(), "one file under two names: nothing is deleted");
    assert_eq!(std::fs::read(&dest).unwrap(), bytes);
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&out));
}

#[test]
fn an_identity_windows_cannot_read_never_deletes_the_staged_copy() {
    // A RAM disk or a user-mode file system that answers no file index: unknown is not "different".
    let (stage, out) = (sandbox("noid-s"), sandbox("noid-o"));
    let (src, bytes) = staged(&stage, "A.mp4", 5000);
    let dest = out.join("A.mp4");
    std::fs::write(&dest, &bytes).unwrap();
    std::fs::File::options().write(true).open(&dest).unwrap().set_modified(mtime(&src)).unwrap();
    let unknown = Config { identity: |_, _| None, ..cfg(ACROSS) };
    assert!(move_clip(&MoveJob { staged: src.clone(), dest: dest.clone() }, &unknown, &AtomicBool::new(false)).is_err(), "parked, retried later");
    assert_eq!(std::fs::read(&src).unwrap(), bytes, "the staged copy stays");
    assert_eq!(std::fs::read(&dest).unwrap(), bytes, "the destination stays");
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&out));
}

#[test]
fn a_destination_windows_cannot_stat_is_never_replaced() {
    // Access denied (or any error but "not found"): the destination may exist.
    let (stage, out) = (sandbox("stat-s"), sandbox("stat-o"));
    let (src, bytes) = staged(&stage, "A.mp4", 5000);
    let dest = out.join("A.mp4");
    let denied = Config { stat: |_| Err(std::io::Error::from_raw_os_error(5)), ..cfg(ACROSS) };
    assert!(move_clip(&MoveJob { staged: src.clone(), dest: dest.clone() }, &denied, &AtomicBool::new(false)).is_err());
    assert!(!dest.exists(), "nothing was written over a destination it could not see");
    assert_eq!(std::fs::read(&src).unwrap(), bytes, "the staged copy stays");
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&out));
}
