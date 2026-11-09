use super::*;

fn stats() -> RenderStats {
    RenderStats::default()
}

#[test]
fn exclusive_fullscreen_is_a_game() {
    let s = RenderStats { legacy_flips: 3, kernel_presents: 3, ..stats() };
    assert_eq!(judge(&s, false), Verdict::Game(Why::ExclusiveFullscreen));
    let s = RenderStats { legacy_flips: 2, ..s };
    assert_eq!(judge(&s, false), Verdict::Unsure, "two flips are not yet a signal");
}

#[test]
fn a_valorant_like_window_commits_within_a_few_frames() {
    // VALORANT on 2026-11-01: independent flip, every present allows tearing.
    let s = RenderStats { runtime_presents: 4, tearing_presents: 4, independent_frames: 3, composed_frames: 1, ..stats() };
    assert_eq!(judge(&s, false), Verdict::Game(Why::IndependentFlip));
    // The first frames after going fullscreen are often composed: wait, then commit.
    let s = RenderStats { independent_frames: 3, composed_frames: 5, ..stats() };
    assert_eq!(judge(&s, false), Verdict::Unsure);
    let s = RenderStats { independent_frames: 6, composed_frames: 5, ..stats() };
    assert_eq!(judge(&s, false), Verdict::Game(Why::IndependentFlip));
}

#[test]
fn tearing_presents_alone_count_as_a_game() {
    // E.g. a game DWM still composes because something sits above it.
    let s = RenderStats { runtime_presents: 10, tearing_presents: 6, composed_frames: 10, ..stats() };
    assert_eq!(judge(&s, false), Verdict::Game(Why::Tearing));
    let s = RenderStats { runtime_presents: 10, tearing_presents: 4, composed_frames: 10, ..stats() };
    assert_eq!(judge(&s, true), Verdict::NotGame(Why::Composed), "a minority of tearing presents is not enough");
}

#[test]
fn composed_only_is_not_a_game_once_the_watch_ends() {
    // A 60 fps video or a busy chat window: same rate as a capped game, but composed.
    let s = RenderStats { runtime_presents: 90, kernel_presents: 90, composed_frames: 90, ..stats() };
    assert_eq!(judge(&s, false), Verdict::Unsure, "decided only at the end of the watch");
    assert_eq!(judge(&s, true), Verdict::NotGame(Why::Composed));
}

#[test]
fn a_decode_swap_chain_is_video_even_with_independent_flip() {
    let s = RenderStats { runtime_presents: 30, video_presents: 30, independent_frames: 30, ..stats() };
    assert_eq!(judge(&s, false), Verdict::NotGame(Why::VideoSwapChain));
    // A game that also shows a small video: mostly normal presents, still a game.
    let s = RenderStats { runtime_presents: 100, video_presents: 3, independent_frames: 90, ..stats() };
    assert_eq!(judge(&s, false), Verdict::Game(Why::IndependentFlip));
}

#[test]
fn nothing_presented_is_not_a_game() {
    assert_eq!(judge(&stats(), false), Verdict::Unsure);
    assert_eq!(judge(&stats(), true), Verdict::NotGame(Why::NoFrames), "a still slide or a paused video");
}

#[test]
fn the_gate_commits_holds_and_rejects() {
    let t0 = Instant::now();
    assert_eq!(gate(Watch::Unavailable, t0).0, Gate::Reject(Why::NoFrames), "no trace rights: no verdict, not a game (official-proof ruling)");
    let game = RenderStats { independent_frames: 5, ..stats() };
    assert_eq!(gate(Watch::Watching { since: t0, stats: game }, t0).0, Gate::Commit);
    let unsure = Watch::Watching { since: t0, stats: stats() };
    let at = t0 + Duration::from_millis(300);
    assert_eq!(gate(unsure, at).0, Gate::Hold(at + ASK_EVERY));
    let near_end = t0 + MAX_WATCH - Duration::from_millis(30);
    assert_eq!(gate(unsure, near_end).0, Gate::Hold(t0 + MAX_WATCH), "never holds past the end");
    assert_eq!(gate(unsure, t0 + MAX_WATCH).0, Gate::Reject(Why::NoFrames));
}

#[test]
fn a_rejected_window_waits_longer_each_time() {
    let t0 = Instant::now();
    let mut r = Rejected::default();
    r.note(7, 70, Why::Composed, stats(), t0);
    assert_eq!(RECHECK_FIRST, Duration::from_secs(2), "a match start is caught within seconds");
    assert!(r.holds(7, 70, t0 + RECHECK_FIRST - Duration::from_millis(1)));
    assert!(!r.holds(7, 70, t0 + RECHECK_FIRST));
    assert!(!r.holds(7, 71, t0), "another window of the process is new");
    let mut waits = vec![RECHECK_FIRST];
    for _ in 0..5 {
        r.note(7, 70, Why::Composed, stats(), t0);
        waits.push(r.until().unwrap() - t0);
    }
    let secs: Vec<u64> = waits.iter().map(Duration::as_secs).collect();
    assert_eq!(secs, [2, 4, 8, 16, 30, 30], "doubling, capped at 30 s");
    r.note(8, 80, Why::VideoSwapChain, stats(), t0);
    assert_eq!(r.until(), Some(t0 + RECHECK_FIRST), "another window starts over");
}

