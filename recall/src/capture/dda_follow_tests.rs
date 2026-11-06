//! Following a game window with monitor Desktop Duplication: injected window and desktop
//! states, no Win32. Alt-tab, minimize, occlusion and the lock screen hold frames; only a
//! real monitor change or a persistent failure restarts capture.

use std::time::{Duration, Instant};

use super::*;

const MONITOR: usize = 0x1001;
const OTHER_MONITOR: usize = 0x2002;
const SCREEN: Bounds = Bounds { left: 0, top: 0, right: 2560, bottom: 1440 };

/// The game in front, covering the captured monitor.
fn playing() -> WindowState {
    WindowState { exists: true, iconic: false, monitor: MONITOR, covers: true, foreground_is_game: true, monitor_rect: SCREEN, above: vec![] }
}

/// Alt-tabbed to a Discord / browser window that sits over the game.
fn covered_by(above: Above) -> WindowState {
    WindowState { foreground_is_game: false, above: vec![above], ..playing() }
}

fn discord() -> Above {
    Above { rect: Bounds { left: 200, top: 100, right: 1800, bottom: 1200 }, ..Above::default() }
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn follow_checks_every_quarter_second() {
    assert!(FOLLOW_CHECK <= ms(250), "occlusion is seen within ~250 ms, not {FOLLOW_CHECK:?}");
}

#[test]
fn a_window_over_the_game_holds_frames_without_a_restart() {
    let (mut f, t0) = (Follower::default(), Instant::now());
    assert_eq!(f.decide(&covered_by(discord()), MONITOR, Desktop::USER, t0), Follow::Hold(Hold::Covered));
    // Discord stays on top for ten minutes: still held, never a restart.
    for s in 1..=600 {
        assert_eq!(f.decide(&covered_by(discord()), MONITOR, Desktop::USER, t0 + Duration::from_secs(s)), Follow::Hold(Hold::Covered));
    }
}

#[test]
fn windows_that_never_show_or_belong_to_the_game_do_not_cover_it() {
    let (mut f, t0) = (Follower::default(), Instant::now());
    for above in [
        Above { cloaked: true, ..discord() },
        Above { tool: true, ..discord() },
        Above { click_through: true, ..discord() },
        Above { excluded: true, ..discord() }, // Recall's island / guide grid
        Above { game: true, ..discord() },
        Above { rect: Bounds { left: 2560, top: 0, right: 4000, bottom: 1440 }, ..discord() }, // on the next monitor
        Above { rect: Bounds { left: 10, top: 10, right: 11, bottom: 11 }, ..discord() }, // Explorer 1x1 helper
    ] {
        assert_eq!(f.decide(&covered_by(above), MONITOR, Desktop::USER, t0), Follow::Deliver, "{above:?}");
    }
}

#[test]
fn the_game_in_front_follows_even_with_a_window_listed_above() {
    let state = WindowState { above: vec![discord()], ..playing() };
    assert_eq!(Follower::default().decide(&state, MONITOR, Desktop::USER, Instant::now()), Follow::Deliver);
}

#[test]
fn alt_tab_out_and_back_resumes_without_a_restart() {
    let (mut f, t0) = (Follower::default(), Instant::now());
    assert_eq!(f.decide(&playing(), MONITOR, Desktop::USER, t0), Follow::Deliver);
    assert_eq!(f.decide(&covered_by(discord()), MONITOR, Desktop::USER, t0 + ms(250)), Follow::Hold(Hold::Covered));
    assert_eq!(f.decide(&covered_by(discord()), MONITOR, Desktop::USER, t0 + Duration::from_secs(30)), Follow::Hold(Hold::Covered));
    assert_eq!(f.decide(&playing(), MONITOR, Desktop::USER, t0 + Duration::from_secs(31)), Follow::Deliver, "back on top: frames resume");
}

#[test]
fn a_minimized_game_holds_and_never_restarts() {
    let (mut f, t0) = (Follower::default(), Instant::now());
    // Exclusive-fullscreen alt-tab and Win+D minimize the game; IsIconic windows sit off screen.
    let minimized = WindowState { iconic: true, covers: false, monitor: 0, foreground_is_game: false, ..playing() };
    for s in 0..=120 {
        assert_eq!(f.decide(&minimized, MONITOR, Desktop::USER, t0 + Duration::from_secs(s)), Follow::Hold(Hold::Minimized));
    }
    assert_eq!(f.decide(&playing(), MONITOR, Desktop::USER, t0 + Duration::from_secs(121)), Follow::Deliver, "restored: frames resume");
}

#[test]
fn moving_to_another_monitor_restarts_at_once() {
    let moved = WindowState { monitor: OTHER_MONITOR, ..playing() };
    assert!(matches!(Follower::default().decide(&moved, MONITOR, Desktop::USER, Instant::now()), Follow::Restart(_)));
}

#[test]
fn a_window_that_stops_covering_holds_then_restarts_after_the_grace() {
    let (mut f, t0) = (Follower::default(), Instant::now());
    let windowed = WindowState { covers: false, ..playing() };
    assert_eq!(f.decide(&windowed, MONITOR, Desktop::USER, t0), Follow::Hold(Hold::NotCovering));
    assert_eq!(f.decide(&windowed, MONITOR, Desktop::USER, t0 + LEAVE_GRACE - ms(1)), Follow::Hold(Hold::NotCovering));
    assert!(matches!(f.decide(&windowed, MONITOR, Desktop::USER, t0 + LEAVE_GRACE), Follow::Restart(_)));
}

#[test]
fn a_mode_switch_that_comes_back_within_the_grace_does_not_restart() {
    let (mut f, t0) = (Follower::default(), Instant::now());
    let windowed = WindowState { covers: false, ..playing() };
    assert_eq!(f.decide(&windowed, MONITOR, Desktop::USER, t0), Follow::Hold(Hold::NotCovering));
    assert_eq!(f.decide(&playing(), MONITOR, Desktop::USER, t0 + ms(1500)), Follow::Deliver);
    // The grace starts again on the next departure.
    assert_eq!(f.decide(&windowed, MONITOR, Desktop::USER, t0 + ms(3000)), Follow::Hold(Hold::NotCovering));
    assert_eq!(f.decide(&windowed, MONITOR, Desktop::USER, t0 + ms(4500)), Follow::Hold(Hold::NotCovering));
}

#[test]
fn a_closed_window_restarts_after_the_grace() {
    let (mut f, t0) = (Follower::default(), Instant::now());
    let gone = WindowState::default();
    assert_eq!(f.decide(&gone, MONITOR, Desktop::USER, t0), Follow::Hold(Hold::Gone));
    assert!(matches!(f.decide(&gone, MONITOR, Desktop::USER, t0 + LEAVE_GRACE), Follow::Restart(_)));
}

#[test]
fn the_lock_screen_holds_and_never_restarts() {
    let (mut f, t0) = (Follower::default(), Instant::now());
    let locked = Desktop { input_is_users: false, display_on: true };
    let asleep = Desktop { input_is_users: true, display_on: false };
    // Whatever the window looks like meanwhile (games often minimize on lock).
    let odd = WindowState { covers: false, foreground_is_game: false, ..playing() };
    for s in 0..=60 {
        let desktop = if s % 2 == 0 { locked } else { asleep };
        assert_eq!(f.decide(&odd, MONITOR, desktop, t0 + Duration::from_secs(s)), Follow::Hold(Hold::Away));
    }
    assert_eq!(f.decide(&playing(), MONITOR, Desktop::USER, t0 + Duration::from_secs(61)), Follow::Deliver);
}

#[test]
fn the_secure_desktop_for_a_minute_never_gives_up() {
    let (mut clock, t0) = (FailClock::default(), Instant::now());
    let secure = Desktop { input_is_users: false, display_on: true };
    // A re-create every <= 2 s (backoff cap) for 60 s of lock screen / UAC.
    let mut t = Duration::ZERO;
    for attempt in 0u32.. {
        if t > Duration::from_secs(60) {
            break;
        }
        assert!(!clock.give_up(t0 + t, secure), "gave up {t:?} into the secure desktop");
        t += backoff(attempt);
    }
    // Display off counts the same.
    let off = Desktop { input_is_users: true, display_on: false };
    assert!(!clock.give_up(t0 + Duration::from_secs(120), off));
    // Back on the user's desktop: the 5 s count starts from there.
    let back = t0 + Duration::from_secs(121);
    assert!(!clock.give_up(back, Desktop::USER));
    assert!(!clock.give_up(back + GIVE_UP, Desktop::USER));
    assert!(clock.give_up(back + GIVE_UP + ms(1), Desktop::USER));
}

#[test]
fn an_unlocked_failure_past_five_seconds_gives_up_as_before() {
    let (mut clock, t0) = (FailClock::default(), Instant::now());
    assert!(!clock.give_up(t0, Desktop::USER));
    assert!(!clock.give_up(t0 + ms(5000), Desktop::USER));
    assert!(clock.give_up(t0 + ms(5001), Desktop::USER));
    clock.recovered();
    assert!(!clock.give_up(t0 + ms(6000), Desktop::USER), "a re-create that worked starts the count again");
}

#[test]
fn re_creates_back_off_to_at_most_two_seconds() {
    assert!((0..100).all(|a| backoff(a) <= Duration::from_secs(2)));
}
