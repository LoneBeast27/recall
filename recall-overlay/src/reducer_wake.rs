//! When [`Reducer::tick`] next has work, so the overlay can sleep until then
//! instead of ticking 4 times a second: a notice ending, a save turning
//! "Still saving…" or going unconfirmed, the status feed going stale.

use std::time::{Duration, Instant};

use super::{Reducer, STALE_AFTER};

/// `tick`'s stale check is strict (`> STALE_AFTER`): wake just past it.
const PAST: Duration = Duration::from_millis(1);

impl Reducer {
    /// The earliest moment after `now` at which `tick` can change something;
    /// None when nothing is pending. Any message in between (a status) may
    /// move it, so it is asked again after every tick. The stale deadline
    /// follows the last status: a controller that sends one at least every
    /// `STALE_AFTER` (a 1 s keepalive) never reaches it.
    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        let transient = self.transient.as_ref().map(|t| t.until);
        let stale = self.last_status.map(|at| at + STALE_AFTER + PAST);
        let [slow, confirm] = self.saves.deadlines();
        [transient, stale, slow, confirm].into_iter().flatten().filter(|at| *at > now).min()
    }
}
