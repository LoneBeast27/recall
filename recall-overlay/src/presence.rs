//! How the island appears from nothing and leaves again. A circle the pill's
//! height pops in at the anchored (right) end with a small spring (150 ms),
//! carrying the mark; then it unravels sideways into the pill (250 ms) and
//! the content fades in once the pill is open. Hiding is the same motion
//! backwards, after the content has faded out. One progress value runs
//! forward or back at the same rate, so a reversal mid-flight carries on
//! from exactly where the island is.

use std::time::{Duration, Instant};

use crate::anim::{ease_out, Frame};

pub const POP: Duration = Duration::from_millis(150);
pub const UNRAVEL: Duration = Duration::from_millis(250);
/// The content fades out this long (from full) before the pill rolls up.
pub const FADE_OUT: Duration = Duration::from_millis(150);
/// The instant style's window fade (in and out).
pub const FADE_IN: Duration = Duration::from_millis(120);
/// Ease-out-back constant: the circle overshoots its size by ≈5.7%.
const BACK: f32 = 1.25;

/// How the island appears from nothing and leaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// The circle pops in and unravels (and rolls back up).
    Unravel,
    /// The open pill with its content, the window fading in (and out) in
    /// `FADE_IN`: for what must be readable at once.
    Instant,
}

fn open_for() -> f32 {
    (POP + UNRAVEL).as_secs_f32()
}

fn pop_share() -> f32 {
    POP.as_secs_f32() / open_for()
}

/// Ease-out with one small overshoot, landing exactly on 1.
pub fn ease_out_back(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0) - 1.0;
    1.0 + (BACK + 1.0) * t * t * t + BACK * t * t
}

#[derive(Clone, Copy, Debug)]
pub struct Presence {
    /// Progress (0 = nothing, 1 = open) at `at`.
    from: f32,
    /// When the motion (re)started; a hide waits here for the content fade.
    at: Instant,
    opening: bool,
    style: Style,
}

impl Presence {
    pub fn hidden(now: Instant) -> Self {
        Self { from: 0.0, at: now, opening: false, style: Style::Unravel }
    }

    fn span(&self) -> Duration {
        match self.style {
            Style::Unravel => POP + UNRAVEL,
            Style::Instant => FADE_IN,
        }
    }

    pub fn progress(&self, now: Instant) -> f32 {
        let moved = now.saturating_duration_since(self.at).as_secs_f32() / self.span().as_secs_f32();
        if self.opening { (self.from + moved).min(1.0) } else { (self.from - moved).max(0.0) }
    }

    /// Open (again) in `style`, from wherever it is now.
    pub fn show(&mut self, now: Instant, style: Style) {
        *self = Self { from: self.progress(now), at: now, opening: true, style };
    }

    /// Close: an open, unravelled pill waits `fade` for its content to fade
    /// out first; an instant one fades with its window.
    pub fn hide(&mut self, now: Instant, fade: Duration) {
        let from = self.progress(now);
        let at = if from >= 1.0 && self.style == Style::Unravel { now + fade } else { now };
        *self = Self { from, at, opening: false, style: self.style };
    }

    /// When an opening pill is (or was) fully open.
    pub fn opened_at(&self) -> Instant {
        let left = (1.0 - self.from).max(0.0) as f64 * self.span().as_nanos() as f64;
        self.at + Duration::from_nanos(left.round() as u64)
    }

    /// The content may fade in from here (`None`: at once, with the window).
    pub fn text_ready(&self) -> Option<Instant> {
        (self.style == Style::Unravel).then(|| self.opened_at())
    }

    /// The window's opacity: the instant style fades it; the circle does not.
    pub fn window_alpha(&self, now: Instant) -> f32 {
        match self.style {
            Style::Unravel => 1.0,
            Style::Instant => ease_out(self.progress(now)),
        }
    }

    pub fn gone(&self, now: Instant) -> bool {
        !self.opening && self.progress(now) <= 0.0
    }

    pub fn moving(&self, now: Instant) -> bool {
        let p = self.progress(now);
        if self.opening { p < 1.0 } else { p > 0.0 }
    }

    /// (circle scale, unravel) at `now`: the circle pops to its size first,
    /// then unrolls into the pill.
    pub fn shape(&self, now: Instant) -> (f32, f32) {
        if self.style == Style::Instant {
            return (1.0, 1.0);
        }
        let (p, share) = (self.progress(now), pop_share());
        if p < share {
            (ease_out_back(p / share), 0.0)
        } else {
            (1.0, ease_out((p - share) / (1.0 - share)))
        }
    }
}

/// One drawn frame of the island (physical px). The anchor places a
/// `box_w`×`box_h` box (right-anchored); the window is that box plus
/// `margin` on every side (room for the circle's overshoot).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub box_w: f32,
    pub box_h: f32,
    pub margin: f32,
    /// The drawn pill in box coordinates: x, y, w, h (a circle while popping).
    pub pill: [f32; 4],
    /// The open pill's width (the content is laid out for it).
    pub full_w: f32,
    /// 0 = still a circle, 1 = open: the mark rides the left end meanwhile.
    pub unroll: f32,
    /// The circle's scale (the mark scales with it).
    pub pop: f32,
    pub alpha: f32,
    pub content_alpha: f32,
    /// The title line's opacity (≤ `content_alpha`; it fades alone when only it changed).
    pub title_alpha: f32,
    pub icon_alpha: f32,
}

impl Look {
    /// The pill open at `frame` (transformations, previews).
    pub fn open(frame: Frame) -> Self {
        Self::shaped(frame, 1.0, 1.0)
    }

    /// The pill at `frame` size, `pop` / `unroll` of the way open; alphas 1.
    pub fn shaped(frame: Frame, pop: f32, unroll: f32) -> Self {
        let (w, h) = (frame.w, frame.h);
        let (box_w, pill, margin) = if unroll <= 0.0 {
            let d = h * pop;
            (h, [(h - d) / 2.0, (h - d) / 2.0, d, d], ((d - h) / 2.0).max(0.0).ceil())
        } else {
            let pw = h + (w - h).max(0.0) * unroll;
            (pw, [0.0, 0.0, pw, h], 0.0)
        };
        Self { box_w, box_h: h, margin, pill, full_w: w, unroll, pop, alpha: frame.alpha, content_alpha: 1.0, title_alpha: 1.0, icon_alpha: 1.0 }
    }

    /// The window's size in whole pixels.
    pub fn window(&self) -> (i32, i32) {
        let m = 2.0 * self.margin;
        ((self.box_w.ceil() + m).max(1.0) as i32, (self.box_h.ceil() + m).max(1.0) as i32)
    }
}

/// Where the mark is drawn, in window coordinates, and its scale: it rides
/// the pill's left end (centred in the circle) and settles into its place in
/// the content as the pill unrolls. `content_w` / `icon_cx` come from the
/// scene (the content is centred in the open pill).
pub fn icon_pose(look: &Look, content_w: f32, icon_cx: f32) -> (f32, f32, f32) {
    let [x, y, _, h] = look.pill;
    let settled = (look.full_w - content_w) / 2.0 + icon_cx;
    let offset = h / 2.0 + (settled - h / 2.0) * look.unroll;
    (look.margin + x + offset, look.margin + y + h / 2.0, look.pop)
}

#[cfg(test)]
#[path = "presence_tests.rs"]
mod tests;
