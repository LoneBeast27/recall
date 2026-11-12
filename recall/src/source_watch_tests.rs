use super::*;

/// The tests that read or set the shared `CHANGED` flag run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

#[test]
fn scans_on_an_event_or_the_backstop_only() {
    let t0 = Instant::now();
    let soon = t0 + Duration::from_secs(1);
    assert!(scan_due(true, false, None, t0), "the first scan runs at once");
    assert!(!scan_due(true, false, Some(t0), soon), "no event, inside the backstop: no scan");
    assert!(scan_due(true, true, Some(t0), soon), "a foreground / move / minimize / rename event scans");
    assert!(scan_due(true, false, Some(t0), t0 + BACKSTOP), "the backstop scans");
}

#[test]
fn without_hooks_it_scans_every_tick() {
    let t0 = Instant::now();
    assert!(scan_due(false, false, Some(t0), t0 + Duration::from_secs(1)));
}

/// The real hooks install (out of context: nothing is loaded into another process), and
/// with them the second tick of a quiet desktop skips the scan.
#[test]
fn the_hooks_install_and_quiet_ticks_skip_the_scan() {
    let _one = serial();
    let mut watch = SourceWatch::default();
    let t0 = Instant::now();
    assert!(watch.due(t0), "the first scan runs");
    assert_eq!(watch.hooks.len(), EVENTS.len(), "every hook installed");
    // This test thread pumps no messages, so no hooked event can arrive here.
    assert!(!watch.due(t0 + Duration::from_secs(1)));
    assert!(watch.due(t0 + BACKSTOP));
}

/// The rename hook is scoped to the source's process and moves only with it.
#[test]
fn the_rename_hook_follows_the_source_process() {
    let mut watch = SourceWatch::default();
    let window = |pid, hwnd| source::Found { name: "Game".into(), pid, hwnd };
    watch.follow(&window(4, 0x10));
    let first = watch.name_hook.map(|(pid, h)| (pid, h.0 as usize));
    assert_eq!(first.map(|f| f.0), Some(4), "hooked for the source's pid");
    watch.follow(&window(4, 0x20));
    assert_eq!(watch.name_hook.map(|(pid, h)| (pid, h.0 as usize)), first, "same process: same hook");
    assert_eq!(SOURCE_HWND.load(Relaxed), 0x20, "a new window of it is followed");
    watch.follow(&window(8, 0x30));
    assert_eq!(watch.name_hook.map(|(pid, _)| pid), Some(8), "another process: hooked again");
    SOURCE_HWND.store(0, Relaxed);
}

#[test]
fn only_the_source_window_itself_renaming_counts() {
    let (src, obj, me) = (0x10, OBJID_WINDOW.0, CHILDID_SELF as i32);
    assert!(is_source_rename(src, obj, me, src));
    assert!(!is_source_rename(0x11, obj, me, src), "another window of the process");
    assert!(!is_source_rename(src, obj + 1, me, src), "not the window object");
    assert!(!is_source_rename(src, obj, 3, src), "a child element (a control)");
    assert!(!is_source_rename(0, obj, me, 0), "no source yet");
}

/// The save is the event: the clip takes the name the screen shows now, not the last tick's.
#[test]
fn a_save_takes_the_name_scanned_at_save_time() {
    let _one = serial();
    let shared = Mutex::new("Loading…".to_string());
    CHANGED.store(false, Relaxed);
    assert_eq!(fresh_name_with(&shared, true, || Some("Game".into())), "Game");
    assert_eq!(*shared.lock().unwrap(), "Game", "the shared name follows");
    assert!(CHANGED.swap(false, Relaxed), "the loop rescans to rename its window and status");
    assert_eq!(fresh_name_with(&shared, true, || Some("Game".into())), "Game");
    assert!(!CHANGED.load(Relaxed), "an unchanged name asks nothing of the loop");
}

#[test]
fn a_named_capture_or_an_empty_scan_keeps_the_name() {
    let _one = serial();
    let shared = Mutex::new("Desktop".to_string());
    let scanned = std::cell::Cell::new(false);
    let name = fresh_name_with(&shared, false, || {
        scanned.set(true);
        Some("Other".into())
    });
    assert_eq!(name, "Desktop");
    assert!(!scanned.get(), "a controller-named source is never scanned");
    assert_eq!(fresh_name_with(&shared, true, || None), "Desktop", "nothing qualifies: the name stays");
    CHANGED.store(false, Relaxed);
}
