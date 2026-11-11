use super::*;
use crate::config::{Config, Override};
#[allow(unused_imports)]
use crate::record_target_rules::*;
use crate::types::RuleKind;
use crate::rules;

fn ev(stem: &str, title: &str, pid: u32, hwnd: isize, fullscreen: bool) -> WatchEvent {
    WatchEvent {
        pid,
        exe_stem: stem.into(),
        exe_name: format!("{stem}.exe"),
        title: title.into(),
        fullscreen,
        fullscreen_on_target: fullscreen,
        steam_game: false,
        launcher_game: false,
        store_game: false,
        hosted: false,
        window_class: String::new(),
        minecraft_java: false,
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

const REMOTE_DESKTOP: &[&str] = &[
    "mstsc", "msrdc", "msrdcw", "rdclient", "rdclient.windows", "windows365", "windowsapp",
    "rdcman", "vmconnect", "vmware-remotemks", "virtualboxvm", "mremoteng", "royalts",
    "remotedesktopmanager", "remotedesktopmanager64", "anydesk", "teamviewer", "rustdesk",
    "parsecd", "moonlight", "remoting_desktop", "remoting_host", "vncviewer", "tvnviewer",
    "nomachine", "nxplayer", "splashtop", "strwinclt",
    // R7-detect-3: Citrix, VMware Horizon, Amazon WorkSpaces, Shadow PC.
    "wfica32", "cdviewer", "vmware-view", "workspaces", "shadowpc", "shadow pc",
];
const NEW_LAUNCHERS: &[&str] = &[
    "upc", "uplay", "ubisoftconnect", "origin", "xboxpcapp", "xboxapp", "gamingservicesui",
    "amazongames", "amazon games ui", "playnite.fullscreenapp", "playnite.desktopapp",
];

/// R7-detect-3: the Shadow PC client is `Shadow.exe`; a game may share that
/// bare name, so Steam evidence or `known_games` still makes it one.
#[test]
fn the_shadow_client_is_remote_unless_steam_says_it_is_a_game() {
    let cfg = user_cfg();
    assert!(!is_target("Shadow.exe", true, &cfg), "fullscreen Shadow client");
    assert!(classify_event(&ev("Shadow", "Shadow", 3, 0x3, true), &cfg).is_none());
    assert!(classify_event(&steam_ev("Shadow"), &cfg).is_some(), "a Steam game called Shadow.exe");
}

#[test]
fn remote_desktop_and_launchers_are_never_targets_even_fullscreen() {
    let cfg = user_cfg();
    for &stem in REMOTE_DESKTOP.iter().chain(NEW_LAUNCHERS) {
        let upper = stem.to_ascii_uppercase();
        for s in [stem.to_string(), upper.clone(), format!("{stem}.exe"), format!("{upper}.EXE")] {
            assert!(!is_target(&s, true, &cfg), "{s} fullscreen must not be recorded");
            assert!(classify_event(&ev(&s, "Remote Desktop", 3, 0x3, true), &cfg).is_none(), "{s}");
            assert!(classify_event(&steam_ev(&s), &cfg).is_none(), "{s} in a Steam library");
        }
    }
}

#[test]
fn the_logged_false_positive_is_denied() {
    // 2026-11-04: record: target -> "Remote Desktop" (msrdc.exe, pid 484, ...)
    let cfg = user_cfg();
    assert!(classify_event(&ev("msrdc", "Remote Desktop", 484, 0xdec027e, true), &cfg).is_none());
}

#[test]
fn steam_remote_play_and_exe_suffixed_games_stay_targets() {
    let cfg = user_cfg();
    assert!(is_target("streaming_client", true, &cfg), "Steam Remote Play is recorded on purpose");
    assert!(is_target("VALORANT-Win64-Shipping.exe", true, &cfg));
    assert!(is_target("exe", true, &cfg), "a stem that is only 'exe' is untouched");
    assert!(!is_target(".exe", true, &cfg), "empty after the suffix");
}

#[test]
fn a_settings_entry_beats_the_deny_lists_but_never_windows_or_recall() {
    // Final review F-detect-2 (the maintainer 2026-11-07, "support for manual application registering"):
    // the user's own entry beats the remote-client, browser, media-player, launcher, Store-app
    // and private lists. It never makes Recall itself or a part of Windows a target.
    let mut cfg = user_cfg();
    for exe in ["msrdc", "parsecd", "chrome", "vlc", "steam", "microsoft.media.player", "zoom", "teams"] {
        assert!(!is_target(exe, true, &cfg), "{exe}: denied without an entry");
        cfg.overrides.push(Override { exe: exe.into(), kind: RuleKind::SdrGame, clamp: None });
        assert!(is_target(exe, false, &cfg), "{exe}: the entry records it, windowed too");
    }
    for exe in ["recall-native", "Recall.exe", "dwm", "explorer.exe", "LockApp", "logonui", "svchost", "shellhost"] {
        cfg.overrides.push(Override { exe: exe.into(), kind: RuleKind::HdrGame, clamp: None });
        assert!(!is_target(exe, true, &cfg), "{exe}: never a target, entry or not");
    }
    cfg.detect.fullscreen_games.push("explorer".into());
    assert!(!is_target("explorer", true, &cfg), "not through fullscreen_games either");
    let scr = WatchEvent { exe_stem: "mystify".into(), exe_name: "Mystify.scr".into(), fullscreen: true, fullscreen_on_target: true, ..WatchEvent::unknown(7, 0x7) };
    cfg.overrides.push(Override { exe: "mystify".into(), kind: RuleKind::SdrGame, clamp: None });
    assert!(classify_event(&scr, &cfg).is_none(), "a screensaver never is");
}

#[test]
fn fullscreen_game_after_remote_desktop_commits_after_start_debounce() {
    let cfg = user_cfg();
    let t0 = Instant::now();
    let mut tr = TargetTracker::new(TARGET_DEBOUNCE, START_DEBOUNCE);
    tr.on_foreground(classify_event(&ev("msrdc", "Remote Desktop", 484, 0x1, true), &cfg), t0);
    assert!(!tr.poll(t0 + Duration::from_secs(10)), "Remote Desktop never commits");
    assert!(tr.current().is_none());
    let t1 = t0 + Duration::from_secs(10);
    let game = classify_event(&ev("cs2", "Counter-Strike 2", 9, 0x9, true), &cfg);
    tr.on_foreground(game, t1);
    assert_eq!(START_DEBOUNCE, Duration::from_millis(300));
    assert!(!tr.poll(t1 + Duration::from_millis(299)));
    assert!(tr.poll(t1 + START_DEBOUNCE), "first start stays 300 ms");
    assert_eq!(tr.current().unwrap().pid, 9);
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

#[test]
fn a_held_candidate_waits_for_the_render_check_then_commits() {
    let t0 = Instant::now();
    let mut tr = TargetTracker::new(TARGET_DEBOUNCE, START_DEBOUNCE);
    tr.on_foreground(Some(target(5, 0x50, "Netflix")), t0);
    let due = t0 + START_DEBOUNCE;
    let ask = due + Duration::from_millis(100);
    assert!(!tr.poll_admit(due, |_| Admit::Hold(ask)), "held: nothing commits");
    assert_eq!(tr.pending().map(|t| t.pid), Some(5), "still pending");
    assert_eq!(tr.next_deadline(), Some(ask), "the pump wakes for the next ask");
    assert!(!tr.poll_admit(ask - Duration::from_millis(1), |_| panic!("not due")));
    assert!(tr.poll_admit(ask, |_| Admit::Commit));
    assert_eq!(tr.current().map(|t| t.pid), Some(5));
    assert_eq!(tr.take_last_wait(), Some(ask - t0), "the wait counts from first seen");
}

#[test]
fn a_rejected_candidate_is_dropped_and_the_session_stays() {
    let t0 = Instant::now();
    let mut tr = TargetTracker::new(TARGET_DEBOUNCE, START_DEBOUNCE);
    tr.on_foreground(Some(target(1, 0x10, "Game")), t0);
    assert!(tr.poll(t0 + START_DEBOUNCE));
    let t1 = t0 + Duration::from_secs(5);
    tr.on_foreground(Some(target(2, 0x20, "Video")), t1);
    assert!(!tr.poll_admit(t1 + TARGET_DEBOUNCE, |_| Admit::Reject));
    assert_eq!(tr.current().map(|t| t.pid), Some(1), "the game's session stays");
    assert!(tr.pending().is_none());
    assert_eq!(tr.next_deadline(), None);
}

#[test]
fn a_new_candidate_clears_an_old_hold() {
    let t0 = Instant::now();
    let mut tr = TargetTracker::new(TARGET_DEBOUNCE, START_DEBOUNCE);
    tr.on_foreground(Some(target(5, 0x50, "Slow")), t0);
    let far = t0 + Duration::from_secs(1);
    assert!(!tr.poll_admit(t0 + START_DEBOUNCE, |_| Admit::Hold(far)));
    let t1 = t0 + Duration::from_millis(400);
    tr.on_foreground(Some(target(6, 0x60, "Other")), t1);
    assert_eq!(tr.next_deadline(), Some(t1 + START_DEBOUNCE), "the old hold does not delay a new window");
    // Seeing the held candidate again keeps its hold.
    let mut tr = TargetTracker::new(TARGET_DEBOUNCE, START_DEBOUNCE);
    tr.on_foreground(Some(target(5, 0x50, "Slow")), t0);
    assert!(!tr.poll_admit(t0 + START_DEBOUNCE, |_| Admit::Hold(far)));
    tr.on_foreground(Some(target(5, 0x50, "Slow")), t1);
    assert_eq!(tr.next_deadline(), Some(far));
}

#[test]
fn the_process_exiting_drops_a_held_candidate() {
    let t0 = Instant::now();
    let mut tr = TargetTracker::new(TARGET_DEBOUNCE, START_DEBOUNCE);
    tr.on_foreground(Some(target(5, 0x50, "Slow")), t0);
    assert!(!tr.poll_admit(t0 + START_DEBOUNCE, |_| Admit::Hold(t0 + Duration::from_secs(1))));
    assert!(!tr.on_process_exit(5));
    assert_eq!(tr.next_deadline(), None);
}

#[test]
fn discord_fullscreen_is_a_game_by_default_windowed_is_not() {
    let cfg = Config::default();
    assert!(rules::is_denied("discord"), "the HDR deny list still names it");
    for stem in ["Discord", "discord", "Discord.exe"] {
        assert!(is_target(stem, true, &cfg), "{stem} fullscreen is recorded (detect.fullscreen_games)");
        assert!(!is_target(stem, false, &cfg), "{stem} windowed is not a target");
        assert!(is_fullscreen_game(&cfg, stem), "{stem} skips the render check");
    }
    assert!(classify_event(&ev("discord", "Discord", 5, 0x5, true), &cfg).is_some());
    assert!(classify_event(&ev("discord", "Discord", 5, 0x5, false), &cfg).is_none());
    assert!(!is_fullscreen_game(&cfg, "chrome"));
    assert!(!is_fullscreen_game(&cfg, "  "));
}

#[test]
fn fullscreen_games_yield_to_none_overrides_and_recall_itself() {
    let mut cfg = Config::default();
    cfg.detect.fullscreen_games = vec!["recall-native".into(), "Recall.exe".into(), "chrome".into(), "discord".into()];
    assert!(!is_target("recall-native", true, &cfg), "Recall's own windows are never recorded");
    assert!(!is_target("recall", true, &cfg));
    assert!(is_target("chrome", true, &cfg), "a listed browser beats the deny list (user's call)");
    assert!(!is_target("chrome", false, &cfg));
    cfg.overrides.push(Override { exe: "discord".into(), kind: RuleKind::None, clamp: None });
    assert!(!is_target("discord", true, &cfg), "a none override still wins");
    cfg.detect.fullscreen_games.clear();
    cfg.overrides.clear();
    assert!(!is_target("discord", true, &cfg), "an empty list restores the deny list");
}

#[test]
fn valorant_is_never_left_to_the_render_check() {
    // The maintainer's main game: even vsynced or DWM-composed (which the render check
    // would reject), its fullscreen window commits on the 300 ms rule.
    let cfg = Config::default();
    let v = ev("VALORANT-Win64-Shipping", "VALORANT  ", 10, 0x10, true);
    assert!(classify_event(&v, &cfg).is_some());
    assert!(skips_render_check(&v, &cfg), "VALORANT skips the render check");
    let composed = crate::render_verdict::RenderStats { runtime_presents: 90, kernel_presents: 90, composed_frames: 90, ..Default::default() };
    assert!(matches!(crate::render_verdict::judge(&composed, true), crate::render_verdict::Verdict::NotGame(_)), "what the check would say");
    let windowed = ev("VALORANT-Win64-Shipping", "VALORANT", 10, 0x10, false);
    assert!(classify_event(&windowed, &cfg).is_some(), "windowed: a known game is a candidate too (G10)");
    let mut off = cfg.clone();
    off.overrides.push(Override { exe: "valorant-win64-shipping".into(), kind: RuleKind::None, clamp: None });
    assert!(classify_event(&v, &off).is_none(), "a none override still wins");
}

#[test]
fn known_games_skip_the_check_only_as_fullscreen_candidates() {
    let cfg = Config::default();
    for (stem, title) in [("FortniteClient-Win64-Shipping", "Fortnite"), ("Overwatch", "Overwatch"), ("League of Legends", "League of Legends (TM) Client"), ("r5apex_dx12", "Apex Legends"), ("javaw", "Minecraft 1.21.1")] {
        let e = ev(stem, title, 3, 0x3, true);
        assert!(classify_event(&e, &cfg).is_some() && skips_render_check(&e, &cfg), "{stem}");
        // G10: a known game is a candidate windowed too (`known_game_windowed_is_a_candidate`).
        assert!(classify_event(&ev(stem, title, 3, 0x3, false), &cfg).is_some(), "{stem} windowed");
    }
    let ide = ev("javaw", "IntelliJ IDEA", 4, 0x4, true);
    assert!(classify_event(&ide, &cfg).is_some(), "an unknown fullscreen Java app is a candidate");
    assert!(!skips_render_check(&ide, &cfg), "but it must render like a game");
    assert!(!skips_render_check(&ev("notepad", "x", 1, 1, true), &cfg));
}

#[path = "record_target_class_tests.rs"]
mod class;
