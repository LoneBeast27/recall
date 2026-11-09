//! The engine's command line and its single-instance guard, run as a real process.
//!
//! Every launch here must end without recording. In case a regression would start a recorder,
//! each one uses a temporary clip folder, the Steam streaming mic only, and is killed after
//! a timeout (the test then fails).
#[path = "../src/single_instance.rs"]
mod single_instance;

use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const EXE: &str = env!("CARGO_BIN_EXE_recall");

fn engine(args: &[&str], tag: &str) -> Child {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::env::temp_dir().join(format!("recall-cli-test-{}", std::process::id()));
    Command::new(EXE)
        .args(args)
        .env(single_instance::TAG_ENV, tag)
        .env("RECALL_OUT_DIR", &out)
        .env("RECALL_AUDIO_MIC", "microphone steam streaming")
        .env("RECALL_AUDIO_EXACT", "1")
        .env_remove("RECALL_SHOW")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .expect("start recall.exe")
}

/// Waits for the process; kills it and panics if it is still running after `limit`.
fn finish(mut child: Child, limit: Duration, what: &str) -> Output {
    let started = Instant::now();
    loop {
        if child.try_wait().expect("poll recall.exe").is_some() {
            return child.wait_with_output().expect("collect output");
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{what}: recall.exe still running after {limit:?}, so it started recording");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn tag(name: &str) -> String {
    format!("cli-{name}-{}", std::process::id())
}

#[test]
fn help_prints_usage_and_exits_without_recording() {
    for flag in ["--help", "-h"] {
        let out = finish(engine(&[flag], &tag("help")), Duration::from_secs(15), flag);
        assert_eq!(out.status.code(), Some(0), "{flag} exits 0");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("usage: recall"), "{flag} prints usage, got: {text}");
    }
}

#[test]
fn help_works_while_an_engine_holds_the_guard() {
    let tag = tag("help-held");
    let name = format!("Local\\{}.{tag}", single_instance::ENGINE);
    let held = single_instance::claim(&name);
    assert!(held.is_first(), "the test holds the engine guard");
    let out = finish(engine(&["--help"], &tag), Duration::from_secs(15), "--help while held");
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("usage: recall"));
}

#[test]
fn unknown_arguments_are_refused_without_recording() {
    let out = finish(engine(&["--bogus"], &tag("bogus")), Duration::from_secs(15), "--bogus");
    assert_eq!(out.status.code(), Some(2), "unknown argument exits 2");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unknown argument '--bogus'") && err.contains("usage: recall"), "got: {err}");
}

#[test]
fn a_second_recorder_exits_while_one_holds_the_guard() {
    let tag = tag("second");
    let name = format!("Local\\{}.{tag}", single_instance::ENGINE);
    let held = single_instance::claim(&name);
    assert!(held.is_first(), "the test stands in for the running engine");
    let started = Instant::now();
    let out = finish(engine(&["--json"], &tag), Duration::from_secs(20), "second recorder");
    assert_eq!(out.status.code(), Some(86), "a second recorder exits 86 (not abort()'s 3); stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("another Recall engine is already recording"), "the controller is told why: {stdout}");
    assert!(!stdout.contains("\"hello\""), "it never announced itself as a recorder: {stdout}");
    // It waited for a restart window, then gave up.
    assert!(started.elapsed() >= Duration::from_secs(3), "waited {:?}", started.elapsed());
    drop(held);
}
