use std::time::{Duration, Instant};

use super::*;
use crate::config::Config;
use crate::record_target::{classify_event, is_target, Target, TargetTracker, START_DEBOUNCE, TARGET_DEBOUNCE};
use crate::watcher::WatchEvent;

const GAME_HWND: isize = 0x10;
const GAME_PID: u32 = 10;
const GHOST_HWND: isize = 0x99;
const DWM_PID: u32 = 500;

fn class_of(hwnd: isize) -> Option<String> {
    match hwnd {
        GHOST_HWND => Some(GHOST_CLASS.into()),
        GAME_HWND => Some("UnrealWindow".into()),
        _ => None,
    }
}

/// The event `watcher::build_event` makes for the window `resolve_with` picks:
/// the game's own window, or the ghost (owned by `dwm.exe`, fullscreen).
fn event_for(hwnd: isize, hung_of: impl Fn(isize) -> isize) -> WatchEvent {
    let (pid, stem) = match resolve_with(hwnd, class_of, owner_of, hung_of) {
        GAME_HWND => (GAME_PID, "VALORANT-Win64-Shipping"),
        _ => (DWM_PID, "dwm"),
    };
    let hwnd = if pid == GAME_PID { GAME_HWND } else { hwnd };
    WatchEvent {
        pid,
        exe_stem: stem.into(),
        exe_name: format!("{stem}.exe"),
        title: if pid == GAME_PID { "VALORANT".into() } else { "VALORANT (Not Responding)".into() },
        fullscreen: true,
        fullscreen_on_target: true,
        steam_game: false,
        launcher_game: false,
        store_game: false,
        hosted: false,
        window_class: String::new(),
        minecraft_java: false,
        hwnd,
    }
}

/// Owner of each window: the ghost is `dwm.exe`'s, the game its own.
fn owner_of(hwnd: isize) -> Option<String> {
    match hwnd {
        GHOST_HWND => Some(GHOST_OWNER_STEM.into()),
        GAME_HWND => Some("VALORANT-Win64-Shipping".into()),
        _ => None,
    }
}

fn hung_is_game(ghost: isize) -> isize {
    if ghost == GHOST_HWND { GAME_HWND } else { 0 }
}

#[test]
fn a_ghost_maps_to_its_hung_window() {
    assert_eq!(resolve_with(GHOST_HWND, class_of, owner_of, hung_is_game), GAME_HWND);
}

#[test]
fn a_normal_window_and_an_unresolved_ghost_stay_themselves() {
    assert_eq!(resolve_with(GAME_HWND, class_of, owner_of, |_| 0x77), GAME_HWND, "not a ghost: no lookup");
    assert_eq!(resolve_with(GHOST_HWND, class_of, owner_of, |_| 0), GHOST_HWND);
    assert_eq!(resolve_with(0x55, |_| None, owner_of, |_| 0x77), 0x55, "class unknown");
}

#[test]
fn a_ghost_class_window_of_another_process_stays_itself() {
    // An app may name its own window class "Ghost": only dwm.exe's ghosts map.
    let app = |_: isize| Some("SomeApp".to_string());
    assert_eq!(resolve_with(GHOST_HWND, class_of, app, hung_is_game), GHOST_HWND, "owner not dwm");
    assert_eq!(resolve_with(GHOST_HWND, class_of, |_| None, hung_is_game), GHOST_HWND, "owner unknown");
    let dwm_upper = |_: isize| Some("DWM".to_string());
    assert_eq!(resolve_with(GHOST_HWND, class_of, dwm_upper, hung_is_game), GAME_HWND, "stem case");
}

#[test]
fn dwm_is_never_a_target_even_fullscreen() {
    let cfg = Config::default();
    assert!(!is_target("dwm", true, &cfg));
    assert!(!is_target("dwm.exe", true, &cfg));
}

/// A session on the game, then its ghost holds the foreground for 10 s (the
/// watcher's hook + 1 s probe). Expected: no retarget, same window and pid.
fn ghost_for_ten_seconds_keeps_the_session(hung_of: impl Fn(isize) -> isize + Copy) {
    let cfg = Config::default();
    let t0 = Instant::now();
    let mut tr = TargetTracker::new(TARGET_DEBOUNCE, START_DEBOUNCE);
    tr.on_foreground(classify_event(&event_for(GAME_HWND, hung_of), &cfg), t0);
    assert!(tr.poll(t0 + START_DEBOUNCE));
    let start = t0 + Duration::from_secs(1);
    for step in 0..=100u64 {
        let now = start + Duration::from_millis(step * 100);
        tr.on_foreground(classify_event(&event_for(GHOST_HWND, hung_of), &cfg), now);
        assert!(!tr.poll(now), "retargeted at {} ms of the ghost", step * 100);
    }
    let cur: &Target = tr.current().expect("session kept");
    assert_eq!((cur.pid, cur.hwnd), (GAME_PID, GAME_HWND));
}

#[test]
fn a_ghost_mapped_to_the_game_keeps_the_session_for_ten_seconds() {
    ghost_for_ten_seconds_keeps_the_session(hung_is_game);
}

#[test]
fn an_unmapped_dwm_ghost_keeps_the_session_for_ten_seconds() {
    ghost_for_ten_seconds_keeps_the_session(|_| 0);
}
