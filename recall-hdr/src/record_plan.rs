//! Planner -> engine, every pump tick, as a provided method ([`PlanHost::plan_tick`]) so the
//! record controller (`Controller::plan_engine`) and the transitions Sim run the same code
//! (review P3-2: the release settle and the drain notice were Controller-only lines no test
//! reached). Each host only supplies the reads and the effects: the engine's buffered seconds,
//! the supervisor's settle, applying a target, the long recording's stop, the log.
//!
//! A retarget that falls due while the long recording runs stops it first (one `record_stop`;
//! the retarget applies once it reports stopping, or after `PAUSE_STOP_WAIT`); one due while a
//! save is in flight waits for it. Meanwhile the status says `pending_restart`.

use std::time::Instant;

use crate::record_activity::PAUSE_STOP_WAIT;
use crate::record_drain::drain_len;
use crate::record_engine::{EngineTarget, Step};
use crate::record_transition::Transitions;

pub(crate) trait PlanHost: Transitions {
    /// The engine's buffered seconds (`EngineView::buffer_secs`) and the configured buffer.
    fn plan_buffered(&self) -> (Option<f32>, u32);
    /// Every save of the released engine is answered: it may be cut to the quit grace
    /// (`Supervisor::release_settled`).
    fn plan_settle_release(&mut self);
    /// Start, restart or stop the engine for `target` (`apply_engine_target`).
    fn plan_apply(&mut self, target: &EngineTarget);
    /// `Controller::retarget_held`: a retarget is held (for the status and the island).
    fn retarget_held(&mut self) -> &mut bool;
    /// Send `record_stop` for a retarget (logged when not sent; the stop's deadline bounds it).
    fn plan_record_stop(&mut self);
    fn plan_log(&mut self, line: &str);
    /// A tick the planner held (the Sim counts them).
    fn plan_held_tick(&mut self) {}

    /// One pump tick of the planner at `now` (the activity's timers read the awake clock).
    fn plan_tick(&mut self, now: Instant) {
        let (buffered, cap) = self.plan_buffered();
        self.planner_mut().set_drain_len(drain_len(buffered, cap));
        let wanted = self.wanted_now();
        let awake = self.awake_now();
        let busy = self.activity_mut().retarget_busy(awake);
        if self.activity_mut().release_may_settle(awake) {
            self.plan_settle_release(); // a released engine's saves were all answered (P7/P8)
        }
        let drained = self.running_drain(now);
        match self.planner_mut().step(&wanted, now, busy) {
            Step::Apply(target) => {
                *self.retarget_held() = false;
                self.note_drain_cut(drained.as_ref(), &target);
                self.plan_apply(&target);
            }
            Step::Held => {
                self.plan_held_tick();
                if !*self.retarget_held() {
                    self.plan_log("record: engine retarget deferred until the save ends");
                }
                *self.retarget_held() = true;
            }
            Step::StopLongRec { game_closed } => {
                let game_closed = self.closed_game(game_closed);
                if self.activity_mut().auto_stop(awake, game_closed) {
                    let why = if game_closed { "the game closed" } else { "a new game is wanted" };
                    self.plan_log(&format!("record: {why} -- stopping the long recording before the retarget (up to {} s)", PAUSE_STOP_WAIT.as_secs()));
                    self.plan_record_stop();
                }
                *self.retarget_held() = true;
            }
            Step::Keep => *self.retarget_held() = false,
        }
        self.drain_block_notice(); // design P17
    }
}
