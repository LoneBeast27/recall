use super::*;
use crate::config::Override;

fn ev(stem: &str, title: &str, pid: u32, hwnd: isize, fullscreen: bool) -> WatchEvent {
    WatchEvent {
        pid,
        exe_stem: stem.into(),
        exe_name: format!("{stem}.exe"),
        title: title.into(),
        fullscreen,
        fullscreen_on_target: fullscreen,
        steam_game: false,
        hwnd,
    }
}

fn steam_ev(stem: &str) -> WatchEvent {
    WatchEvent { steam_game: true, ..ev(stem, stem, 7, 0x70, false) }
}

fn target(pid: u32, hwnd: isize, name: &str) -> Target {
    Target { hwnd, pid, name: name.into(), exe: format!("{name}.exe") }
}

fn user_cfg() -> Config {
    let mut cfg = Config::default();
    cfg.overrides.push(Override { exe: "brave".into(), kind: RuleKind::None, clamp: None });
    cfg.overrides.push(Override { exe: "windowedgame".into(), kind: RuleKind::SdrGame, clamp: None });
    cfg.overrides.push(Override { exe: "spotify".into(), kind: RuleKind::None, clamp: None });
    cfg
}

#[test]
fn fullscreen_unknown_game_is_a_target_named_by_title() {
    let cfg = user_cfg();
    let t = classify_event(&ev("VALORANT-Win64-Shipping", "VALORANT  ", 10, 0x10, true), &cfg).unwrap();
    assert_eq!(t.name, "VALORANT");
    assert_eq!(t.exe, "VALORANT-Win64-Shipping.exe");
    assert_eq!((t.pid, t.hwnd), (10, 0x10));
    let untitled = classify_event(&ev("RocketLeague", "", 11, 0x11, true), &cfg).unwrap();
    assert_eq!(untitled.name, "RocketLeague", "falls back to the exe stem");
}

#[test]
fn windowed_unknown_is_not_a_target_but_an_override_is() {
    let cfg = user_cfg();
    assert!(classify_event(&ev("notepad", "x", 1, 1, false), &cfg).is_none());
    assert!(classify_event(&ev("WindowedGame", "x", 1, 1, false), &cfg).is_some());
}

#[test]
fn windowed_steam_library_game_is_a_target_unless_denied() {
    let cfg = user_cfg();
    assert!(classify_event(&steam_ev("sobosuba"), &cfg).is_some(), "maximized windowed Steam game");
    for stem in ["wallpaper64", "ui32", "brave"] {
        assert!(classify_event(&steam_ev(stem), &cfg).is_none(), "{stem} in a Steam library is not a game");
    }
    let mut off = user_cfg();
    off.overrides.push(Override { exe: "sobosuba".into(), kind: RuleKind::None, clamp: None });
    assert!(classify_event(&steam_ev("sobosuba"), &off).is_none(), "a none override still wins");
}

#[test]
fn denied_and_none_override_stems_are_never_targets() {
    let cfg = user_cfg();
    for stem in ["explorer", "recall", "recall-hdr", "recall-native", "recall-overlay", "chrome", "msedge", "firefox", "brave", "spotify", "vlc", "NVIDIA Overlay", "GameBar", "steamwebhelper"] {
        assert!(!is_target(stem, true, &cfg), "{stem} fullscreen must not be recorded");
    }
    assert!(!is_target("  ", true, &cfg));
}

#[test]
fn default_game_class_none_still_records_fullscreen_games() {
    // HDR rules use default_game_class; capture does not.
    let mut cfg = user_cfg();
    cfg.behavior.default_game_class = RuleKind::None;
    assert!(is_target("eldenring", true, &cfg));
}

#[test]
fn sanitize_collapses_and_caps() {
    assert_eq!(sanitize("  Elden\tRing\u{7}  "), "Elden Ring");
    assert_eq!(sanitize(&"x".repeat(100)).chars().count(), 64);
    assert_eq!(display_name("", ""), "Game");
}

#[test]
fn first_game_commits_after_the_start_debounce() {
    let t0 = Instant::now();
    let mut tr = TargetTracker::new(TARGET_DEBOUNCE, START_DEBOUNCE);
    tr.on_foreground(Some(target(1, 1, "A")), t0);
    assert_eq!(tr.next_deadline(), Some(t0 + START_DEBOUNCE));
    assert!(!tr.poll(t0 + Duration::from_millis(299)));
    assert!(tr.current().is_none());
    assert!(tr.poll(t0 + START_DEBOUNCE));
    assert_eq!(tr.current().unwrap().pid, 1);
    assert_eq!(tr.take_last_wait(), Some(START_DEBOUNCE));
    assert_eq!(tr.take_last_wait(), None, "taken once");
}

