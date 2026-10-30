//! The action notice's motion cue: one soft light band in the notice's own
//! tone colour sweeps across the pill once, left to right, while the pill
//! grows. Motion is what peripheral vision catches; no new colours, no
//! second pass, nothing continuous.

use std::time::{Duration, Instant};

use crate::anim::ease_in_out;
use crate::layout::Rgba;

/// The sweep starts once the pill has begun to grow...
pub const CUE_DELAY: Duration = Duration::from_millis(60);
/// ...and crosses it in this long.
pub const CUE_SWEEP: Duration = Duration::from_millis(560);
/// Peak opacity of the band's centre over the pill.
pub const CUE_PEAK: f32 = 0.42;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cue {
    pub start: Instant,
    pub color: Rgba,
}

impl Cue {
    /// 0..1 across the pill while the band is on screen, `None` before and after.
    pub fn progress(&self, now: Instant) -> Option<f32> {
        let since = now.checked_duration_since(self.start + CUE_DELAY)?;
        (since < CUE_SWEEP).then(|| since.as_secs_f32() / CUE_SWEEP.as_secs_f32())
    }

    /// Still to finish (waiting for its delay or sweeping).
    pub fn running(&self, now: Instant) -> bool {
        now < self.start + CUE_DELAY + CUE_SWEEP
    }
}

/// What the renderer draws for one frame of the sweep.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sheen {
    pub progress: f32,
    pub color: Rgba,
}

/// The band's gradient axis inside a `w`×`h` pill: transparent at `start`,
/// `CUE_PEAK` half-way, transparent at `end`. The axis leans up to the right,
/// so the band is a slanted streak; it enters fully left of the pill and
/// leaves fully right of it.
pub fn band(w: f32, h: f32, progress: f32) -> ((f32, f32), (f32, f32)) {
    let half = h * 0.9;
    let reach = half + h * 0.3; // the slant's horizontal extent, so no edge lingers
    let centre = -reach + (w + 2.0 * reach) * ease_in_out(progress);
    ((centre - half, h * 0.8), (centre + half, h * 0.2))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::ACTIVE;

    #[test]
    fn the_cue_waits_sweeps_once_then_ends() {
        let now = Instant::now();
        let cue = Cue { start: now, color: ACTIVE };
        assert_eq!(cue.progress(now), None, "waits for the pill to start growing");
        assert!(cue.running(now));
        let mid = now + CUE_DELAY + CUE_SWEEP / 2;
        assert!((cue.progress(mid).unwrap() - 0.5).abs() < 1e-3);
        let end = now + CUE_DELAY + CUE_SWEEP;
        assert_eq!(cue.progress(end), None);
        assert!(!cue.running(end));
        assert_eq!(cue.progress(end + Duration::from_secs(2)), None, "never comes back");
    }

    #[test]
    fn the_band_enters_from_the_left_and_leaves_on_the_right() {
        let (w, h) = (340.0, 58.0);
        let ((x0, _), (x1, _)) = band(w, h, 0.0);
        assert!(x1 + h * 0.3 <= 0.5, "fully left of the pill at the start: {x1}");
        let ((x0_end, _), _) = band(w, h, 1.0);
        assert!(x0_end - h * 0.3 >= w - 0.5, "fully right of the pill at the end: {x0_end}");
        assert!(x0 < x1);
        let centres: Vec<f32> = (0..=20).map(|i| band(w, h, i as f32 / 20.0)).map(|(a, b)| (a.0 + b.0) / 2.0).collect();
        assert!(centres.windows(2).all(|p| p[1] > p[0]), "one pass, left to right");
    }
}
