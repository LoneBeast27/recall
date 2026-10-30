//! A clip save's mark, the "shutter ring": while the save runs a 2 px arc of
//! ~1.1π circles the record-red dot (one turn per 900 ms, drawn in over the
//! first 300 ms). When the clip lands the arc closes into a full ring
//! (160 ms), the dot grows from the inner dot to fill the ring in the success
//! green (220 ms from 120 ms) and a tick draws over it (240 ms from 260 ms).
//! A failed save closes the ring in the failure colour, no fill, no tick.
//! Tones only: record red, success green, failure colour, the pill's dark.

use std::f32::consts::{PI, TAU};
use std::time::{Duration, Instant};

use crate::anim::ease_out;
use crate::layout::{Rgba, ACTIVE, ERROR, PILL, SIGNAL};
use crate::notice::SavePhase;

pub const TURN: Duration = Duration::from_millis(900);
pub const DRAW_IN: Duration = Duration::from_millis(300);
/// The spinning arc's length (radians).
pub const ARC: f32 = 1.1 * PI;
pub const CLOSE: Duration = Duration::from_millis(160);
pub const FILL_AT: Duration = Duration::from_millis(120);
pub const FILL_FOR: Duration = Duration::from_millis(220);
pub const TICK_AT: Duration = Duration::from_millis(260);
pub const TICK_FOR: Duration = Duration::from_millis(240);
/// The settle is over (the tick is drawn).
pub const SETTLED: Duration = Duration::from_millis(500);
/// The light sweep after a save lands waits for the fill.
pub const SWEEP_AFTER: Duration = Duration::from_millis(340);
/// The tick's colour: the pill's own dark, cut into the green (≈9.6:1;
/// the user's pick over white, 2026-10-30).
pub const TICK: Rgba = Rgba { rgb: PILL.rgb, a: 1.0 };

/// One frame of the badge. Angles in radians clockwise from 12 o'clock;
/// sizes in dot radii (the ring is 2, its stroke 0.5).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Badge {
    pub start: f32,
    pub sweep: f32,
    pub ring: Rgba,
    /// Dot radius: 1 = the inner dot, 2 = filling the ring.
    pub dot: f32,
    pub dot_color: Rgba,
    /// How much of the tick is drawn (0..1).
    pub tick: f32,
}

impl Badge {
    /// The still badge for a stage (previews, smoke renders).
    pub fn rest(phase: SavePhase) -> Self {
        match phase {
            SavePhase::Saving => Badge { start: 0.0, sweep: ARC, ring: SIGNAL, dot: 1.0, dot_color: SIGNAL, tick: 0.0 },
            SavePhase::Saved => Badge { start: 0.0, sweep: TAU, ring: ACTIVE, dot: 2.0, dot_color: ACTIVE, tick: 1.0 },
            SavePhase::Failed => Badge { start: 0.0, sweep: TAU, ring: ERROR, dot: 1.0, dot_color: ERROR, tick: 0.0 },
        }
    }
}

fn secs(d: Duration) -> f32 {
    d.as_secs_f32()
}

/// Where a save's mark is in its animation (kept by the island).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SaveMark {
    pub phase: SavePhase,
    /// The ring started spinning (`None`: the clip landed with no `saving` first).
    saving_since: Option<Instant>,
    /// The save landed or failed.
    settled_at: Option<Instant>,
}

impl SaveMark {
    pub fn new(phase: SavePhase, now: Instant) -> Self {
        let spinning = phase == SavePhase::Saving;
        Self { phase, saving_since: spinning.then_some(now), settled_at: (!spinning).then_some(now) }
    }

    /// Follows the notice's stage: a second `saving` keeps the ring spinning,
    /// the outcome settles it once, a new save after an outcome restarts it.
    pub fn follow(&mut self, phase: SavePhase, now: Instant) {
        match (self.phase, phase) {
            (SavePhase::Saving, SavePhase::Saving) => {}
            (_, SavePhase::Saving) => *self = Self::new(SavePhase::Saving, now),
            (old, new) if old == new => {}
            (SavePhase::Saving, outcome) => {
                self.phase = outcome;
                self.settled_at = Some(now);
            }
            (_, outcome) => *self = Self { phase: outcome, saving_since: None, settled_at: Some(now) },
        }
    }

    /// The spinning arc at `now`: (start, sweep). The turn is quantised to
    /// 1/2048 so identical moments draw identical frames.
    fn spin(&self, now: Instant) -> (f32, f32) {
        let Some(since) = self.saving_since else { return (0.0, 0.0) };
        let t = secs(now.saturating_duration_since(since));
        let turn = ((t / secs(TURN)).fract() * 2048.0).round() / 2048.0;
        (turn * TAU, ARC * ease_out(t / secs(DRAW_IN)))
    }