#[test]
fn a_game_flashing_by_under_the_start_debounce_never_commits() {
    let t0 = Instant::now();
    let mut tr = TargetTracker::new(TARGET_DEBOUNCE, START_DEBOUNCE);
    tr.on_foreground(Some(target(1, 1, "A")), t0);
    tr.on_foreground(None, t0 + Duration::from_millis(200));
    assert!(!tr.poll(t0 + Duration::from_secs(5)));
    assert!(tr.current().is_none());
}

#[test]
fn seeing_the_pending_candidate_again_keeps_its_first_seen_time() {
    let t0 = Instant::now();
    let mut tr = TargetTracker::new(TARGET_DEBOUNCE, START_DEBOUNCE);
    tr.on_foreground(Some(target(1, 1, "A")), t0);
    tr.on_foreground(Some(target(1, 1, "A")), t0 + Duration::from_millis(250));
    assert!(tr.poll(t0 + START_DEBOUNCE), "a repeat event must not push the deadline");
}

#[test]
fn a_different_game_needs_the_full_debounce_against_a_live_session() {
    let t0 = Instant::now();
    let mut tr = TargetTracker::new(TARGET_DEBOUNCE, START_DEBOUNCE);
    tr.on_foreground(Some(target(1, 1, "A")), t0);
    tr.poll(t0 + START_DEBOUNCE);
    let t1 = t0 + Duration::from_secs(5);
    tr.on_foreground(Some(target(2, 2, "B")), t1);
    assert_eq!(tr.next_deadline(), Some(t1 + TARGET_DEBOUNCE));
    assert!(!tr.poll(t1 + Duration::from_millis(1999)));
    assert_eq!(tr.current().unwrap().pid, 1);
    assert!(tr.poll(t1 + TARGET_DEBOUNCE));
    assert_eq!(tr.current().unwrap().pid, 2);
    assert_eq!(tr.take_last_wait(), Some(TARGET_DEBOUNCE));
}

#[test]
fn a_new_window_of_the_same_process_needs_only_the_start_debounce() {
    let t0 = Instant::now();
    let mut tr = TargetTracker::new(TARGET_DEBOUNCE, START_DEBOUNCE);
    tr.on_foreground(Some(target(1, 1, "A")), t0);
    tr.poll(t0 + START_DEBOUNCE);
    let t1 = t0 + Duration::from_secs(5);
    tr.on_foreground(Some(target(1, 9, "A")), t1);
    assert!(tr.poll(t1 + START_DEBOUNCE));
    assert_eq!(tr.current().unwrap().hwnd, 9);
}

#[test]
fn a_waiting_game_gets_the_short_debounce_once_the_session_ends() {
    // B waits against A's live session; A exits 1 s in. B was first seen
    // more than 300 ms ago, so it commits on the next poll.
    let t0 = Instant::now();
    let mut tr = TargetTracker::new(TARGET_DEBOUNCE, START_DEBOUNCE);
    tr.on_foreground(Some(target(1, 1, "A")), t0);
    tr.poll(t0 + START_DEBOUNCE);
    let t1 = t0 + Duration::from_secs(5);
    tr.on_foreground(Some(target(2, 2, "B")), t1);
    assert!(tr.on_process_exit(1));
    assert_eq!(tr.next_deadline(), Some(t1 + START_DEBOUNCE));
    assert!(tr.poll(t1 + Duration::from_secs(1)));
    assert_eq!(tr.current().unwrap().pid, 2);
    // The same after the session's window is destroyed.
    let t2 = t1 + Duration::from_secs(10);
    tr.on_foreground(Some(target(3, 3, "C")), t2);
    assert_eq!(tr.next_deadline(), Some(t2 + TARGET_DEBOUNCE));
    assert!(tr.on_window_gone(2));
    assert!(tr.poll(t2 + START_DEBOUNCE));
    assert_eq!(tr.current().unwrap().pid, 3);
}

