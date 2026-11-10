//! `ReleaseWait` (design-detect-release P10-P12, P19): the kill moment from facts, and
//! `finish_shutdown`'s live wait on a fake awake clock, with heartbeats fed through the real
//! stdout reader (`read_protocol`) and a wedged fake engine (cmd.exe /D /Q, no window).

use super::*;
use crate::awake_clock::FakeClock;
use crate::record_dispatch::Sent;
use std::time::Instant;

const S: fn(u64) -> Duration = Duration::from_secs;

fn at(secs: u64) -> Awake {
    Awake::ZERO + S(secs)
}

#[test]
fn kill_at_takes_the_earliest_fact_and_moves_with_every_beat() {
    let mut w = ReleaseWait::new(at(0), true);
    assert_eq!(w.kill_at(None), at(18), "no beat yet: from the release, 10 s + the grace");
    assert_eq!(w.kill_at(Some(at(5))), at(23));
    // P10 non-sticky: a hung reading at 12 s (no beat since 0) is no verdict; a beat at 12 s
    // moves the kill to 30 s.
    assert!(!w.poll(at(12), None, false, None) && w.kill_at(Some(at(12))) == at(30));
    let mut quiet = ReleaseWait::new(at(0), false);
    assert_eq!(quiet.kill_at(None), at(0) + RELEASE_GRACE, "no heartbeats: only the bound (P19)");
    assert!(!quiet.poll(at(1), None, true, None) && quiet.kill_at(None) == at(1) + QUIT_GRACE, "settled: 8 s");
    assert!(!w.poll(at(13), Some(at(12)), false, Some(S(1))) && w.kill_at(Some(at(12))) == at(14), "app quit: its grace");
}

#[test]
fn a_poll_after_an_observer_gap_restarts_the_heartbeat_window() {
    let mut w = ReleaseWait::new(at(0), true);
    for s in 1..=15 {
        assert!(!w.poll(at(s), None, false, None), "{s} s");
    }
    assert!(!w.poll(at(20), None, false, None), "5 s with no poll: the supervisor stalled, not the engine");
    assert_eq!(w.kill_at(None), at(38));
}

#[test]
fn the_engines_bound_is_inside_the_supervisors_on_the_same_clock() {
    // P19: the engine's 30 min budget + its 0.5 s abort reserve + the mover's 1.5 s, all awake.
    assert!(Duration::from_secs(30 * 60) + Duration::from_millis(2_000) < RELEASE_GRACE);
}

/// P11: a heartbeat line read by the real stdout reader moves the beat (R2-p7-3: deleting
/// `note_beat` from `protocol_line` left every heartbeat engine killed at 18 s, suite green).
#[test]
fn a_heartbeat_read_from_the_engine_is_noted_on_the_awake_clock() {
    let clock = Arc::new(FakeClock::new());
    let shared = Arc::new(Shared::with_clock(clock.clone()));
    clock.advance_awake(S(7));
    super::super::protocol::read_protocol(&shared, &b"{\"type\":\"save_progress\",\"stalled_for\":3}\n"[..]);
    assert_eq!(shared.last_beat(), Some(at(7)));
}

