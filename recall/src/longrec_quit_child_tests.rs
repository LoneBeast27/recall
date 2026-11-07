//! A dead controller: the engine's stdin hits EOF and its stderr pipe is broken.
//! The test re-runs its own test exe as the "engine" (no capture or audio device):
//! the child runs the real stdin-command thread (`stdin_commands`) with a real quit
//! flag, logs through the engine's `eprintln!` (`stderr_log`), and has a long
//! recording running on the writer thread with the fake sink. On quit it takes
//! the engine's quit-time `LongRec::shutdown`, checks no `.part` is left, prints
//! `quit-ok` and exits 0.

use super::*;

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::sync_channel;

use test_sink::{pkt, temp_dir, tracks, FakeFactory, MS};

const CHILD_ENV: &str = "RECALL_TEST_CHILD";
const CHILD_TEST: &str = "longrec::quit_child_tests::closed_stderr_child";

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
    w.start("G", Some((0, Instant::now())));
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
    std::thread::spawn(|| crate::stdin_commands::read_commands(std::io::stdin().lock(), &QUIT, |_| {}));
    eprintln!("[recall] child up; stderr is a broken pipe");
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
    println!("quit-ok");
    std::process::exit(0);
}

#[test]
fn a_dead_controller_still_quits_the_engine_and_finalizes_the_recording() {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([CHILD_TEST, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, "closed_stderr")
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
    assert_eq!(status.code(), Some(0), "clean exit; child stdout: {out}");
    assert!(out.contains("quit-ok"), "child stdout: {out}");
}
