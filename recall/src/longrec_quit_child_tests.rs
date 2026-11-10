//! A dead controller: the engine's stdin hits EOF and its stderr pipe is broken.
//! The test re-runs its own test exe as the "engine" (no capture or audio device):
//! the child runs the live stdin reader's body (`engine_stdin::spawn_early`: `read_early`,
//! then `stdin_reader::finish`) with a real quit flag, logs one line through the release
//! build's `eprintln!` (`quiet_io::err`) to the broken stderr, and has a long recording
//! running on the writer thread with the fake sink. On quit it takes the engine's quit-time
//! `LongRec::shutdown`, checks no `.part` is left, writes `quit-ok` to a marker file (the
//! end of stdin points stdout at NUL, as in the engine) and exits 0.

use super::*;

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::sync_channel;

use test_sink::{pkt, temp_dir, tracks, FakeFactory, MS};

const CHILD_ENV: &str = "RECALL_TEST_CHILD";
const CHILD_TEST: &str = "longrec::quit_child_tests::closed_stderr_child";
/// Where the child writes `quit-ok` (its stdout is NUL once stdin has ended).
const CHILD_OK_ENV: &str = "RECALL_TEST_CHILD_OK";

/// A temp dir removed on drop (also on a panic).
struct TempDir(std::path::PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The child's body; a no-op unless the parent started this exe for it.
#[test]
fn closed_stderr_child() {
    if std::env::var(CHILD_ENV).as_deref() != Ok("closed_stderr") {
        return;
    }
    static QUIT: AtomicBool = AtomicBool::new(false);
    let dir = temp_dir("quit-child");
    let generation = u64::MAX - 101;
    let mut w = Writer { out_dir: dir.to_string_lossy().into_owned(), json: false, recorder: Recorder::new(FakeFactory::default(), tracks()), generation, skew: Duration::ZERO, parts_so_far: Vec::new(), missed: (0.0, 0.0), lock: LockClock::default() };
    w.start("G", Some((0, Instant::now())), None);
    w.generation = generation;
    w.data(Data::Video { pts_us: 0, keyframe: true, data: pkt(1), generation });
    w.data(Data::Video { pts_us: 16 * MS, keyframe: false, data: pkt(2), generation });
    assert!(w.recorder.is_active(), "a long recording is running");
    let (ctl, ctl_rx) = channel::<Control>();
    let (_data_tx, data_rx) = sync_channel::<Data>(4);
    let (done_tx, done) = channel::<()>();
    std::thread::spawn(move || {
        let _done = done_tx;
        w.run(&ctl_rx, &data_rx);
    });
    let longrec = LongRec { ctl, done: std::sync::Mutex::new(done) };
    // The release `eprintln!` to the broken stderr: quiets it and says so once on stdout,
    // before the end of stdin quiets stdout too.
    crate::quiet_io::err(format_args!("[recall] child up; stderr is a broken pipe"));
    std::thread::spawn(|| {
        let end = crate::engine_stdin::read_early(std::io::stdin().lock(), |_| {});
        crate::stdin_reader::finish(end, &QUIT);
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !QUIT.load(Relaxed) {
        assert!(Instant::now() < deadline, "stdin EOF never set quit");
        std::thread::sleep(Duration::from_millis(10));
    }
    eprintln!("[recall] quitting");
    longrec.shutdown(Duration::from_millis(1500));
    let names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(names.iter().all(|n| !n.ends_with(".part")) && names.len() == 1, "finalized, no .part: {names:?}");
    std::fs::write(std::env::var(CHILD_OK_ENV).unwrap(), "quit-ok").unwrap();
    std::process::exit(0);
}

/// The child's exit status, its stdout, and its marker file's text.
fn run_child() -> (std::process::ExitStatus, String, String) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let marker_dir = TempDir(temp_dir("quit-child-ok"));
    let marker = marker_dir.0.join("ok.txt");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([CHILD_TEST, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, "closed_stderr")
        .env(CHILD_OK_ENV, &marker)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .expect("start the child");
    drop(child.stderr.take()); // the controller is gone: its stderr reader too
    drop(child.stdin.take()); // ...and the engine's stdin hits EOF
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the child did not exit within 5 s (quit never seen)");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let out = reader.join().unwrap();
    let ok = std::fs::read_to_string(&marker).unwrap_or_default();
    (status, out, ok)
}

#[test]
fn a_dead_controller_still_quits_the_engine_and_finalizes_the_recording() {
    let (status, out, ok) = run_child();
    assert_eq!(status.code(), Some(0), "clean exit; child stdout: {out}");
    assert_eq!(ok, "quit-ok", "the child reached its end; stdout: {out}");
}

/// R8-overlay-4: a broken stderr leaves exactly one `encoder_log` line on the protocol
/// (`stdin_reader::stream_failed`), so the controller's log shows the engine went quiet.
#[test]
fn a_broken_stderr_says_so_once_on_the_protocol() {
    let (_, out, _) = run_child();
    let notices = out
        .lines()
        // The harness may print `test <name> ... ` on the same line, before the protocol line.
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(&l[l.find('{')?..]).ok())
        .filter(|v| v["type"] == "encoder_log" && v["message"] == crate::stdin_reader::STDERR_LOST)
        .count();
    assert_eq!(notices, 1, "child stdout: {out}");
}
