use super::*;
use std::cell::{Cell, RefCell};
use std::io;

const MS250: Duration = Duration::from_millis(250);
const S30: Duration = Duration::from_secs(30);
const INSTALLER: u32 = 4242;

fn missing() -> io::Result<Vec<u8>> {
    Err(io::Error::from(ErrorKind::NotFound))
}

fn pid_file(pid: u32) -> io::Result<Vec<u8>> {
    Ok(pid.to_string().into_bytes()) // as install-swap.ps1 writes it: digits, no newline
}

#[test]
fn no_marker_returns_at_once_without_sleeping_or_asking_for_a_pid() {
    let sleeps = RefCell::new(Vec::new());
    let asked = Cell::new(0);
    let got = wait_while_installing(Path::new("x/.installing"), MS250, S30, |_| missing(), |_| { asked.set(asked.get() + 1); true }, |d| sleeps.borrow_mut().push(d));
    assert_eq!(got, Waited::NoMarker);
    assert!(sleeps.borrow().is_empty());
    assert_eq!(asked.get(), 0);
}

#[test]
fn a_marker_removed_after_n_polls_returns_after_n_polls() {
    let sleeps = RefCell::new(Vec::new());
    let reads = Cell::new(0);
    // Present (live installer) at the first look and after 3 sleeps; gone after the 4th.
    let got = wait_while_installing(
        Path::new("x/.installing"),
        MS250,
        S30,
        |_| {
            reads.set(reads.get() + 1);
            if reads.get() <= 4 { pid_file(INSTALLER) } else { missing() }
        },
        |pid| pid == INSTALLER,
        |d| sleeps.borrow_mut().push(d),
    );
    assert_eq!(got, Waited::Cleared { polls: 4 });
    assert_eq!(*sleeps.borrow(), vec![MS250; 4]);
}

#[test]
fn a_marker_that_never_goes_gives_up_at_the_timeout() {
    let total = Cell::new(Duration::ZERO);
    let got = wait_while_installing(Path::new("x/.installing"), MS250, S30, |_| pid_file(INSTALLER), |_| true, |d| total.set(total.get() + d));
    assert_eq!(got, Waited::TimedOut { polls: 120 });
    assert_eq!(total.get(), S30);
}

#[test]
fn the_last_sleep_never_runs_past_the_timeout() {
    let total = Cell::new(Duration::ZERO);
    let got = wait_while_installing(Path::new("x"), Duration::from_millis(400), Duration::from_millis(1000), |_| pid_file(INSTALLER), |_| true, |d| total.set(total.get() + d));
    assert_eq!(got, Waited::TimedOut { polls: 3 });
    assert_eq!(total.get(), Duration::from_millis(1000));
}

#[test]
fn a_dead_installer_pid_starts_at_once() {
    let slept = Cell::new(false);
    let got = wait_while_installing(Path::new("x/.installing"), MS250, S30, |_| pid_file(INSTALLER), |_| false, |_| slept.set(true));
    assert!(matches!(got, Waited::Stale { polls: 0, ref reason } if reason.contains("4242")), "{got:?}");
    assert!(!slept.get());
}

#[test]
fn an_installer_that_dies_mid_wait_starts_at_the_next_poll() {
    let looks = Cell::new(0);
    let got = wait_while_installing(Path::new("x"), MS250, S30, |_| pid_file(INSTALLER), |_| { looks.set(looks.get() + 1); looks.get() <= 2 }, |_| {});
    assert!(matches!(got, Waited::Stale { polls: 2, .. }), "{got:?}");
}

#[test]
fn garbage_contents_start_at_once() {
    for junk in [&b"not a pid"[..], b"-5", b"12ab", b"0", b"99999999999", &[0xC3, 0x28]] {
        let slept = Cell::new(false);
        let got = wait_while_installing(Path::new("x"), MS250, S30, |_| Ok(junk.to_vec()), |_| true, |_| slept.set(true));
        assert!(matches!(got, Waited::Stale { polls: 0, .. }), "{junk:?}: {got:?}");
        assert!(!slept.get(), "{junk:?}");
    }
}

