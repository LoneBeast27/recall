//! The two-step shortcut hint (Alt + ` then a key): a card that unfolds
//! below the island, each option a key cap and its label. The island pill
//! above stays what it is (the stats, a notice, or a small "Alt `" pill).
//! Its layout follows the room the HUD scan measured under the island, card
//! by card over the columns each covers (HUD that comes and goes counted):
//! one option per row (drop-down) if that fits, else two columns, else one
//! row (the shortest; also when nothing fits). Unscanned spots drop down.

use serde::Serialize;
use std::time::{Duration, Instant};

use crate::anim::ease_out;
use crate::layout::{Measure, Role, Text, TEXT};

/// The card's unfold (and fold) time: it answers a key press.
pub const UNFOLD: Duration = Duration::from_millis(120);
/// Sizes in 1440p px (× the island scale).
pub const PAD: f32 = 12.0;
pub const ROW_H: f32 = 28.0;
pub const ROW_GAP: f32 = 4.0;
pub const KEY: f32 = 22.0;
pub const KEY_R: f32 = 6.0;
pub const KEY_GAP: f32 = 10.0;
pub const COL_GAP: f32 = 24.0;
pub const RADIUS: f32 = 16.0;
/// Between the island pill and the card.
pub const GAP: f32 = 6.0;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HintOption {
    pub key: String,
    pub label: String,
}

/// The chord event's options while its window is open.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HintView {
    /// "Alt `": the island's small pill when nothing else shows.
    pub leader: String,
    pub options: Vec<HintOption>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CardLayout {
    /// One option per row.
    Dropdown,
    /// The first half (rounded up) left, the rest right.
    TwoColumn,
    /// Every option side by side (the shortest).
    Row,
}

impl CardLayout {
    fn rows(self, n: usize) -> usize {
        match self {
            CardLayout::Dropdown => n,
            CardLayout::TwoColumn => n.div_ceil(2),
            CardLayout::Row => 1,
        }
        .max(1)
    }

    /// The card's height for `n` options at scale `s`.
    pub fn height(self, n: usize, s: f32) -> f32 {
        let rows = self.rows(n) as f32;
        (2.0 * PAD + rows * ROW_H + (rows - 1.0) * ROW_GAP) * s
    }
}

/// A key cap: a rounded square in the track tone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KeyCap {
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub r: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CardScene {
    pub w: f32,
    pub h: f32,
    pub radius: f32,
    pub layout: CardLayout,
    pub keys: Vec<KeyCap>,
    pub texts: Vec<Text>,
}

/// Places `text` with its cap height centred on `cy`.
fn centred(m: &dyn Measure, role: Role, s: f32, text: &str, x: f32, cy: f32) -> Text {
    let metrics = m.measure(role, s, text);
    Text { role, text: text.into(), x, y: cy + metrics.cap / 2.0 - metrics.baseline, color: TEXT }
}

/// The card for `hint` in `layout` at scale `s`.
pub fn card_scene(hint: &HintView, layout: CardLayout, s: f32, m: &dyn Measure) -> CardScene {
    let u = |v: f32| v * s;
    let rows = layout.rows(hint.options.len());
    let mut scene = CardScene { w: 0.0, h: layout.height(hint.options.len(), s), radius: u(RADIUS), layout, keys: Vec::new(), texts: Vec::new() };
    let mut x = u(PAD);
    for column in hint.options.chunks(rows) {
        let mut label_w: f32 = 0.0;
        for (row, option) in column.iter().enumerate() {
            let top = u(PAD) + row as f32 * u(ROW_H + ROW_GAP);
            let cy = top + u(ROW_H) / 2.0;
            scene.keys.push(KeyCap { x, y: cy - u(KEY) / 2.0, size: u(KEY), r: u(KEY_R) });
            let key_w = m.measure(Role::Key, s, &option.key).width;
            scene.texts.push(centred(m, Role::Key, s, &option.key, x + (u(KEY) - key_w) / 2.0, cy));
            scene.texts.push(centred(m, Role::Option, s, &option.label, x + u(KEY + KEY_GAP), cy));
            label_w = label_w.max(m.measure(Role::Option, s, &option.label).width);
        }
        x += u(KEY + KEY_GAP) + label_w + u(COL_GAP);
    }
    scene.w = (x - u(COL_GAP) + u(PAD)).ceil();
    scene
}

/// The first card (drop-down, then two columns, then one row) that fits the
/// room under it - `room(card_w)`: physical px under a card that wide, `None`
/// when unscanned (anything fits) - else the single row, the shortest.
pub fn choose_card(hint: &HintView, s: f32, m: &dyn Measure, room: impl Fn(f32) -> Option<f32>) -> CardScene {
    let mut cards: Vec<CardScene> =
        [CardLayout::Dropdown, CardLayout::TwoColumn, CardLayout::Row].into_iter().map(|layout| card_scene(hint, layout, s, m)).collect();
    let pick = cards.iter().position(|card| room(card.w).is_none_or(|r| card.h <= r)).unwrap_or(2);
    cards.swap_remove(pick)
}

/// The card's unfold: open (or folding) since when, from where.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Unfold {
    from: f32,
    at: Instant,
    opening: bool,
}

impl Unfold {
    pub fn closed(now: Instant) -> Self {
        Self { from: 0.0, at: now, opening: false }
    }

    fn raw(&self, now: Instant) -> f32 {
        let moved = now.saturating_duration_since(self.at).as_secs_f32() / UNFOLD.as_secs_f32();
        if self.opening { (self.from + moved).min(1.0) } else { (self.from - moved).max(0.0) }
    }

    /// Open or fold, from wherever the card is now.
    pub fn set(&mut self, open: bool, now: Instant) {
        if open != self.opening {
            *self = Self { from: self.raw(now), at: now, opening: open };
        }
    }

    /// 0..1 of the card's height shown (eased); `None` once folded away.
    pub fn progress(&self, now: Instant) -> Option<f32> {
        let p = self.raw(now);
        (self.opening || p > 0.0).then(|| ease_out(p))
    }

    pub fn moving(&self, now: Instant) -> bool {
        let p = self.raw(now);
        if self.opening { p < 1.0 } else { p > 0.0 }
    }
}

#[cfg(test)]
#[path = "hint_tests.rs"]
mod tests;
