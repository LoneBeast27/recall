use super::*;
use std::sync::mpsc::channel;

/// Clear the global slot so tests that assert on install-state start clean.
/// Tests touching the singleton run under a shared serial lock (below) so this
/// never races another test.
fn reset_state() {
    let mut slot = state().lock().unwrap_or_else(|p| p.into_inner());
    *slot = None;
}

/// Serialize the tests that mutate the process-global watcher slot. Two
/// installs in parallel would legitimately conflict (that's the feature), so we
/// force them sequential to test each in isolation.
static SERIAL: Mutex<()> = Mutex::new(());

// --- Steam library paths (pure) ------------------------------------------

#[test]
fn steam_library_paths_mark_games_but_not_redistributables() {
    assert!(is_steam_library_path(r"G:\SteamLibrary\steamapps\common\sobosuba\sobosuba.exe"));
    assert!(is_steam_library_path(r"C:\Program Files (x86)\Steam\SteamApps\Common\Game\bin\game.exe"));
    assert!(is_steam_library_path("D:/Steam/steamapps/common/Game/game.exe"));
    assert!(!is_steam_library_path(r"C:\Program Files (x86)\Steam\steam.exe"));
    assert!(!is_steam_library_path(r"G:\SteamLibrary\steamapps\common\Steamworks Shared\_CommonRedist\vcredist\vc_redist.x64.exe"));
    assert!(!is_steam_library_path(r"C:\Windows\notepad.exe"));
}

// --- rect_covers geometry (pure, no Win32) -------------------------------

#[test]
fn rect_covers_exact_match() {
    let mon = RECT { left: 0, top: 0, right: 3840, bottom: 2160 };
    let win = mon;
    assert!(rect_covers(&mon, &win, FULLSCREEN_TOL));
}

#[test]
fn rect_covers_one_px_overhang_is_fullscreen() {
    // Borderless windows overhang by ~1px -- must still count as covering.
    let mon = RECT { left: 0, top: 0, right: 3840, bottom: 2160 };
    let win = RECT { left: -1, top: -1, right: 3841, bottom: 2161 };
    assert!(rect_covers(&mon, &win, FULLSCREEN_TOL));
}

#[test]
fn rect_covers_tolerance_boundary() {
    let mon = RECT { left: 0, top: 0, right: 3840, bottom: 2160 };
    // Exactly at tolerance on every edge: still covers.
    let at = RECT {
        left: FULLSCREEN_TOL,
        top: FULLSCREEN_TOL,
        right: 3840 - FULLSCREEN_TOL,
        bottom: 2160 - FULLSCREEN_TOL,
    };
    assert!(rect_covers(&mon, &at, FULLSCREEN_TOL));
    // One px past tolerance on a single edge: does not cover.
    let over = RECT { left: FULLSCREEN_TOL + 1, ..at };
    assert!(!rect_covers(&mon, &over, FULLSCREEN_TOL));
}

#[test]
fn rect_covers_windowed_is_not_fullscreen() {
    let mon = RECT { left: 0, top: 0, right: 3840, bottom: 2160 };
    let win = RECT { left: 100, top: 100, right: 1200, bottom: 800 };
    assert!(!rect_covers(&mon, &win, FULLSCREEN_TOL));
}

#[test]
fn rect_covers_offset_monitor() {
    // Secondary monitor to the right of primary (non-zero origin).
    let mon = RECT { left: 3840, top: 0, right: 7680, bottom: 2160 };
    let win = mon;
    assert!(rect_covers(&mon, &win, FULLSCREEN_TOL));
    // A window at the primary's origin does NOT cover the secondary.
    let primary_win = RECT { left: 0, top: 0, right: 3840, bottom: 2160 };
    assert!(!rect_covers(&mon, &primary_win, FULLSCREEN_TOL));
}

#[test]
fn fullscreen_geometry_on_a_secondary_monitor_counts() {
    // The record mode attributes fullscreen to the window's OWN monitor: a
    // borderless game on the right-hand display is fullscreen there.
    let secondary = RECT { left: 2560, top: 0, right: 5120, bottom: 1440 };
    assert!(is_fullscreen_geometry(&secondary, &secondary, false, false));
    // Exclusive/borderless games are WS_POPUP: a non-captioned window that
    // happens to be maximized still counts.
    assert!(is_fullscreen_geometry(&secondary, &secondary, true, false));
}

#[test]
fn maximized_captioned_app_is_not_fullscreen() {
    // Custom-frame apps (Electron, Chrome) maximized on a taskbar-less monitor
    // have rect == monitor; they are app windows, not games.
    let mon = RECT { left: 0, top: 0, right: 2560, bottom: 1440 };
    assert!(!is_fullscreen_geometry(&mon, &mon, true, true));
    // A windowed (non-covering) window is never fullscreen.
    let win = RECT { left: 100, top: 100, right: 1200, bottom: 800 };
    assert!(!is_fullscreen_geometry(&mon, &win, false, false));
}

// --- single-instance install semantics -----------------------------------

#[test]
fn second_install_is_rejected() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    reset_state();

    let (tx1, _rx1) = channel::<WatchEvent>();
    let w1 = Watcher::install(tx1, 0x1234)
        .expect("first install succeeds (or SetWinEventHook is unavailable in CI)");

    // Second install must be refused while the first is live.
    let (tx2, _rx2) = channel::<WatchEvent>();
    let err = Watcher::install(tx2, 0x5678)
        .expect_err("second concurrent install must be rejected");
    assert!(err.contains("already installed"), "unexpected error: {err}");

    // Dropping the first frees the slot; a fresh install then succeeds.
    drop(w1);
    let (tx3, _rx3) = channel::<WatchEvent>();
    let w3 = Watcher::install(tx3, 0x9abc).expect("install after drop succeeds");
    drop(w3);
}

