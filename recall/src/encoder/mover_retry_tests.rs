//! Retries of a move whose clip landed but could not be flushed (round-2 review R2-1, R2-3,
//! R2-6): the user is told once when it keeps failing, a retry never lands a second copy, and
//! a clip the user deleted meanwhile is not copied back.

use super::*;
use std::sync::atomic::AtomicUsize;
use std::sync::Mutex;

/// `%TEMP%\recall-retry-<tag>-<uuid>`, removed when the guard drops, also on a panic.
fn temp_dir(tag: &str) -> crate::encoder::test_scratch::Scratch {
    crate::encoder::test_scratch::scratch("recall-retry", tag)
}

fn cfg(flush: FlushFn, retry_after: Duration) -> Config {
    Config {
        chunk: 4096,
        sync_every: 10_000,
        retry_after,
        same_volume: |_, _| false,
        background: false,
        stamp: stamp_mtime,
        identity: same_identity,
        stat: stat_dest,
        flush_dest: flush,
    }
}

fn staged(dir: &Path, name: &str) -> (PathBuf, Vec<u8>) {
    let bytes: Vec<u8> = (0..9000).map(|i| (i % 251) as u8).collect();
    let path = dir.join(name);
    std::fs::write(&path, &bytes).unwrap();
    (path, bytes)
}

fn of_type(sink: &Arc<Mutex<Vec<String>>>, ty: &str) -> Vec<serde_json::Value> {
    let tag = format!("{{\"type\":\"{ty}\"");
    sink.lock().unwrap().iter().filter(|l| l.starts_with(&tag)).map(|l| serde_json::from_str(l).unwrap()).collect()
}

/// Waits up to 5 s for `done`.
fn until(sink: &Arc<Mutex<Vec<String>>>, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "timed out: {:?}", sink.lock().unwrap());
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

#[test]
fn an_always_unflushed_move_warns_once_after_three_tries() {
    let (stage, out) = (temp_dir("s"), temp_dir("o"));
    let (src, bytes) = staged(&stage, "G.mp4");
    let never: FlushFn = |_| Err(std::io::Error::other("flush refused"));
    let (events, sink) = Events::capture(true);
    let (queue, control) = spawn(events, cfg(never, Duration::from_millis(5)), Vec::new()).unwrap();
    assert!(queue.submit(MoveJob { staged: src.clone(), dest: out.join("G.mp4") }));
    let tries = || sink.lock().unwrap().iter().filter(|l| l.contains("not flushed yet")).count();
    until(&sink, || tries() >= 6);
    let failed = of_type(&sink, "clip_move_failed");
    assert_eq!(failed.len(), 1, "one warning, after the third try: {:?}", sink.lock().unwrap());
    assert!(failed[0]["reason"].as_str().is_some_and(|r| r.contains("couldn't confirm")), "{}", failed[0]);
    assert_eq!(std::fs::read(&src).unwrap(), bytes, "the staged copy is never deleted without a flush");
    assert!(control.shutdown(Duration::from_secs(1)));
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&out));
}

static UNSTAMPED_FLUSHES: AtomicUsize = AtomicUsize::new(0);

#[test]
fn an_unstamped_unflushed_move_lands_one_copy_only() {
    let (stage, out) = (temp_dir("s"), temp_dir("o"));
    let (src, bytes) = staged(&stage, "G.mp4");
    let first_fails: FlushFn = |_| if UNSTAMPED_FLUSHES.fetch_add(1, Relaxed) == 0 { Err(std::io::Error::other("not yet")) } else { Ok(()) };
    let refuse: StampFn = |_, _| Err(std::io::Error::other("stamp refused"));
    let (events, sink) = Events::capture(true);
    let (queue, control) = spawn(events, Config { stamp: refuse, ..cfg(first_fails, Duration::from_millis(5)) }, Vec::new()).unwrap();
    assert!(queue.submit(MoveJob { staged: src.clone(), dest: out.join("G.mp4") }));
    until(&sink, || !src.exists());
    assert_eq!(names(&out), vec!["G.mp4".to_string()], "no -recovered duplicate: {:?}", sink.lock().unwrap());
    assert_eq!(std::fs::read(out.join("G.mp4")).unwrap(), bytes);
    assert!(control.shutdown(Duration::from_secs(1)));
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&out));
}

static DELETED_FLUSHES: AtomicUsize = AtomicUsize::new(0);

#[test]
fn a_clip_deleted_before_the_flush_retry_is_not_copied_back() {
    let (stage, out) = (temp_dir("s"), temp_dir("o"));
    let (src, bytes) = staged(&stage, "G.mp4");
    let dest = out.join("G.mp4");
    let first_fails: FlushFn = |_| if DELETED_FLUSHES.fetch_add(1, Relaxed) == 0 { Err(std::io::Error::other("not yet")) } else { Ok(()) };
    let (events, sink) = Events::capture(true);
    let (queue, control) = spawn(events, cfg(first_fails, Duration::from_millis(300)), Vec::new()).unwrap();
    assert!(queue.submit(MoveJob { staged: src.clone(), dest: dest.clone() }));
    until(&sink, || !of_type(&sink, "clip_moved").is_empty());
    // The user deletes the fresh clip in the library before the retry.
    std::fs::remove_file(&dest).unwrap();
    until(&sink, || !src.exists());
    std::thread::sleep(Duration::from_millis(400));
    assert!(!dest.exists(), "the deleted clip came back: {:?}", sink.lock().unwrap());
    let set_aside = stage.join(KEPT_DIR).join("G.mp4");
    assert_eq!(std::fs::read(&set_aside).unwrap(), bytes, "the SSD copy is set aside, never deleted");
    assert!(std::fs::metadata(format!("{}:{KEPT_MARK}", set_aside.display())).is_err(), "unmarked: the app never deletes it");
    assert!(control.shutdown(Duration::from_secs(1)));
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&out));
}