    pub fn badge(&self, now: Instant) -> Badge {
        let Some(at) = self.settled_at else {
            let (start, sweep) = self.spin(now);
            return Badge { start, sweep, ring: SIGNAL, dot: 1.0, dot_color: SIGNAL, tick: 0.0 };
        };
        let (start, from) = self.spin(at);
        let t = now.saturating_duration_since(at);
        let sweep = from + (TAU - from) * ease_out(secs(t) / secs(CLOSE));
        let outcome = if self.phase == SavePhase::Saved { ACTIVE } else { ERROR };
        let color = if t >= FILL_AT { outcome } else { SIGNAL };
        let (dot, tick) = match self.phase {
            SavePhase::Saved => {
                let fill = ease_out(secs(t.saturating_sub(FILL_AT)) / secs(FILL_FOR));
                (1.0 + fill, ease_out(secs(t.saturating_sub(TICK_AT)) / secs(TICK_FOR)))
            }
            _ => (1.0, 0.0),
        };
        Badge { start, sweep, ring: color, dot, dot_color: color, tick }
    }

    /// Frames are still changing: the ring spins, or the outcome is settling.
    pub fn animating(&self, now: Instant) -> bool {
        match self.settled_at {
            None => true,
            Some(at) => now.saturating_duration_since(at) < SETTLED,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn the_ring_draws_in_then_spins_one_turn_per_900_ms() {
        let now = Instant::now();
        let mark = SaveMark::new(SavePhase::Saving, now);
        assert_eq!(mark.badge(now).sweep, 0.0, "nothing drawn yet");
        assert!((mark.badge(now + DRAW_IN).sweep - ARC).abs() < 1e-5, "full arc after 300 ms");
        let (a, b) = (mark.badge(now + ms(450)), mark.badge(now + ms(450) + TURN));
        assert!((a.start - PI).abs() < 0.01, "half a turn at 450 ms: {}", a.start);
        assert_eq!(a, b, "a turn later the frame repeats exactly");
        assert_eq!((a.ring, a.dot, a.tick), (SIGNAL, 1.0, 0.0));
        assert!(mark.animating(now + Duration::from_secs(20)), "spins as long as the save runs");
    }

    #[test]
    fn a_saved_clip_closes_the_ring_fills_green_and_ticks() {
        let now = Instant::now();
        let mut mark = SaveMark::new(SavePhase::Saving, now);
        let landed = now + ms(1_800);
        let spinning = mark.badge(landed);
        mark.follow(SavePhase::Saved, landed);
        assert_eq!(mark.badge(landed), spinning, "no jump when the clip lands");
        let closed = mark.badge(landed + CLOSE);
        assert!((closed.sweep - TAU).abs() < 1e-4 && closed.start == spinning.start, "closes in place");
        assert_eq!(mark.badge(landed + ms(100)).ring, SIGNAL, "still red before the fill");
        let filling = mark.badge(landed + FILL_AT + ms(10));
        assert_eq!((filling.ring, filling.dot_color), (ACTIVE, ACTIVE));
        assert!(filling.dot > 1.0 && filling.dot < 2.0 && filling.tick == 0.0);
        assert!(mark.badge(landed + TICK_AT + ms(120)).tick > 0.5);
        let done = mark.badge(landed + SETTLED);
        assert_eq!((done.dot, done.tick), (2.0, 1.0));
        assert!(mark.animating(landed + ms(499)) && !mark.animating(landed + SETTLED));
    }

    #[test]
    fn a_failed_save_closes_in_the_failure_colour_without_a_tick() {
        let now = Instant::now();
        let mut mark = SaveMark::new(SavePhase::Saving, now);
        mark.follow(SavePhase::Failed, now + ms(900));
        let done = mark.badge(now + ms(900) + SETTLED);
        assert_eq!((done.ring, done.dot_color, done.dot, done.tick), (ERROR, ERROR, 1.0, 0.0));
        assert!((done.sweep - TAU).abs() < 1e-4);
    }

    #[test]
    fn a_clip_with_no_saving_first_plays_the_settle_from_the_dot() {
        let now = Instant::now();
        let mark = SaveMark::new(SavePhase::Saved, now);
        assert_eq!(mark.badge(now).sweep, 0.0, "starts as the plain dot");
        assert!((mark.badge(now + CLOSE).sweep - TAU).abs() < 1e-4);
        assert_eq!(mark.badge(now + SETTLED), Badge::rest(SavePhase::Saved));
    }

    #[test]
    fn a_second_save_keeps_spinning_and_a_new_save_after_the_outcome_restarts() {
        let now = Instant::now();
        let mut mark = SaveMark::new(SavePhase::Saving, now);
        mark.follow(SavePhase::Saving, now + ms(700));
        assert_eq!(mark.badge(now + ms(800)), SaveMark::new(SavePhase::Saving, now).badge(now + ms(800)), "not restarted");
        mark.follow(SavePhase::Saved, now + ms(1_000));
        let settle = mark.badge(now + ms(1_200));
        mark.follow(SavePhase::Saved, now + ms(1_100));
        assert_eq!(mark.badge(now + ms(1_200)), settle, "a repeated outcome does not replay");
        mark.follow(SavePhase::Saving, now + ms(2_000));
        assert_eq!(mark.badge(now + ms(2_000)).sweep, 0.0, "a new save draws its ring in again");
        assert_eq!(mark.phase, SavePhase::Saving);
    }
}
