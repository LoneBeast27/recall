//! The manager thread waits on events, not a 50 ms poll: a stop, a retarget or the engine's
//! exit ends its wait at once, and nothing else wakes it before [`CHANGE_BACKSTOP`].

use super::*;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};

/// Well under the backstop: a missing wake would show as a wait of ~5 s.
const PROMPT: Duration = Duration::from_millis(1_500);

fn after(delay: Duration, f: impl FnOnce() + Send + 'static) -> JoinHandle<()> {
    thread::spawn(move || {
        thread::sleep(delay);
        f();
    })
}

#[test]
fn an_idle_manager_wakes_on_a_retarget_and_on_a_stop() {
    let shared = Arc::new(Shared::new(None));
    let gen = shared.generation();
    let s = shared.clone();
    let poke = after(Duration::from_millis(100), move || s.set_desired(None, Sent::Switching));
    let t = Instant::now();
    assert!(wait_for_change(&shared, gen), "a retarget");
    assert!(t.elapsed() < PROMPT, "{:?}", t.elapsed());
    poke.join().unwrap();

    let gen = shared.generation();
    let s = shared.clone();
    let stop = after(Duration::from_millis(100), move || s.request_stop(Duration::ZERO));
    let t = Instant::now();
    assert!(!wait_for_change(&shared, gen), "a stop");
    assert!(t.elapsed() < PROMPT, "{:?}", t.elapsed());
    stop.join().unwrap();
}

#[test]
fn an_idle_manager_is_not_woken_by_a_timer() {
    let shared = Arc::new(Shared::new(None));
    // Nothing happens: the wait runs to its backstop (it used to re-read every 50 ms).
    let t = Instant::now();
    let woke = shared.change.wait_with(None, Duration::from_millis(400));
    assert_eq!(woke, crate::record_app::pump_wake::Woke::Timeout);
    assert!(t.elapsed() >= Duration::from_millis(390), "{:?}", t.elapsed());
}

#[test]
fn a_crash_backoff_is_cut_short_by_a_retarget() {
    let shared = Arc::new(Shared::new(None));
    let gen = shared.generation();
    let s = shared.clone();
    let poke = after(Duration::from_millis(100), move || s.set_desired(Some(EngineSpec::default()), Sent::Switching));
    let t = Instant::now();
    assert!(sleep_unless_change(&shared, Duration::from_secs(30), gen));
    assert!(t.elapsed() < PROMPT, "{:?}", t.elapsed());
    poke.join().unwrap();
}

#[test]
fn a_supervised_engine_is_seen_ending_at_once_and_a_retarget_ends_the_watch() {
    let shared = Arc::new(Shared::new(None));
    let gen = shared.generation();
    let cmd = std::env::var_os("ComSpec").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\cmd.exe"));
    // An engine that exits by itself after ~300 ms (ping waits ~1 s per extra echo; no window).
    let mut quick = Command::new(&cmd).args(["/D", "/C", "ping -n 2 127.0.0.1 >NUL & exit 3"]).stdout(Stdio::null()).stderr(Stdio::null()).creation_flags(super::super::CREATE_NO_WINDOW).spawn().expect("cmd");
    let t = Instant::now();
    assert!(matches!(supervise_child(&shared, &mut quick, gen), ChildExit::Clean(3)));
    assert!(t.elapsed() < Duration::from_secs(4), "seen at its exit, not on a poll: {:?}", t.elapsed());

    let mut wedged = wedged_engine();
    let s = shared.clone();
    let poke = after(Duration::from_millis(100), move || s.set_desired(None, Sent::Switching));
    let t = Instant::now();
    assert!(matches!(supervise_child(&shared, &mut wedged, gen), ChildExit::Retarget));
    assert!(t.elapsed() < PROMPT, "{:?}", t.elapsed());
    poke.join().unwrap();
    let _ = wedged.kill();
    let _ = wedged.wait();
}

#[test]
fn a_plain_quit_grace_ends_when_the_engine_exits_not_on_a_poll() {
    let shared = Arc::new(Shared::new(None));
    let cmd = std::env::var_os("ComSpec").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\cmd.exe"));
    let child = Command::new(&cmd).args(["/D", "/C", "ping -n 2 127.0.0.1 >NUL"]).stdout(Stdio::null()).stderr(Stdio::null()).creation_flags(super::super::CREATE_NO_WINDOW).spawn().expect("cmd");
    let t = Instant::now();
    assert!(finish_shutdown(&shared, Box::new(child), Duration::from_secs(20), Some(Sent::NotResponding), false).is_none(), "exited in its grace");
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
}

#[test]
fn every_engine_line_and_state_change_wakes_the_controllers_pump() {
    use crate::record_app::pump_wake::pump;
    let shared = Arc::new(Shared::new(None));
    let before = pump().wakes();
    let lines = b"{\"type\":\"hello\",\"sync_hold\":true}\n{\"type\":\"status\",\"encoder\":\"h264_nvenc\"}\n";
    super::super::protocol::read_protocol(&shared, &lines[..]);
    assert!(pump().wakes() >= before + 2, "one per line, after its caches");
    assert_eq!(shared.hello_count.load(Ordering::SeqCst), 1);
    let before = pump().wakes();
    shared.set_health(EngineHealth::Restarting { attempt: 1 });
    shared.forward_exit();
    assert!(pump().wakes() >= before + 2, "health and exit");
}