#[test]
fn an_unreadable_marker_starts_at_once() {
    let got = wait_while_installing(Path::new("x"), MS250, S30, |_| Err(io::Error::from(ErrorKind::PermissionDenied)), |_| true, |_| {});
    assert!(matches!(got, Waited::Stale { polls: 0, ref reason } if reason.contains("unreadable")), "{got:?}");
}

#[test]
fn a_marker_caught_mid_write_is_read_again_then_waited_on() {
    // Empty at the first look (created, pid not yet written), then the live pid, then gone.
    let reads = Cell::new(0);
    let got = wait_while_installing(
        Path::new("x"),
        MS250,
        S30,
        |_| {
            reads.set(reads.get() + 1);
            match reads.get() { 1 => Ok(Vec::new()), 2 | 3 => pid_file(INSTALLER), _ => missing() }
        },
        |_| true,
        |_| {},
    );
    assert_eq!(got, Waited::Cleared { polls: 3 });
}

#[test]
fn a_marker_that_stays_empty_is_stale_after_a_second() {
    let got = wait_while_installing(Path::new("x"), MS250, S30, |_| Ok(Vec::new()), |_| true, |_| {});
    assert!(matches!(got, Waited::Stale { polls: 4, .. }), "{got:?}");
}

#[test]
fn the_pid_parses_as_powershell_may_write_it() {
    assert_eq!(parse_pid(b"4242"), Some(4242));
    assert_eq!(parse_pid(b"4242\r\n"), Some(4242));
    assert_eq!(parse_pid(b"\xEF\xBB\xBF4242"), Some(4242)); // UTF-8 BOM
    assert_eq!(parse_pid(&[0xFF, 0xFE, b'4', 0, b'2', 0]), Some(42)); // UTF-16 LE
    assert_eq!(parse_pid(b""), None);
    assert_eq!(parse_pid(b" "), None);
}

#[test]
fn this_process_is_alive_and_a_pid_from_nowhere_is_not() {
    assert!(pid_alive(std::process::id()));
    // Pids are multiples of 4 on Windows: an odd one never names a process.
    assert!(!pid_alive(0xFFFF_FFF1));
}

#[test]
fn the_marker_sits_beside_the_program() {
    let path = marker_path().expect("test binary has a folder");
    assert_eq!(path.file_name().and_then(|n| n.to_str()), Some(MARKER));
    assert_eq!(path.parent(), std::env::current_exe().unwrap().parent());
}

/// A program swapped in by the install (a new file at the same path) is told apart from the one
/// this process started from; the same file, or a rename of it, is not.
#[test]
fn a_swapped_program_file_has_a_new_identity_and_restarts_only_after_a_wait() {
    let dir = crate::test_temp::TempDir::new(format!("recall-install-id-{}", std::process::id()));
    let exe = dir.join("recall-native.exe");
    std::fs::write(&exe, b"old").unwrap();
    let first = file_id(&exe);
    assert!(first.is_some());
    assert_eq!(file_id(&exe), first, "the same file keeps its identity");
    // What install-swap does: the old file is renamed aside, the new one moves in.
    std::fs::create_dir_all(dir.join(".old")).unwrap();
    std::fs::rename(&exe, dir.join(".old").join("recall-native.exe")).unwrap();
    assert_eq!(file_id(&dir.join(".old").join("recall-native.exe")), first, "a rename keeps the identity");
    std::fs::write(&exe, b"new").unwrap();
    let swapped = file_id(&exe);
    assert!(swapped.is_some() && swapped != first, "the new file at the path is another file");
    assert!(needs_restart(&Waited::Cleared { polls: 3 }, first, swapped), "waited and swapped: start the new program");
    assert!(needs_restart(&Waited::TimedOut { polls: 120 }, first, swapped));
    assert!(!needs_restart(&Waited::NoMarker, first, swapped), "no wait: carry on");
    assert!(!needs_restart(&Waited::Stale { reason: String::new(), polls: 0 }, first, swapped), "a stale marker: carry on");
    assert!(!needs_restart(&Waited::Cleared { polls: 3 }, first, first), "waited but not swapped: carry on");
    assert!(!needs_restart(&Waited::Cleared { polls: 3 }, None, swapped), "unknown identity: never restart");
}
