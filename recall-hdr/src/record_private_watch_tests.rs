use super::*;
use crate::config::Override;
use crate::record_composed::{notice_step, notice_text};
use crate::render_verdict::{gate, ComposedNotice, Rejected, Watch, MAX_WATCH};
use crate::types::RuleKind;
use std::time::Duration;

/// A plain fullscreen window of `stem`.
fn plain(stem: &str) -> WatchEvent {
    WatchEvent {
        pid: 60,
        exe_stem: stem.into(),
        exe_name: format!("{stem}.exe"),
        title: format!("{stem} window"),
        fullscreen: true,
        fullscreen_on_target: true,
        steam_game: false,
        hosted: false,
        hwnd: 0x600,
    }
}

fn flips(n: u32) -> RenderStats {
    RenderStats { runtime_presents: n, kernel_presents: n, independent_frames: n, ..RenderStats::default() }
}

fn composed(n: u32) -> RenderStats {
    RenderStats { runtime_presents: n, kernel_presents: n, composed_frames: n, ..RenderStats::default() }
}

/// The record path's G4 loop for one blocked window, as `record_render` runs
/// it: each recheck the probe watches for `MAX_WATCH`, the verdict is noted
/// (never committed), and the 1 s foreground re-feed runs the clock. Returns
/// the exes named.
fn run_blocked(ev: &WatchEvent, stats: RenderStats, secs: u64, notices_on: bool) -> Vec<String> {
    let cfg = Config::default();
    let t0 = Instant::now();
    let (mut rejected, mut notice, mut named) = (Rejected::default(), ComposedNotice::default(), Vec::new());
    for s in 0..=secs {
        let now = t0 + Duration::from_secs(s);
        assert!(blocked_private(ev, &cfg), "{}: only the private list blocks it", ev.exe_stem);
        let last = rejected.verdict(ev.pid, ev.hwnd);
        let front = private_front(last, false).map(|g| ((ev.pid, ev.hwnd, ev.exe_name.as_str()), g));
        named.extend(notice_step(&mut notice, front, notices_on, now));
        let watch = watch_wanted(true, notices_on, notice.named(&ev.exe_name));
        if watch && !rejected.holds(ev.pid, ev.hwnd, now) {
            let since = now - MAX_WATCH;
            let (g, v) = gate(Watch::Watching { since, stats }, now);
            let why = private_admit(g, v).expect("a finished watch decides");
            rejected.note(ev.pid, ev.hwnd, why, stats, now);
        }
    }
    named
}

#[test]
fn a_blocked_window_that_renders_like_a_game_gets_the_notice_once() {
    // A non-Steam copy of a game called Element.exe: independent flip, never recorded.
    let named = run_blocked(&plain("Element"), flips(90), 60, true);
    assert_eq!(named, vec!["Element.exe".to_string()], "named once, after 20 s");
    let (title, detail) = notice_text(&named[0]);
    assert_eq!(format!("{title}: {detail}"), "Not recording Element.exe: add it as a game in Settings");
}

#[test]
fn a_composed_blocked_window_needs_a_game_like_rate() {
    assert_eq!(run_blocked(&plain("Zoom"), composed(90), 40, true), vec!["Zoom.exe".to_string()], "steady composed");
    assert!(run_blocked(&plain("Zoom"), composed(10), 40, true).is_empty(), "an idle call");
    let video = RenderStats { video_presents: 60, ..composed(60) };
    assert!(run_blocked(&plain("Plex"), video, 40, true).is_empty(), "a film");
    assert!(run_blocked(&plain("Element"), flips(90), 40, false).is_empty(), "notices off: not used up");
}

#[test]
fn a_watched_private_window_is_never_committed() {
    let now = Instant::now();
    let since = now - MAX_WATCH;
    for stats in [flips(90), composed(90), RenderStats { legacy_flips: 9, ..RenderStats::default() }, RenderStats::default()] {
        let (g, v) = gate(Watch::Watching { since, stats }, now);
        assert!(private_admit(g, v).is_ok(), "{stats:?}: rejected, not committed");
    }
    let early = gate(Watch::Watching { since: now, stats: RenderStats::default() }, now);
    assert!(private_admit(early.0, early.1).is_err(), "unsure: ask again");
    assert_eq!(private_admit(Gate::Commit, Verdict::Unsure), Ok(Why::NoFrames), "no probe: today's rule never applies");
    assert_eq!(private_admit(Gate::Commit, Verdict::Game(Why::Tearing)), Ok(Why::Tearing));
}

#[test]
fn only_a_window_the_private_list_alone_blocks_is_watched() {
    let cfg = Config::default();
    assert!(blocked_private(&plain("Zoom"), &cfg));
    assert!(blocked_private(&plain("electron"), &cfg));
    let windowed = WatchEvent { fullscreen: false, fullscreen_on_target: false, ..plain("Zoom") };
    assert!(!blocked_private(&windowed, &cfg), "windowed");
    assert!(!blocked_private(&WatchEvent { hosted: true, ..plain("WhatsApp") }, &cfg), "a resolved Store identity");
    assert!(!blocked_private(&WatchEvent { steam_game: true, ..plain("Element") }, &cfg), "game evidence: recorded instead");
    for stem in ["chrome", "mstsc", "parsecd", "Moonlight", "vlc", "UnknownGame", "recall-native"] {
        assert!(!blocked_private(&plain(stem), &cfg), "{stem}: not a private-list block");
    }
    let mut listed = Config::default();
    listed.detect.fullscreen_games.push("Zoom".into());
    assert!(!blocked_private(&plain("Zoom"), &listed), "fullscreen_games: recorded");
    let mut none = Config::default();
    none.overrides.push(Override { exe: "slack".into(), kind: RuleKind::None, clamp: None });
    assert!(!blocked_private(&plain("slack"), &none), "a none override is the user's own no");
}

#[test]
fn the_generic_stems_stay_private() {
    for stem in ["line", "wire", "signal", "element", "messenger", "plex", "electron", "zoom"] {
        assert!(crate::private_apps::is_private(stem), "{stem}");
        assert_eq!(classify_event(&plain(stem), &Config::default()), None, "{stem}: not recorded without an opt-in");
    }
}

#[test]
fn the_watch_runs_only_when_it_can_show_something() {
    assert!(watch_wanted(true, true, false));
    assert!(!watch_wanted(false, true, false), "no trace rights");
    assert!(!watch_wanted(true, false, false), "notices off");
    assert!(!watch_wanted(true, true, true), "already named this run");
    let mut w = PrivateWatch::default();
    assert!(w.first_block("Zoom.exe"));
    assert!(!w.first_block("ZOOM.EXE"), "logged once per exe");
    w.window = Some((1, 2));
    assert!(w.is_watched(1, 2) && !w.is_watched(1, 3));
}