#[test]
fn a_short_pending_game_cannot_meet_a_session_that_starts_under_it() {
    // `current` only changes by committing `pending`, so a candidate seen
    // with no session cannot find one set before its deadline. A second
    // game replacing the first pending game restarts the clock.
    let t0 = Instant::now();
    let mut tr = TargetTracker::new(TARGET_DEBOUNCE, START_DEBOUNCE);
    tr.on_foreground(Some(target(1, 1, "A")), t0);
    tr.on_foreground(Some(target(2, 2, "B")), t0 + Duration::from_millis(200));
    assert!(!tr.poll(t0 + START_DEBOUNCE));
    assert!(tr.current().is_none());
    assert!(tr.poll(t0 + Duration::from_millis(500)));
    assert_eq!(tr.current().unwrap().pid, 2);
}

#[test]
fn brief_focus_of_another_game_does_not_retarget() {
    let t0 = Instant::now();
    let mut tr = TargetTracker::new(TARGET_DEBOUNCE, START_DEBOUNCE);
    tr.on_foreground(Some(target(1, 1, "A")), t0);
    tr.poll(t0 + TARGET_DEBOUNCE);
    // B is foreground for 1 s, then the desktop: B never commits.
    tr.on_foreground(Some(target(2, 2, "B")), t0 + Duration::from_secs(5));
    tr.on_foreground(None, t0 + Duration::from_secs(6));
    assert!(!tr.poll(t0 + Duration::from_secs(20)));
    assert_eq!(tr.current().unwrap().pid, 1);
}

#[test]
fn alt_tab_keeps_target_while_its_process_lives() {
    let t0 = Instant::now();
    let mut tr = TargetTracker::new(TARGET_DEBOUNCE, START_DEBOUNCE);
    tr.on_foreground(Some(target(1, 1, "A")), t0);
    tr.poll(t0 + TARGET_DEBOUNCE);
    tr.on_foreground(None, t0 + Duration::from_secs(10)); // desktop for a minute
    assert!(!tr.poll(t0 + Duration::from_secs(70)));
    assert_eq!(tr.current().unwrap().pid, 1, "target persists off-foreground");
    // Back to the game: no change, nothing pending.
    tr.on_foreground(Some(target(1, 1, "A")), t0 + Duration::from_secs(71));
    assert!(tr.next_deadline().is_none());
}

#[test]
fn a_settled_different_game_replaces_the_target() {
    let t0 = Instant::now();
    let mut tr = TargetTracker::new(TARGET_DEBOUNCE, START_DEBOUNCE);
    tr.on_foreground(Some(target(1, 1, "A")), t0);
    tr.poll(t0 + TARGET_DEBOUNCE);
    tr.on_foreground(Some(target(2, 2, "B")), t0 + Duration::from_secs(5));
    assert!(tr.poll(t0 + Duration::from_secs(7)));
    assert_eq!(tr.current().unwrap().pid, 2);
    // Same process, new window (fullscreen mode switch) also retargets.
    tr.on_foreground(Some(target(2, 3, "B")), t0 + Duration::from_secs(9));
    assert!(tr.poll(t0 + Duration::from_secs(11)));
    assert_eq!(tr.current().unwrap().hwnd, 3);
}

#[test]
fn process_exit_clears_target_and_pending() {
    let t0 = Instant::now();
    let mut tr = TargetTracker::new(TARGET_DEBOUNCE, START_DEBOUNCE);
    tr.on_foreground(Some(target(1, 1, "A")), t0);
    tr.poll(t0 + TARGET_DEBOUNCE);
    assert!(!tr.on_process_exit(99), "unrelated exit ignored");
    assert!(tr.on_process_exit(1));
    assert!(tr.current().is_none());
    // A candidate whose process dies before committing never commits.
    tr.on_foreground(Some(target(5, 5, "C")), t0 + Duration::from_secs(3));
    tr.on_process_exit(5);
    assert!(!tr.poll(t0 + Duration::from_secs(10)));
    assert!(tr.current().is_none());
}

#[test]
fn destroyed_target_window_ends_the_session() {
    let t0 = Instant::now();
    let mut tr = TargetTracker::new(TARGET_DEBOUNCE, START_DEBOUNCE);
    tr.on_foreground(Some(target(1, 0x10, "A")), t0);
    tr.poll(t0 + TARGET_DEBOUNCE);
    assert!(!tr.on_window_gone(0x99), "another window is irrelevant");
    assert!(tr.on_window_gone(0x10));
    assert!(tr.current().is_none());
}
