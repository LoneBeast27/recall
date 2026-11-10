//! `saver_stop` (moved out for file size): the orphan / released drain's hold, its heartbeat
//! and the engine's own give-up, on an injected awake-only clock (design-detect-release P5/P6).

use super::*;
use crate::awake_clock::FakeClock;
use std::time::Instant;

const S: fn(u64) -> Duration = Duration::from_secs;

/// A saver that is done at once (its sender already dropped).
fn ended() -> Receiver<()> {
    let (tx, done) = std::sync::mpsc::channel::<()>();
    drop(tx);
    done
}

#[test]
fn an_orphan_holds_the_engine_only_while_a_save_makes_progress() {
    // R6-overlay-2: the orphan keeps the engine mutex while it waits.
    let t0 = Awake::ZERO;
    let normal = S(3);
    let mut idle = OrphanHold::new(t0, 0, normal);
    assert_eq!(idle.keep(0, 0, t0 + S(1)), None, "the normal budget always");
    assert_eq!(idle.keep(0, 0, t0 + S(4)), Some(None), "nothing in flight: exit after the normal budget");

    let mut busy = OrphanHold::new(t0, 0, normal);
    for sec in 1..=600u64 {
        assert_eq!(busy.keep(1, sec, t0 + S(sec)), None, "a save still moving at {sec} s holds on");
    }
    let mut hung = OrphanHold::new(t0, 5, normal);
    assert_eq!(hung.keep(1, 5, t0 + S(119)), None);
    assert_eq!(hung.keep(1, 5, t0 + S(120)), Some(Some(STALLED_REASON)), "no progress for 120 s: a hung encoder, let go");

    let mut late = OrphanHold::new(t0, 0, normal);
    assert!(late.keep(1, 0, t0 + S(60)).is_none() && late.keep(0, 0, t0 + S(61)) == Some(None), "the save answered: exit");

    let mut long = OrphanHold::new(t0, 0, normal);
    assert_eq!(long.keep(1, 1, t0 + ORPHAN_SAVE_BUDGET), Some(Some(BUDGET_REASON)), "never past the 30 min cap");
}

#[test]
fn a_draining_save_reports_its_progress_every_poll() {
    // R2-detect-r10-p2-1 / p6-1: after stop_capture no `status` line goes out; the drain says
    // the save still writes (or stalls) on every poll, so the controller knows it is alive.
    let progress = SaveProgress::default();
    progress.jobs.store(2, Relaxed);
    let (tx, done) = std::sync::mpsc::channel::<()>();
    let ender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        drop(tx);
    });
    let clock = FakeClock::stepping(S(1), Duration::ZERO);
    let mut seen = Vec::new();
    let outcome = wait_orphan(&done, &progress, Duration::from_millis(10), &clock, Duration::from_millis(20), |w, stalled| seen.push((w, stalled)));
    ender.join().unwrap();
    assert_eq!(outcome, Hold::Drained, "the saver ended");
    assert!(seen.len() >= 2, "one per poll: {seen:?}");
    assert_eq!(seen[0].0.map(|w| w.queued), Some(1), "the one writing plus one behind it");
    let stalled: Vec<u64> = seen.iter().map(|(_, s)| *s).collect();
    assert!(stalled[0] <= 1 && *stalled.last().unwrap() >= 2, "no step moved: stalled_for counts up ({stalled:?})");
}

/// Design P5: the first heartbeat goes out before the first poll's wait. A saver that is
/// already done still produced one `save_progress` (the controller's heartbeat window starts
/// at the release, and a 1 s poll must not open it empty).
#[test]
fn the_first_heartbeat_is_sent_before_the_first_poll_wait() {
    crate::protocol::test_emits::install();
    let progress = SaveProgress::default();
    progress.jobs.store(1, Relaxed);
    let (abort, current) = (AtomicBool::new(false), Mutex::new(None));
    let outcome = stop_saver_on(&ended(), &abort, &current, Duration::from_millis(20), Some(&progress), &FakeClock::new(), S(30));
    assert_eq!(outcome, Shutdown::Drained);
    let beats = crate::protocol::test_emits::of("save_progress");
    assert_eq!(beats.len(), 1, "one beat before the wait: {beats:?}");
    assert!(beats[0].contains(r#""stalled_for":0"#) && beats[0].contains(r#""queued":0"#), "{beats:?}");
}

#[test]
fn stop_saver_emits_save_progress_on_the_protocol_while_it_drains() {
    // R2-detect-r10-p3-2: the live quit path (`stop_saver`, the real clock and poll) puts
    // `save_progress` on the protocol. This thread's own lines are kept.
    crate::protocol::test_emits::install();
    let progress = SaveProgress::default();
    progress.jobs.store(1, Relaxed);
    let (tx, done) = std::sync::mpsc::channel::<()>();
    let ender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1_500));
        drop(tx);
    });
    let (abort, current) = (AtomicBool::new(false), Mutex::new(None));
    let outcome = stop_saver(&done, &abort, &current, Duration::from_millis(20), Some(&progress));
    ender.join().unwrap();
    assert_eq!(outcome, Shutdown::Drained);
    let seen = crate::protocol::test_emits::of("save_progress").len();
    assert!(seen >= 2, "save_progress lines while draining: {seen}");
}