/// A released heartbeat engine (wedged cmd.exe) in `finish_shutdown` on its own thread.
struct Live {
    clock: Arc<FakeClock>,
    shared: Arc<Shared>,
    done: std::sync::mpsc::Receiver<()>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Live {
    fn start(beats: bool) -> Live {
        let clock = Arc::new(FakeClock::new());
        let shared = Arc::new(Shared::with_clock(clock.clone()));
        let hello = format!(r#"{{"type":"hello","stop_capture":true,"drain_heartbeat":{beats}}}"#);
        shared.cache.lock().unwrap().hello = Some(hello);
        let mut child = super::super::manager::wedged_engine();
        shared.set_child_stdin(child.stdin.take(), None);
        shared.set_desired_how(None, Sent::Switching, true);
        let (tx, done) = std::sync::mpsc::channel();
        let waiter = shared.clone();
        let handle = std::thread::spawn(move || {
            let _ = super::super::manager::finish_shutdown(&waiter, Box::new(child), RELEASE_GRACE, None, true);
            let _ = tx.send(());
        });
        let live = Live { clock, shared, done, handle: Some(handle) };
        live.clock.wait_reads(1); // the wait's first poll ran
        live
    }

    /// The wait ended (the engine was killed).
    fn killed(&self) -> bool {
        !matches!(self.done.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty))
    }

    /// Advance the awake clock from `from` to `secs` s, 1 s per poll (no observer gap), and
    /// beat through the reader at every step when `beat`. Returns false once killed.
    fn run_to(&self, from: u64, secs: u64, beat: bool) -> bool {
        for _ in from..secs {
            self.clock.advance_awake(S(1));
            if beat {
                super::super::protocol::read_protocol(&self.shared, &b"{\"type\":\"save_progress\",\"stalled_for\":0}
"[..]);
            }
            if !self.polled_after(self.clock.reads()) {
                return false;
            }
        }
        std::thread::sleep(Duration::from_millis(120)); // a kill on the last poll lands
        !self.killed()
    }

    /// Wait until the wait read the clock again after `reads` reads (true: the read began after
    /// the caller's advance, so it saw it) or ended (false).
    fn polled_after(&self, reads: u64) -> bool {
        let until = Instant::now() + S(10);
        loop {
            if self.killed() {
                return false;
            }
            if self.clock.reads() > reads {
                return true;
            }
            assert!(Instant::now() < until, "the wait stopped polling");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        // Never leave the fake engine behind: a test that failed early still ends the wait.
        self.shared.request_stop(Duration::ZERO);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// P12: no heartbeat at all: killed at 18 s awake, not before.
#[test]
fn a_released_engine_with_no_heartbeat_is_killed_at_18_s_awake() {
    let live = Live::start(true);
    assert!(live.run_to(0, 17, false), "killed before 10 s + the grace");
    assert!(!live.run_to(17, 19, false), "killed once its heartbeats were lost");
}

/// P10/P11 live: beats through the reader keep it past 18 s; once they stop it dies 18 s later.
/// A 2 h sleep in the middle, and the resume's retarget after it, change nothing.
#[test]
fn a_released_engine_kept_alive_by_heartbeats_dies_18_s_after_they_stop() {
    let live = Live::start(true);
    assert!(live.run_to(0, 25, true), "killed while its heartbeats flowed");
    live.clock.sleep_for(S(2 * 3600));
    // The resume's restart re-targets the next engine (P15): the drain is not cut.
    live.shared.set_desired_how(Some(Default::default()), Sent::Restarting, false);
    assert!(live.run_to(25, 40, true), "a sleep is not a silence, a resume retarget no kill");
    assert!(live.run_to(40, 57, false), "killed before 18 s of silence");
    assert!(!live.run_to(57, 59, false), "killed 18 s after the last beat");
}

/// P10 non-sticky, live: silent until 12 s (a "hung" reading), then one beat: the kill moves
/// to 30 s.
#[test]
fn a_beat_after_a_hung_reading_moves_the_kill_on() {
    let live = Live::start(true);
    assert!(live.run_to(0, 12, false));
    assert!(live.run_to(12, 13, true), "one beat at 13 s");
    assert!(live.run_to(13, 30, false), "killed before 13 + 18 s");
    assert!(!live.run_to(30, 32, false));
}

/// P10 gap rule, live: the clock jumps 6 s between two polls (the supervisor did not run):
/// the silence before it is not counted.
#[test]
fn an_observer_gap_is_not_the_engines_silence() {
    let live = Live::start(true);
    assert!(live.run_to(0, 15, false));
    live.clock.advance_awake(S(6));
    assert!(live.polled_after(live.clock.reads()), "the gap poll");
    assert!(live.run_to(21, 38, false), "kept until the gap poll + 18 s");
    assert!(!live.run_to(38, 40, false));
}

/// P10: a settled release (every save answered) dies 8 s after the settle; an engine without
/// heartbeats is never killed for silence.
#[test]
fn an_old_engine_is_killed_only_after_the_settle() {
    let live = Live::start(false);
    assert!(live.run_to(0, 60, false), "no heartbeat capability: silence is no hang");
    let sup = super::super::Supervisor { shared: live.shared.clone(), handle: None };
    sup.release_settled();
    assert!(live.run_to(60, 68, false));
    assert!(!live.run_to(68, 70, false), "8 s after the settle");
}