#[test]
fn uninstall_clears_slot_for_reinstall() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    reset_state();

    let (tx, _rx) = channel::<WatchEvent>();
    let w = Watcher::install(tx, 0x1).expect("install succeeds");
    // Explicit uninstall (consumes -> Drop -> clears the global slot).
    w.uninstall();

    let mut slot = state().lock().unwrap_or_else(|p| p.into_inner());
    assert!(slot.is_none(), "uninstall must clear the global watcher slot");
    *slot = None;
}

/// The vendored `exe_stem` must resolve the current test process to a non-empty
/// stem (exercises the OpenProcess + QueryFullProcessImageNameW path for real).
#[test]
fn exe_stem_resolves_self() {
    let stem = exe_stem(std::process::id());
    assert!(stem.is_some(), "exe_stem(self) should resolve");
    assert!(!stem.unwrap().trim().is_empty(), "self stem is non-empty");
}

// --- scoped location hook -------------------------------------------------

#[test]
fn follow_without_a_sink_or_window_installs_nothing() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    reset_state();
    let (tx, _rx) = channel::<WatchEvent>();
    let mut w = Watcher::install(tx, 0x1).expect("install succeeds");
    // No sink: asked, but nothing to send to.
    w.follow(Some(unsafe { GetForegroundWindow() }.0 as isize));
    assert!(w.followed().is_none() && !follow::installed());
    // A dead window is refused, and is not retried while it stays asked.
    let (ftx, _frx) = channel::<WatchEvent>();
    w.follow_into(ftx);
    w.follow(None);
    w.follow(Some(0x7fff_fff0));
    assert!(w.followed().is_none() && !follow::installed());
    drop(w);
    assert!(!follow::installed());
}

#[test]
fn follow_hooks_a_live_window_and_drop_unhooks_it() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    reset_state();
    // SAFETY: plain query. The desktop window always exists and is owned by
    // csrss; any live top-level window of another process works too.
    let hwnd = unsafe { GetForegroundWindow() }.0 as isize;
    if hwnd == 0 {
        return; // no foreground (locked session): nothing to hook
    }
    let (tx, _rx) = channel::<WatchEvent>();
    let (ftx, _frx) = channel::<WatchEvent>();
    let mut w = Watcher::install(tx, 0x1).expect("install succeeds");
    w.follow_into(ftx);
    w.follow(Some(hwnd));
    assert_eq!(w.followed(), Some(hwnd));
    assert!(follow::installed());
    w.follow(None);
    assert!(w.followed().is_none() && !follow::installed(), "None unhooks");
    w.follow(Some(hwnd));
    assert!(follow::installed());
    drop(w);
    assert!(!follow::installed(), "dropping the watcher unhooks the scoped hook too");
}

/// Cost probe (run by hand, never in CI): follow the foreground window like
/// the record mode does for `RECALL_FOLLOW_SECS` (default 20) while the PC is
/// used normally, then print what the scoped hook delivered. Shows whether
/// cursor / caret moves leak through the thread filter.
#[test]
#[ignore]
fn follow_hook_cost_probe() {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    use windows::Win32::UI::WindowsAndMessaging::{MsgWaitForMultipleObjectsEx, MWMO_INPUTAVAILABLE, QS_ALLINPUT};
    fn cpu_100ns() -> u64 {
        let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        // SAFETY: out-params on the current process.
        let _ = unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
        let v = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
        v(k) + v(u)
    }
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    reset_state();
    let secs: u64 = std::env::var("RECALL_FOLLOW_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(20);
    let (tx, _rx) = channel::<WatchEvent>();
    let (ftx, frx) = channel::<WatchEvent>();
    let mut w = Watcher::install(tx, 0x1).expect("install succeeds");
    w.follow_into(ftx);
    let before = follow::stats();
    let cpu0 = cpu_100ns();
    let t0 = std::time::Instant::now();
    let mut windows = Vec::<String>::new();
    let (mut wakes, mut flips) = (0u64, 0u64);
    while t0.elapsed().as_secs() < secs {
        let fg = unsafe { GetForegroundWindow() };
        if w.followed() != Some(fg.0 as isize) {
            w.follow(Some(fg.0 as isize));
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(fg, Some(&mut pid)) };
            windows.push(exe_stem(pid).unwrap_or_default());
        }
        unsafe { MsgWaitForMultipleObjectsEx(None, 50, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
        wakes += 1;
        let _ = crate::app::drain_messages();
        while frx.try_recv().is_ok() {
            flips += 1;
        }
    }
    let wall = t0.elapsed().as_secs_f64();
    let cpu_ms = (cpu_100ns() - cpu0) as f64 / 10_000.0;
    drop(w);
    let s = follow::stats();
    let d = |a: u64, b: u64| a - b;
    let events = d(s.events, before.events);
    let checked = d(s.checked, before.checked);
    println!(
        "FOLLOW-COST secs={wall:.1} windows={windows:?} events={events} ({:.1}/s) cursor={} caret={} other_object={} other_window={} checked={checked} sent={} flips_received={flips} avg_check_us={:.2} pump_wakes={wakes} process_cpu_ms={cpu_ms:.1}",
        events as f64 / wall,
        d(s.cursor, before.cursor),
        d(s.caret, before.caret),
        d(s.other_object, before.other_object),
        d(s.other_window, before.other_window),
        d(s.sent, before.sent),
        if checked > 0 { d(s.checked_ns, before.checked_ns) as f64 / checked as f64 / 1000.0 } else { 0.0 },
    );
}