/// Design P6, sleep: every poll the PC slept an hour and was awake 1 ms; the save's step never
/// moves. Asleep time ages nothing (a day or more of it), so the save is kept until the saver ends.
#[test]
fn a_sleep_during_the_drain_never_gives_up_a_healthy_save() {
    crate::protocol::test_emits::install();
    let progress = SaveProgress::default();
    progress.jobs.store(1, Relaxed);
    let (tx, done) = std::sync::mpsc::channel::<()>();
    let ender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1_500));
        drop(tx);
    });
    let clock = FakeClock::stepping(Duration::from_millis(1), S(3600));
    let (abort, current) = (AtomicBool::new(false), Mutex::new(None));
    let outcome = stop_saver_on(&done, &abort, &current, Duration::from_millis(4), Some(&progress), &clock, Duration::from_millis(1));
    ender.join().unwrap();
    assert!(clock.reads() > 20, "polled through the sleep ({} reads)", clock.reads());
    assert_eq!(outcome, Shutdown::Drained, "the save was let finish");
    assert!(!abort.load(Relaxed) && crate::protocol::test_emits::of("save_failed").is_empty());
}

/// Design P6, stall: 120 s awake with no step. The engine lets go and answers BOTH saves in
/// the saver itself ("The save stopped making progress") before the abort, which the mux
/// here ignores (TimedOut: before, nothing answered them).
#[test]
fn a_stalled_save_is_given_up_by_the_engine_with_its_own_answer() {
    crate::protocol::test_emits::install();
    let progress = SaveProgress::default();
    progress.jobs.store(2, Relaxed);
    let (_tx, done) = std::sync::mpsc::channel::<()>(); // the saver never ends
    let clock = FakeClock::stepping(S(1), Duration::ZERO);
    let (abort, current) = (AtomicBool::new(false), Mutex::new(None));
    let started = Instant::now();
    let outcome = stop_saver_on(&done, &abort, &current, Duration::from_millis(4), Some(&progress), &clock, Duration::from_millis(1));
    assert_eq!(outcome, Shutdown::TimedOut);
    assert!(started.elapsed() < S(15) && abort.load(Relaxed), "let go and aborted");
    assert!((119..=124).contains(&clock.reads()), "let go at 120 s awake ({} reads)", clock.reads());
    let failed = crate::protocol::test_emits::of("save_failed");
    assert_eq!(failed.len(), 2, "one answer per save in the saver: {failed:?}");
    assert!(failed.iter().all(|l| l.contains(STALLED_REASON)), "{failed:?}");
    assert!(progress.given_up.load(Relaxed), "the saver sends no second answer");
    let beats = crate::protocol::test_emits::of("save_progress");
    assert!(beats.len() >= 119 && beats.last().unwrap().contains(r#""stalled_for":120"#), "{:?}", beats.last());
}

#[test]
fn an_orphan_with_nothing_to_save_and_a_hung_encoder_exits_promptly() {
    // The encoder thread still holds the queue (a hung NVENC): the saver never ends.
    let (events, _sink) = super::super::saver::Events::capture(true);
    let mux: super::super::saver::MuxFn = Box::new(|_, _, _, _| unreachable!("no job submitted"));
    let (queue, control) = super::super::saver::spawn(events, mux, Box::new(|_, _, _, _| {}));
    let started = Instant::now();
    assert_eq!(control.shutdown_orphan(Duration::from_millis(600)), Shutdown::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(3), "{:?} (was 30 min)", started.elapsed());
    drop(queue);
}
