//! Size/opacity tweening for the island (≈200 ms ease-out, dynamic-island
//! feel), plus the short spring an action notice grows with.

use std::time::{Duration, Instant};

pub const DURATION: Duration = Duration::from_millis(200);
/// How long an action notice's spring runs (it lands exactly on its target).
pub const SPRING_FOR: Duration = Duration::from_millis(420);
/// Damping ratio: one visible overshoot (≈12% of the distance), then settle.
const ZETA: f32 = 0.55;
/// Natural frequency (rad/s): the overshoot peaks ≈160 ms in.
const OMEGA: f32 = 24.0;
/// From this fraction of `SPRING_FOR` the spring blends onto its target.
const TAIL: f32 = 0.65;

/// The island's drawn size (physical px) and window opacity (0..1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub w: f32,
    pub h: f32,
    pub alpha: f32,
}

impl Frame {
    pub fn lerp(self, to: Frame, t: f32) -> Frame {
        let mix = |a: f32, b: f32| a + (b - a) * t;
        Frame { w: mix(self.w, to.w), h: mix(self.h, to.h), alpha: mix(self.alpha, to.alpha) }
    }
}

/// Cubic ease-out: fast start, gentle landing.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Smoothstep: slow in, slow out.
pub fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// One axis of an under-damped spring from `from` (moving at `velocity` px/s)
/// to `to`, `secs` in; blended onto `to` over the tail so it lands exactly.
pub fn spring(from: f32, to: f32, velocity: f32, secs: f32) -> f32 {
    let total = SPRING_FOR.as_secs_f32();
    if secs >= total {
        return to;
    }
    let secs = secs.max(0.0);
    let (decay, omega_d) = (ZETA * OMEGA, OMEGA * (1.0 - ZETA * ZETA).sqrt());
    let d = from - to;
    let b = (velocity + decay * d) / omega_d;
    let free = to + (-decay * secs).exp() * (d * (omega_d * secs).cos() + b * (omega_d * secs).sin());
    let blend = ease_in_out((secs / total - TAIL) / (1.0 - TAIL));
    free + (to - free) * blend
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Motion {
    EaseOut,
    /// Size springs (from these starting velocities, px/s); opacity eases out.
    Spring { vw: f32, vh: f32 },
}

#[derive(Clone, Copy, Debug)]
pub struct Tween {
    from: Frame,
    to: Frame,
    start: Instant,
    duration: Duration,
    motion: Motion,
}

impl Tween {
    pub fn settled(frame: Frame, now: Instant) -> Self {
        Self { from: frame, to: frame, start: now, duration: Duration::ZERO, motion: Motion::EaseOut }
    }

    pub fn new(from: Frame, to: Frame, now: Instant) -> Self {
        Self { from, to, start: now, duration: DURATION, motion: Motion::EaseOut }
    }

    /// Spring from `from` (already moving at `velocity`, px/s for w and h) to `to`.
    pub fn spring(from: Frame, to: Frame, velocity: (f32, f32), now: Instant) -> Self {
        let motion = Motion::Spring { vw: velocity.0, vh: velocity.1 };
        Self { from, to, start: now, duration: SPRING_FOR, motion }
    }

    pub fn progress(&self, now: Instant) -> f32 {
        if self.duration.is_zero() {
            return 1.0;
        }
        now.saturating_duration_since(self.start).as_secs_f32() / self.duration.as_secs_f32()
    }

    pub fn at(&self, now: Instant) -> Frame {
        match self.motion {
            Motion::EaseOut => self.from.lerp(self.to, ease_out(self.progress(now))),
            Motion::Spring { vw, vh } => {
                let secs = now.saturating_duration_since(self.start).as_secs_f32();
                let fade = ease_out(secs / DURATION.as_secs_f32());
                Frame {
                    w: spring(self.from.w, self.to.w, vw, secs),
                    h: spring(self.from.h, self.to.h, vh, secs),
                    alpha: self.from.alpha + (self.to.alpha - self.from.alpha) * fade,
                }
            }
        }
    }

    /// Size velocity (px/s for w and h) at `now`, so a new motion can carry it on.
    pub fn velocity(&self, now: Instant) -> (f32, f32) {
        if self.done(now) {
            return (0.0, 0.0);
        }
        let step = Duration::from_millis(2);
        let (a, b) = (self.at(now), self.at(now + step));
        let secs = step.as_secs_f32();
        ((b.w - a.w) / secs, (b.h - a.h) / secs)
    }

    pub fn done(&self, now: Instant) -> bool {
        self.progress(now) >= 1.0
    }

    /// Head for `to`, starting from wherever the island is now.
    pub fn retarget(&mut self, to: Frame, now: Instant) {
        if to == self.to {
            return;
        }
        *self = Tween::new(self.at(now), to, now);
    }
}

#[cfg(test)]
#[path = "anim_tests.rs"]
mod tests;
