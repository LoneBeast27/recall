use super::*;

fn at(base: Instant, ms: u64) -> Instant {
    base + Duration::from_millis(ms)
}

#[test]
fn watch_fires_only_after_an_unbroken_stretch() {
    let t = Instant::now();
    let mut w = Watch::default();
    assert!(!w.update(Look::Holds, t, ENTER_AFTER));
    assert!(!w.update(Look::Holds, at(t, 1_900), ENTER_AFTER));
    assert!(w.update(Look::Holds, at(t, 2_000), ENTER_AFTER));
}

#[test]
fn a_break_starts_the_watch_again() {
    let t = Instant::now();
    let mut w = Watch::default();
    w.update(Look::Holds, t, ENTER_AFTER);
    assert!(!w.update(Look::Not, at(t, 1_500), ENTER_AFTER), "an alt-tab breaks it");
    assert!(!w.update(Look::Holds, at(t, 2_500), ENTER_AFTER), "counted from the return");
    assert!(w.update(Look::Holds, at(t, 4_500), ENTER_AFTER));
}

#[test]
fn looks_need_the_game_in_front() {
    assert_eq!(look(true, true, true), Look::Holds);
    assert_eq!(look(false, true, true), Look::Not, "another app in front never counts");
    assert_eq!(look(true, false, true), Look::Not);
    assert_eq!(look(true, false, false), Look::Holds, "the way back waits for a borderless game");
    assert_eq!(look(false, false, false), Look::Not, "the desktop in front is not the game leaving");
}

#[test]
fn keys_are_lower_case_file_names() {
    assert_eq!(key(r"D:\Games\Example Shooter\Bin\ExampleGame-Win64.exe").as_deref(), Some("examplegame-win64.exe"));
    assert_eq!(key("C:/Games/demo.EXE").as_deref(), Some("demo.exe"));
    assert_eq!(key(r"C:\Games\"), None);
    assert_eq!(key(""), None);
}

#[test]
fn parse_skips_blanks_and_duplicates() {
    assert_eq!(parse("one.exe\n\n  Two.exe \r\none.exe\n"), vec!["one.exe".to_string(), "two.exe".to_string()]);
    assert!(parse("").is_empty());
}

#[test]
fn edits_change_only_what_they_name() {
    let list = vec!["one.exe".to_string(), "two.exe".to_string()];
    assert_eq!(edited(&list, "three.exe", true), Some(vec!["one.exe".into(), "two.exe".into(), "three.exe".into()]));
    assert_eq!(edited(&list, "one.exe", true), None, "already listed: no write");
    assert_eq!(edited(&list, "one.exe", false), Some(vec!["two.exe".into()]));
    assert_eq!(edited(&list, "three.exe", false), None, "not listed: no write");
}

#[test]
fn render_round_trips() {
    let list = vec!["one.exe".to_string(), "two.exe".to_string()];
    assert_eq!(parse(&render(&list)), list);
}

#[test]
fn the_list_survives_a_write_and_a_read() {
    let dir = crate::user_dirs::appdata().unwrap().join("fullscreen-switch-test");
    let path = dir.join(FILE);
    let _ = std::fs::remove_file(&path);
    write(&path, &["one.exe".to_string()]).unwrap();
    assert_eq!(read(&path), vec!["one.exe".to_string()]);
    assert!(!path.with_extension("txt.tmp").exists(), "the temp file is renamed away");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_desktop_capture_never_gets_a_back_watch() {
    let shared = CaptureShared::new();
    shared.fullscreen_wgc.store(true, Relaxed);
    assert!(BackWatch::new(&shared).is_none(), "window 0 is a desktop capture");
}

#[test]
fn a_game_on_wgc_by_choice_gets_no_back_watch() {
    let shared = CaptureShared::new();
    shared.window.store(0x1234, Relaxed);
    assert!(BackWatch::new(&shared).is_none(), "never moved here for exclusive fullscreen");
}