#[test]
fn no_frames_is_rechecked_soon_then_backs_off() {
    let t0 = Instant::now();
    let mut r = Rejected::default();
    let mut holds = Vec::new();
    for _ in 0..15 {
        r.note(7, 70, Why::NoFrames, stats(), t0);
        holds.push((r.until().unwrap() - t0).as_secs());
    }
    assert_eq!(holds[..NO_FRAMES_FAST as usize], [2; 10], "about 20 s of fast rechecks for a loading screen");
    assert_eq!(holds[10..], [4, 8, 16, 30, 30], "then the backoff, capped at 30 s");
}

#[test]
fn no_frames_backoff_resets_on_frames_or_a_new_window() {
    let t0 = Instant::now();
    let quiet = |r: &mut Rejected, pid, hwnd| {
        for _ in 0..12 {
            r.note(pid, hwnd, Why::NoFrames, stats(), t0);
        }
    };
    let mut r = Rejected::default();
    quiet(&mut r, 7, 70);
    assert_eq!(r.until(), Some(t0 + Duration::from_secs(8)));
    r.note(7, 70, Why::Composed, stats(), t0);
    assert_eq!(r.until(), Some(t0 + RECHECK_FIRST), "it started presenting: back to 2 s");
    r.note(7, 70, Why::NoFrames, stats(), t0);
    assert_eq!(r.until(), Some(t0 + RECHECK_NO_FRAMES), "and the fast no-frames rounds again");
    quiet(&mut r, 7, 70);
    r.note(7, 71, Why::NoFrames, stats(), t0);
    assert_eq!(r.until(), Some(t0 + RECHECK_NO_FRAMES), "a new window starts at 2 s");
    quiet(&mut r, 7, 71);
    r.note(9, 71, Why::NoFrames, stats(), t0);
    assert_eq!(r.until(), Some(t0 + RECHECK_NO_FRAMES), "a new process too");
}

// --- G4: a composed fullscreen window the player is looking at -------------

#[test]
fn rejected_remembers_a_composed_verdict_per_window() {
    let t0 = Instant::now();
    let mut r = Rejected::default();
    assert!(r.composed(7, 70).is_none());
    r.note(7, 70, Why::Composed, stats(), t0);
    assert!(r.composed(7, 70).is_some());
    assert!(r.composed(7, 70).is_some(), "after its wait too, until the next verdict");
    assert!(r.composed(7, 71).is_none(), "another window");
    r.note(7, 70, Why::NoFrames, stats(), t0);
    assert!(r.composed(7, 70).is_none(), "no frames is not composed");
    r.note(7, 70, Why::VideoSwapChain, stats(), t0);
    assert!(r.composed(7, 70).is_none(), "a video is not");
}

#[test]
fn composed_notice_fires_once_per_exe_after_20_s_in_front() {
    let t0 = Instant::now();
    let s = |n: u64| t0 + Duration::from_secs(n);
    let mut n = ComposedNotice::default();
    let game = Some((7, 70, "Game.exe"));
    assert_eq!(n.observe(game, t0), None);
    assert_eq!(n.observe(game, s(20)), None, "20 s is not more than 20 s");
    assert_eq!(n.observe(game, s(21)).as_deref(), Some("Game.exe"));
    assert_eq!(n.observe(game, s(60)), None, "once per exe");
    // A new window of the same exe (a restart) is still the same exe this run.
    assert_eq!(n.observe(Some((9, 90, "game.EXE")), s(61)), None);
    assert_eq!(n.observe(Some((9, 90, "game.EXE")), s(90)), None);
    // Another exe has its own clock, reset by any other foreground.
    let other = Some((8, 80, "Other.exe"));
    assert_eq!(n.observe(other, s(100)), None);
    assert_eq!(n.observe(None, s(110)), None, "alt-tab away");
    assert_eq!(n.observe(other, s(115)), None, "the clock starts over");
    assert_eq!(n.observe(other, s(130)), None);
    assert_eq!(n.observe(other, s(136)).as_deref(), Some("Other.exe"));
}

#[test]
fn composed_notice_needs_the_same_window_throughout() {
    let t0 = Instant::now();
    let s = |n: u64| t0 + Duration::from_secs(n);
    let mut n = ComposedNotice::default();
    assert_eq!(n.observe(Some((7, 70, "a.exe")), t0), None);
    assert_eq!(n.observe(Some((7, 71, "a.exe")), s(15)), None, "another window starts over");
    assert_eq!(n.observe(Some((7, 71, "a.exe")), s(30)), None);
    assert_eq!(n.observe(Some((7, 71, "a.exe")), s(36)).as_deref(), Some("a.exe"));
}
