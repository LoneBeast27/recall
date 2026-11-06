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

/// The shortcut hint's card under the pill: its size, the gap above it and
/// how much of it has unfolded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CardLook {
    pub w: f32,
    pub h: f32,
    pub gap: f32,
    pub progress: f32,
}

impl CardLook {
    /// The card of a `size` (w, h) card scene at a spot turned `turn`: a side
    /// spot's card stands upright beside the pill (`h` along the edge, `w` across).
    pub fn placed(turn: Turn, (w, h): (f32, f32), gap: f32, progress: f32) -> Self {
        let (w, h) = if turn == Turn::None { (w, h) } else { (h, w) };
        Self { w, h, gap, progress }
    }
}

/// Where `scene`'s content sits in the pill (`content_w` as drawn, scaled):
/// centred, or at the left padding for a scene that leads.
pub fn content_dx(look: &Look, content_w: f32, lead: bool) -> f32 {
    let pill_w = if look.unroll < 1.0 { look.full_w } else { look.pill[2] };
    if lead { 0.0 } else { (pill_w - content_w) / 2.0 }
}

/// The scale a scene laid out `scene_h` high draws at: it rides the pill's
/// height through a transformation, so old and new content move as one.
/// Exactly 1 once settled, so text stays on its layout's pixels.
pub fn content_scale(look: &Look, scene_h: f32) -> f32 {
    let k = look.box_h / scene_h.max(1.0);
    if (k - 1.0).abs() < 0.003 { 1.0 } else { k }
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
    /// The detail line's opacity (≤ `content_alpha`; fades alone when only it changed).
    pub detail_alpha: f32,
    pub icon_alpha: f32,
    /// The shortcut hint's card, while it is out.
    pub card: Option<CardLook>,
    /// How the pill and the card line up in the window, as the anchor:
    /// 0 = left edges, 0.5 = centres, 1 = right edges.
    pub align: f32,
    /// The card stacks above the pill (a bottom spot): the pill keeps the
    /// window's bottom and the card unfolds upwards from it.
    pub up: bool,
    /// A side spot lays the pill down its edge (the window is turned);
    /// text and marks stay upright ([`crate::stack`]). Every size here
    /// stays in the unturned layout.
    pub turn: Turn,
}

/// A side spot's pill runs down its edge: `Right` / `Left` edge. The
/// pill's start (the mark) is at the top on both; nothing is rotated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Turn {
    #[default]
    None,
    Right,
    Left,
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
        Self { box_w, box_h: h, margin, pill, full_w: w, unroll, pop, alpha: frame.alpha, content_alpha: 1.0, title_alpha: 1.0, detail_alpha: 1.0, icon_alpha: 1.0, card: None, align: 1.0, up: false, turn: Turn::None }
    }

    /// The width the anchor places: the pill's box, or the card when wider.
    pub fn span_w(&self) -> f32 {
        self.card.map_or(self.box_w, |c| self.box_w.max(c.w))
    }

    /// How far the unfolded card reaches past the pill (its gap included).
    fn card_reach(&self) -> f32 {
        self.card.map_or(0.0, |c| c.gap + c.h * c.progress)
    }

    /// The depth the look takes from the edge it sits on: the box and the
    /// unfolded card (no margin).
    pub fn extent(&self) -> f32 {
        self.box_h + self.card_reach()
    }

    /// The window's size in whole pixels, unturned: the box (and the
    /// unfolded card below it, or above it when `up`) plus the margin.
    pub fn window(&self) -> (i32, i32) {
        let m = 2.0 * self.margin;
        ((self.span_w().ceil() + m).max(1.0) as i32, (self.extent().ceil() + m).max(1.0) as i32)
    }

    /// The window's size as shown: a side spot's window is turned.
    pub fn shown_window(&self) -> (i32, i32) {
        let (w, h) = self.window();
        match self.turn {
            Turn::None => (w, h),
            Turn::Right | Turn::Left => (h, w),
        }
    }

    /// The pill box's top-left in the (unturned) window.
    pub fn pill_origin(&self) -> (f32, f32) {
        let y = if self.up { self.margin + self.card_reach() } else { self.margin };
        (self.margin + (self.span_w() - self.box_w) * self.align, y)
    }

    /// The card's top-left in the (unturned) window: under the pill, or with
    /// `up` its bottom `gap` above the pill (it unfolds upwards).
    pub fn card_origin(&self) -> Option<(f32, f32)> {
        self.card.map(|c| {
            let x = self.margin + (self.span_w() - c.w) * self.align;
            let y = if self.up { self.margin + self.card_reach() - c.gap - c.h } else { self.margin + self.box_h + c.gap };
            (x, y)
        })
    }
}

/// Where the mark is drawn, in window coordinates, and its scale: it rides
/// the pill's left end (centred in the circle) and settles into its place in
/// the content as the pill unrolls. `content_dx` is where the content sits
/// in the open pill (`content_dx`), `icon_cx` the mark's place in it.
pub fn icon_pose(look: &Look, content_dx: f32, icon_cx: f32) -> (f32, f32, f32) {
    let ([x, y, _, h], (ox, oy)) = (look.pill, look.pill_origin());
    let settled = content_dx + icon_cx;
    let offset = h / 2.0 + (settled - h / 2.0) * look.unroll;
    (ox + x + offset, oy + y + h / 2.0, look.pop)
}

#[cfg(test)]
#[path = "presence_tests.rs"]
mod tests;
