//! Every recovery restart is a `RestartReason::Recovery` (SPEC item 26a): a hung engine, a
//! lasting encoder failure and a capture stuck before its first frame, each driven through
//! its real decision code (`EncoderRecovery::step_held`, `EndingEarly::decide`,
//! `stuck_step`) into the restart call `encoder_recover_tick` / `capture_stuck_tick` make
//! (`record_recovery_act`), with the Sim as the restart host. A site that restarted as a
//! resume (a planned restart, "Recorder is restarting") fails here.

use std::time::{Duration, Instant};

use crate::record_capture_watch::{stuck_step, StuckInputs, StuckWatch, STUCK_AFTER};
use crate::record_engine::{EnginePlanner, EngineTarget};
use crate::record_recover::{EncoderRecovery, EndingEarly, Hold, Recovery};
use crate::record_recovery_act::{ending_early_step, recovery_restart, stuck_restart, EarlyStep};
use crate::record_restart::{RestartHost, RestartReason};
use crate::record_screen::Sight;
use crate::record_target::Target;
use crate::record_status::session_state;
use crate::supervisor::EngineHealth;
use crate::transitions_sim::{game_ev, Sim, Switch};

const VAL: &str = "VALORANT-Win64-Shipping";
const RUN: EngineHealth = EngineHealth::Running { pid: 9 };

/// The Sim as the restart host, with each restart's reason kept.
struct Spy {
    sim: Sim,
    reasons: Vec<RestartReason>,
}

impl RestartHost for Spy {
    fn restart_planner(&mut self) -> &mut EnginePlanner {
        self.sim.restart_planner()
    }
    fn restart_busy(&mut self) -> bool {
        self.sim.restart_busy()
    }
    fn restart_wanted(&self) -> EngineTarget {
        self.sim.restart_wanted()
    }
    fn restart_pending(&mut self) -> &mut Option<RestartReason> {
        self.sim.restart_pending()
    }
    fn restart_apply(&mut self, target: &EngineTarget, reason: RestartReason) {
        self.reasons.push(reason);
        self.sim.restart_apply(target, reason);
    }
    fn restart_notice(&mut self, title: &str, detail: &str) {
        self.sim.restart_notice(title, detail);
    }
    fn restart_log(&mut self, line: &str) {
        self.sim.restart_log(line);
    }
}

/// A recorded mid-game (no drain, nothing in flight).
fn in_game() -> Spy {
    let mut sim = Sim::new();
    sim.focus(game_ev(VAL, 10, 0x10));
    sim.run_to(5_000);
    assert!(sim.recording_of(10), "setup: the game is recorded");
    Spy { sim, reasons: Vec::new() }
}

/// One restart, a recovery: unplanned at the supervisor ("Recorder isn't responding").
fn recovered_once(spy: &Spy, what: &str) {
    assert_eq!(spy.reasons, [RestartReason::Recovery], "{what}");
    assert_eq!(spy.sim.switches.count(), 1, "{what}: one supervisor call");
    assert_eq!(spy.sim.switches.last(), Some(Switch::Restart { stop: false, planned: false }), "{what}");
}

/// `encoder_recover_tick`'s inputs once a second from `t0`: (encoder state, heartbeat) per
/// second; the second its `Recovery::Restart` came, if one did by `to`.
fn first_restart(r: &mut EncoderRecovery, spy: &Spy, t0: Instant, to: u64, hold: Hold, at: impl Fn(u64) -> (&'static str, u64)) -> Option<u64> {
    let wanted = spy.sim.wanted();
    (0..=to).find(|&s| {
        let (encoder, beat) = at(s);
        let state = session_state(&wanted, RUN, Some(encoder), true, false);
        r.step_held(t0 + Duration::from_secs(s), state, RUN, beat, false, true, hold, "VAL:10") == Recovery::Restart
    })
}

/// A hung encoder during a long recording: the recording ends early first (`record_fail`),
/// then the ending-early wait restarts the engine once it reports.
#[test]
fn a_hung_engine_during_a_long_recording_restarts_as_a_recovery() {
    let mut spy = in_game();
    let (t0, mut r) = (spy.sim.now, EncoderRecovery::default());
    let hold = Hold { long_rec: true, ..Hold::default() };
    let hung = first_restart(&mut r, &spy, t0, 120, hold, |s| ("recording", s.min(20))).expect("the hang is seen");
    let lines = recovery_restart(&mut spy, true);
    assert!(lines.iter().any(|l| l.contains("record_fail")), "the recording ends early first: {lines:?}");
    assert!(spy.reasons.is_empty(), "no restart before the recording reports");
    let at = t0 + Duration::from_secs(hung);
    let wait = EndingEarly::new(at, 1);
    assert_eq!(ending_early_step(&mut spy, wait.decide(at + Duration::from_secs(1), true, 1)), EarlyStep::Wait);
    assert_eq!(ending_early_step(&mut spy, wait.decide(at + Duration::from_secs(2), false, 1)), EarlyStep::Restarted);
    recovered_once(&spy, "hang");
}

/// The encoder says `failed` for past 15 s, no long recording: the engine restarts at once.
#[test]
fn a_lasting_encoder_failure_restarts_as_a_recovery() {
    let mut spy = in_game();
    let (t0, mut r) = (spy.sim.now, EncoderRecovery::default());
    let failed = first_restart(&mut r, &spy, t0, 60, Hold::default(), |s| ("failed", s * 10));
    assert!(failed.is_some_and(|s| s > 15), "restart after the 15 s grace: {failed:?}");
    assert_eq!(recovery_restart(&mut spy, false), Vec::<String>::new());
    recovered_once(&spy, "encoder failed");
}

/// The game's session stays `starting` with no first frame: one restart.
#[test]
fn a_capture_stuck_before_its_first_frame_restarts_as_a_recovery() {
    let mut spy = in_game();
    let wanted = spy.sim.wanted();
    let i = StuckInputs { wanted: &wanted, health: RUN, encoder: Some("starting"), fresh: true, paused: false, running: true, held: false, overlay: true };
    let (t0, mut w) = (spy.sim.now, StuckWatch::default());
    let act = (0..=STUCK_AFTER.as_secs() + 5)
        .find_map(|s| stuck_step(&mut w, &i, |_: &Target| Sight::SHOWN, t0 + Duration::from_secs(s)))
        .expect("the stuck session acts");
    assert!(stuck_restart(&mut spy, &act), "the first act is the restart");
    recovered_once(&spy, "capture stuck");
}
