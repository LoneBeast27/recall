//! Transition rules (transitions audit 2026-11-06 T2, T3, T5a, T9, T10, T11).
//! Time, window size and fullscreen state are injected; nothing touches the OS.

use std::time::{Duration, Instant};

use super::*;
use crate::record_target::{classify_event, TargetTracker, START_DEBOUNCE, TARGET_DEBOUNCE};
use crate::watcher::event_or_unknown;

fn ev(stem: &str, pid: u32, hwnd: isize, fullscreen: bool, steam: bool) -> WatchEvent {
    WatchEvent {
        pid,
        exe_stem: stem.into(),
        exe_name: format!("{stem}.exe"),
        title: stem.into(),
        fullscreen,
        fullscreen_on_target: fullscreen,
        steam_game: steam,
        hwnd,
    }
}

fn target(pid: u32, hwnd: isize, name: &str) -> Target {
    Target { hwnd, pid, name: name.into(), exe: format!("{name}.exe") }
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn tracker() -> TargetTracker {
    TargetTracker::new(TARGET_DEBOUNCE, START_DEBOUNCE)
}

/// A tracker recording `t` (committed at `t0 + START_DEBOUNCE`).
fn live(t: Target, t0: Instant) -> TargetTracker {
    let mut tr = tracker();
    tr.on_foreground(Some(t), t0);
    assert!(tr.poll(t0 + START_DEBOUNCE));
    tr
}

// --- T2: crash reporters and the drain --------------------------------------

#[test]
fn crash_reporters_are_never_targets_even_in_a_steam_library() {
    let cfg = Config::default();
    for stem in ["CrashReportClient", "UnityCrashHandler32", "UnityCrashHandler64", "crashpad_handler", "WerFault", "WerFaultSecure", "CrashReporter"] {
        assert!(classify_event(&ev(stem, 3, 0x3, false, true), &cfg).is_none(), "{stem} in a Steam library");
        assert!(classify_event(&ev(stem, 3, 0x3, true, false), &cfg).is_none(), "{stem} fullscreen");
    }
}

#[test]
fn a_windowed_steam_window_waits_for_the_drain_a_fullscreen_one_ends_it() {
    // Game A (pid 1) exited; its last clip drains. A launcher window of the
    // Steam library appears: it holds until the drain ends.
    let t0 = Instant::now();
    let mut tr = tracker();
    tr.set_draining(Some(1));
    tr.on_foreground_with(Some(target(2, 0x20, "Launcher")), Traits::WINDOWED, t0);
    assert!(!tr.poll(t0 + Duration::from_secs(20)), "no commit during the drain");
    assert_eq!(tr.next_deadline(), None, "nothing to wake for while it waits");
    assert_eq!(tr.pending().map(|t| t.pid), Some(2), "still pending");
    tr.set_draining(None);
    assert!(tr.poll(t0 + Duration::from_secs(21)), "commits once the drain ends");
    // A fullscreen game may end a drain: the 300 ms rule.
    let mut tr = tracker();
    tr.set_draining(Some(1));
    tr.on_foreground_with(Some(target(3, 0x30, "Game B")), Traits::FULLSCREEN, t0);
    assert!(tr.poll(t0 + START_DEBOUNCE));
    // So may a vouched-for windowed game (override / known game).
    let mut tr = tracker();
    tr.set_draining(Some(1));
    let vouched = Traits { vouched: true, ..Traits::WINDOWED };
    tr.on_foreground_with(Some(target(4, 0x40, "Game C")), vouched, t0);
    assert!(tr.poll(t0 + START_DEBOUNCE));
}

#[test]
fn ending_a_drain_early_names_the_game_that_started() {
    let a = target(1, 0x10, "Game A");
    let b = target(2, 0x20, "Game B");
    assert_eq!(drain_cut(Some(&a), &EngineTarget::Window(b.clone()), None).as_deref(), Some("Game B"));
    assert_eq!(drain_cut(None, &EngineTarget::Window(b.clone()), None), None, "no drain: nothing dropped");
    assert_eq!(drain_cut(Some(&a), &EngineTarget::Off, None), None, "the drain ran out");
    let remade = target(1, 0x11, "Game A");
    assert_eq!(drain_cut(Some(&a), &EngineTarget::Window(remade), None).as_deref(), Some("Game A"), "same game, new window: the ring restarts too");
    assert_eq!(drain_cut(Some(&a), &EngineTarget::Window(b.clone()), Some(&b)), None, "a pin is the player's choice");
}

// --- T3: fullscreen_games entries start, never replace -----------------------

#[test]
fn listed_discord_never_replaces_a_live_game_and_commits_once_it_exits() {
    let cfg = Config::default();
    let discord = ev("Discord", 5, 0x50, true, false);
    let traits = traits_of(&discord, &cfg);
    assert!(traits.listed_only, "Discord is only a fullscreen_games entry");
    let t0 = Instant::now();
    let mut tr = live(target(1, 0x10, "VALORANT"), t0);
    let t1 = t0 + Duration::from_secs(5);
    tr.on_foreground_with(classify_event(&discord, &cfg), traits, t1);
    assert!(!tr.poll(t1 + Duration::from_secs(10)), "10 s of Discord in front: no retarget");
    assert_eq!(tr.current().map(|t| t.pid), Some(1));
    assert!(tr.on_process_exit(1));
    assert!(tr.poll(t1 + Duration::from_secs(11)), "VALORANT exited: Discord commits");
    assert_eq!(tr.current().map(|t| t.pid), Some(5));
    // With nothing recorded it starts a session on the 300 ms rule.
    let mut tr = tracker();
    tr.on_foreground_with(classify_event(&discord, &cfg), traits, t0);
    assert!(tr.poll(t0 + START_DEBOUNCE));
}

#[test]
fn a_known_game_or_override_is_not_listed_only() {
    let cfg = Config::default();
    assert!(!traits_of(&ev("VALORANT-Win64-Shipping", 1, 1, true, false), &cfg).listed_only);
    assert!(traits_of(&ev("VALORANT-Win64-Shipping", 1, 1, true, false), &cfg).vouched);
    assert!(!traits_of(&ev("notepad", 1, 1, true, false), &cfg).listed_only);
    assert!(traits_of(&ev("sobosuba", 1, 1, false, true), &cfg).windowed_only());
}

// --- T5a: a re-made window of the session's process --------------------------

#[test]
fn fullscreen_to_windowed_recreate_keeps_recording() {
    // The game (not Steam, no override) destroys its fullscreen window and
    // makes a windowed one: not a candidate by the usual rules, adopted here.
    let cfg = Config::default();
    let t0 = Instant::now();
    let mut tr = live(target(1, 0x10, "Unity Game"), t0);
    let gone = t0 + Duration::from_secs(60);
    assert!(tr.on_window_gone_at(0x10, gone));
    assert!(tr.adopts(1, gone), "its process stays adoptable");
    tr.set_draining(Some(1));
    let windowed = ev("UnityGame", 1, 0x11, false, false);
    assert!(classify_event(&windowed, &cfg).is_none(), "today's rules drop it");
    assert!(adopted_candidate(&windowed, &cfg, (400, 200)).is_none(), "a MessageBox-sized window is not the game");
    assert!(adopted_candidate(&ev("UnityGame", 1, 0x12, true, false), &cfg, (400, 200)).is_some(), "fullscreen is never sized out");
    let adopted = adopted_candidate(&windowed, &cfg, MIN_CLIENT);
    assert!(adopted.is_some(), "adopted as the session");
    let seen = gone + ms(500);
    tr.on_foreground_with(adopted, Traits::ADOPTED, seen);
    assert!(tr.poll(seen + START_DEBOUNCE), "the drain does not hold its own game");
    assert_eq!(tr.current().map(|t| t.hwnd), Some(0x11));
    assert!(!tr.adopts(1, seen + START_DEBOUNCE), "adopted once");
}

#[test]
fn adoption_lapses_and_ends_with_the_process() {
    let cfg = Config::default();
    let t0 = Instant::now();
    let mut tr = live(target(1, 0x10, "Game"), t0);
    assert!(tr.on_window_gone_at(0x10, t0 + START_DEBOUNCE));
    assert!(tr.adopts(1, t0 + START_DEBOUNCE + ADOPT_FOR - ms(1)));
    assert!(!tr.adopts(1, t0 + START_DEBOUNCE + ADOPT_FOR), "lapses after ADOPT_FOR");
    assert!(!tr.adopts(2, t0 + ms(400)), "only the session's process");
    let mut tr = live(target(1, 0x10, "Game"), t0);
    assert!(tr.on_window_gone_at(0x10, t0 + START_DEBOUNCE));
    tr.on_process_exit(1);
    assert!(!tr.adopts(1, t0 + ms(400)), "a dead process adopts nothing");
    assert!(adopted_candidate(&ev("explorer", 1, 0x12, false, false), &cfg, MIN_CLIENT).is_none(), "the deny lists still win");
}

// --- T9: retarget storm ------------------------------------------------------

#[test]
fn a_retarget_between_live_games_blocks_another_for_ten_seconds() {
    let t0 = Instant::now();
    let mut tr = live(target(1, 0x10, "A"), t0);
    let t1 = t0 + Duration::from_secs(5);
    tr.on_foreground(Some(target(2, 0x20, "B")), t1);
    assert!(tr.poll(t1 + TARGET_DEBOUNCE), "A -> B as today");
    let swapped = t1 + TARGET_DEBOUNCE;
    // Back to A 0.5 s later and held for 2.5 s: no second retarget yet.
    let back = swapped + ms(500);
    tr.on_foreground(Some(target(1, 0x10, "A")), back);
    assert!(!tr.poll(back + ms(2500)), "within the cool-down");
    assert_eq!(tr.current().map(|t| t.pid), Some(2));
    assert_eq!(tr.next_deadline(), Some(swapped + REPLACE_COOLDOWN));
    assert!(tr.poll(swapped + REPLACE_COOLDOWN), "after 10 s it may replace B");
    assert_eq!(tr.current().map(|t| t.pid), Some(1));
}

#[test]
fn the_cool_down_does_not_hold_a_game_once_the_session_exits() {
    let t0 = Instant::now();
    let mut tr = live(target(1, 0x10, "A"), t0);
    let t1 = t0 + Duration::from_secs(5);
    tr.on_foreground(Some(target(2, 0x20, "B")), t1);
    assert!(tr.poll(t1 + TARGET_DEBOUNCE));
    let t2 = t1 + TARGET_DEBOUNCE + ms(100);
    tr.on_foreground(Some(target(3, 0x30, "C")), t2);
    assert!(tr.on_process_exit(2));
    assert!(tr.poll(t2 + START_DEBOUNCE), "B exited: C starts on the 300 ms rule");
}

// --- T10: a Steam game's splash / launcher window ----------------------------

#[test]
fn a_windowed_steam_window_needs_1_5_s_to_start_a_session() {
    let t0 = Instant::now();
    let mut tr = tracker();
    tr.on_foreground_with(Some(target(7, 0x70, "Splash")), Traits::WINDOWED, t0);
    assert_eq!(tr.next_deadline(), Some(t0 + WINDOWED_START));
    assert!(!tr.poll(t0 + START_DEBOUNCE), "not on the 300 ms rule");
    // The main window (same pid, new hwnd) replaces the splash: one start.
    let main = t0 + ms(800);
    tr.on_foreground_with(Some(target(7, 0x71, "Game")), Traits::WINDOWED, main);
    assert!(!tr.poll(main + ms(1499)));
    assert!(tr.poll(main + WINDOWED_START));
    assert_eq!(tr.current().map(|t| t.hwnd), Some(0x71));
    // Fullscreen keeps 300 ms.
    let mut tr = tracker();
    tr.on_foreground_with(Some(target(8, 0x80, "FS")), Traits::FULLSCREEN, t0);
    assert!(tr.poll(t0 + START_DEBOUNCE));
}

#[test]
fn a_tiny_windowed_steam_window_is_not_a_candidate() {
    assert!(too_small(Traits::WINDOWED, (480, 270)), "a splash");
    assert!(too_small(Traits::WINDOWED, (1280, 300)));
    assert!(!too_small(Traits::WINDOWED, MIN_CLIENT));
    assert!(!too_small(Traits::FULLSCREEN, (320, 200)), "fullscreen is never sized out");
    assert!(!too_small(Traits::ADOPTED, (320, 200)), "an adopted window is the game");
    // R2-4: a known game's windowed box needs the floor, an override does not.
    let known = Traits { vouched: true, known_only: true, ..Traits::WINDOWED };
    assert!(too_small(known, (400, 150)), "the game's error box");
    assert!(!too_small(known, (854, 480)), "Minecraft Java's default window");
    assert!(!too_small(Traits { fullscreen: true, ..known }, (400, 150)));
    assert!(!too_small(Traits { vouched: true, ..Traits::WINDOWED }, (400, 150)), "an override is the user's choice");
}

// --- T11: an unidentifiable foreground ---------------------------------------

#[test]
fn an_unidentifiable_foreground_is_fed_as_not_a_target() {
    let cfg = Config::default();
    let unknown = event_or_unknown(0x99, 4242, None).expect("a real window of another process");
    assert!(unknown.is_unknown());
    assert_eq!(classify_event(&unknown, &cfg), None);
    // It cancels a pending game.
    let t0 = Instant::now();
    let mut tr = tracker();
    tr.on_foreground(Some(target(2, 0x20, "B")), t0);
    tr.on_foreground(classify_event(&unknown, &cfg), t0 + ms(100));
    assert!(!tr.poll(t0 + Duration::from_secs(5)));
    assert!(tr.current().is_none());
    // No window, no owner, or our own process: still nothing.
    assert!(event_or_unknown(0, 4242, None).is_none());
    assert!(event_or_unknown(0x99, 0, None).is_none());
    assert!(event_or_unknown(0x99, std::process::id(), None).is_none());
    let known = ev("game", 3, 0x3, true, false);
    assert_eq!(event_or_unknown(0x3, 3, Some(known.clone())), Some(known));
}

// --- G5: a Steam-distributed tool must not steal a fullscreen session -------

#[test]
fn windowed_steam_window_never_replaces_a_live_fullscreen_session() {
    // Fullscreen game (pid 1) is recorded; Blender from a Steam library (pid 2,
    // windowed) holds the foreground for 5 s.
    let t0 = Instant::now();
    let mut tr = live(target(1, 0x10, "Game"), t0);
    let t1 = t0 + Duration::from_secs(1);
    tr.on_foreground_with(Some(target(2, 0x20, "Blender")), Traits::WINDOWED, t1);
    assert!(!tr.poll(t1 + Duration::from_secs(5)), "a windowed Steam tool never replaces it");
    assert_eq!(tr.next_deadline(), None, "nothing to wake for while the session lives");
    assert_eq!(tr.current().map(|t| t.pid), Some(1));
    // Once the game exits it may start a session (short debounce from first seen).
    assert!(tr.on_process_exit(1));
    assert!(tr.poll(t1 + Duration::from_secs(6)), "commits once the session ended");
    assert_eq!(tr.current().map(|t| t.pid), Some(2));
}

#[test]
fn fullscreen_and_vouched_windows_still_replace_a_fullscreen_session() {
    let t0 = Instant::now();
    let t1 = t0 + Duration::from_secs(1);
    let mut tr = live(target(1, 0x10, "Game"), t0);
    tr.on_foreground_with(Some(target(3, 0x30, "Other")), Traits::FULLSCREEN, t1);
    assert!(tr.poll(t1 + TARGET_DEBOUNCE), "fullscreen: the 2 s rule");
    let mut tr = live(target(1, 0x10, "Game"), t0);
    let vouched = Traits { vouched: true, ..Traits::WINDOWED };
    tr.on_foreground_with(Some(target(4, 0x40, "Known")), vouched, t1);
    assert!(tr.poll(t1 + TARGET_DEBOUNCE), "an override / known game: the 2 s rule");
}

#[test]
fn a_windowed_session_can_still_be_replaced_by_a_windowed_steam_game() {
    // The rule is about fullscreen sessions only: two windowed Steam games swap
    // after the 2 s debounce as before.
    let t0 = Instant::now();
    let mut tr = tracker();
    tr.on_foreground_with(Some(target(1, 0x10, "Windowed A")), Traits::WINDOWED, t0);
    assert!(tr.poll(t0 + WINDOWED_START));
    let t1 = t0 + Duration::from_secs(3);
    tr.on_foreground_with(Some(target(2, 0x20, "Windowed B")), Traits::WINDOWED, t1);
    assert!(tr.poll(t1 + TARGET_DEBOUNCE));
    assert_eq!(tr.current().map(|t| t.pid), Some(2));
    // A session that went fullscreen on re-focus is protected from then on.
    let mut tr = tracker();
    tr.on_foreground_with(Some(target(1, 0x10, "A")), Traits::WINDOWED, t0);
    assert!(tr.poll(t0 + WINDOWED_START));
    tr.on_foreground_with(Some(target(1, 0x10, "A")), Traits::FULLSCREEN, t1);
    tr.on_foreground_with(Some(target(2, 0x20, "B")), Traits::WINDOWED, t1);
    assert!(!tr.poll(t1 + Duration::from_secs(5)), "the session is fullscreen now");
}
