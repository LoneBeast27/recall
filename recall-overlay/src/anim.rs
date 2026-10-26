//! Size/opacity tweening for the island (≈200 ms ease-out, dynamic-island feel).

use std::time::{Duration, Instant};

pub const DURATION: Duration = Duration::from_millis(200);

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

    /// The small, transparent pill an island grows out of and shrinks into.
    pub fn collapsed(w: f32, h: f32) -> Frame {
        Frame { w: (w * 0.35).max(h).min(w), h: h * 0.75, alpha: 0.0 }
    }
}

/// Cubic ease-out: fast start, gentle landing.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

#[derive(Clone, Copy, Debug)]
pub struct Tween {
    from: Frame,
    to: Frame,
    start: Instant,
    duration: Duration,
}

impl Tween {
    pub fn settled(frame: Frame, now: Instant) -> Self {
        Self { from: frame, to: frame, start: now, duration: Duration::ZERO }
    }

    pub fn new(from: Frame, to: Frame, now: Instant) -> Self {
        Self { from, to, start: now, duration: DURATION }
    }

    pub fn target(&self) -> Frame {
        self.to
    }

    pub fn progress(&self, now: Instant) -> f32 {
        if self.duration.is_zero() {
            return 1.0;
        }
        now.saturating_duration_since(self.start).as_secs_f32() / self.duration.as_secs_f32()
    }

    pub fn at(&self, now: Instant) -> Frame {
        self.from.lerp(self.to, ease_out(self.progress(now)))
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
mod tests {
    use super::*;

    const A: Frame = Frame { w: 100.0, h: 44.0, alpha: 1.0 };
    const B: Frame = Frame { w: 300.0, h: 44.0, alpha: 1.0 };

    #[test]
    fn ease_out_is_monotonic_and_front_loaded() {
        assert_eq!((ease_out(0.0), ease_out(1.0), ease_out(2.0)), (0.0, 1.0, 1.0));
        assert!(ease_out(0.5) > 0.5, "ease-out covers most of the distance early");
        let steps: Vec<f32> = (0..=10).map(|i| ease_out(i as f32 / 10.0)).collect();
        assert!(steps.windows(2).all(|pair| pair[1] >= pair[0]));
    }

    #[test]
    fn tween_runs_from_start_to_target_in_200_ms() {
        let now = Instant::now();
        let tween = Tween::new(A, B, now);
        assert_eq!(tween.at(now), A);
        assert!(!tween.done(now + Duration::from_millis(199)));
        assert_eq!(tween.at(now + DURATION), B);
        assert!(tween.done(now + DURATION));
        let mid = tween.at(now + DURATION / 2).w;
        assert!(mid > 200.0 && mid < 300.0, "past the linear midpoint at half time: {mid}");
    }

    #[test]
    fn retarget_mid_flight_continues_from_the_current_frame() {
        let now = Instant::now();
        let mut tween = Tween::new(A, B, now);
        let half = now + DURATION / 2;
        let here = tween.at(half);
        tween.retarget(A, half);
        assert_eq!(tween.at(half), here, "no jump");
        assert_eq!(tween.at(half + DURATION), A);
    }

    #[test]
    fn collapsed_pill_is_small_transparent_and_never_wider_than_the_island() {
        let collapsed = Frame::collapsed(300.0, 44.0);
        assert_eq!((collapsed.w, collapsed.h, collapsed.alpha), (105.0, 33.0, 0.0));
        assert_eq!(Frame::collapsed(30.0, 44.0).w, 30.0);
    }
}
