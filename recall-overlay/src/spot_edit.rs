//! The island editor: the user drags the pill where they want it for the
//! game in front, and Dynamic mode uses that spot for the game from then on.
//! The spot is kept per exe stem (never a window title) in `island-hud.json`
//! as the pill's centre in monitor fractions plus the edge it hugs.
//!
//! Keys while editing (the overlay holds them only while editing):
//! Enter or the editor shortcut saves, Esc cancels (the old spot stays),
//! R clears the game's spot (back to automatic) and leaves. The editor also
//! leaves without saving after [`IDLE`] without input or when the game
//! loses focus.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::placement::{Anchor, Rect, Spot};

/// The edge a placed island hugs, or none (it sits where it was dropped).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
    Free,
}

/// A game's placed island: the pill's centre as fractions of its monitor
/// (0..1 from the left / top), and the edge it hugs.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedSpot {
    pub x: f32,
    pub y: f32,
    pub edge: Edge,
}

impl SavedSpot {
    pub fn sane(&self) -> bool {
        (0.0..=1.0).contains(&self.x) && (0.0..=1.0).contains(&self.y)
    }

    /// The anchor whose look the spot takes: a side edge stacks the island
    /// vertically, the bottom grows it upwards, a free spot looks like the top.
    pub fn anchor(&self) -> Anchor {
        match self.edge {
            Edge::Top | Edge::Free => Anchor::TopCentre,
            Edge::Bottom => Anchor::BottomCentre,
            Edge::Left => Anchor::LeftMiddle,
            Edge::Right => Anchor::RightMiddle,
        }
    }
}

/// How close (1440p px, times the island scale) a dragged pill's edge comes
/// to the monitor's before it snaps to that edge.
pub const SNAP: f32 = 48.0;
/// The editor leaves without saving after this long without input.
pub const IDLE: Duration = Duration::from_secs(60);

/// The spot for a pill centred at `(cx, cy)` (screen px) whose shown box is
/// `(w, h)`, on `monitor`: the nearest edge when the box comes within
/// `snap` px of it, else free.
pub fn snap((cx, cy): (f32, f32), (w, h): (f32, f32), monitor: &Rect, snap: f32) -> SavedSpot {
    let (mw, mh) = ((monitor.right - monitor.left).max(1) as f32, monitor.height().max(1) as f32);
    let (left, top) = (monitor.left as f32, monitor.top as f32);
    let gaps = [
        (Edge::Left, cx - w / 2.0 - left),
        (Edge::Right, left + mw - (cx + w / 2.0)),
        (Edge::Top, cy - h / 2.0 - top),
        (Edge::Bottom, top + mh - (cy + h / 2.0)),
    ];
    let (edge, gap) = gaps.into_iter().min_by(|a, b| a.1.total_cmp(&b.1)).unwrap_or((Edge::Free, f32::MAX));
    let edge = if gap <= snap { edge } else { Edge::Free };
    SavedSpot { x: ((cx - left) / mw).clamp(0.0, 1.0), y: ((cy - top) / mh).clamp(0.0, 1.0), edge }
}

/// The box sizes of a drawn look: `along` its edge and `depth` into the
/// screen (its unturned `span_w()` and `extent()`), the pill's own depth
/// (`box_h`, the card left out) and the window `margin`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LookBox {
    pub along: f32,
    pub depth: f32,
    pub pill: f32,
    pub margin: f32,
}

/// The window's top-left for `spot` on `monitor`, kept inside `area`: the
/// box hugs the spot's edge `edge` px in, centred on the saved fraction
/// along it; a free spot centres the pill on both.
pub fn origin(spot: &SavedSpot, monitor: &Rect, area: &Rect, b: LookBox, edge: i32) -> (i32, i32) {
    let (mw, mh) = ((monitor.right - monitor.left) as f32, monitor.height() as f32);
    let (cx, cy) = (monitor.left as f32 + spot.x * mw, monitor.top as f32 + spot.y * mh);
    let e = edge as f32;
    let (x, y, w, h) = match spot.edge {
        Edge::Top => (cx - b.along / 2.0, area.top as f32 + e, b.along, b.depth),
        Edge::Bottom => (cx - b.along / 2.0, area.bottom as f32 - e - b.depth, b.along, b.depth),
        Edge::Left => (area.left as f32 + e, cy - b.along / 2.0, b.depth, b.along),
        Edge::Right => (area.right as f32 - e - b.depth, cy - b.along / 2.0, b.depth, b.along),
        Edge::Free => (cx - b.along / 2.0, cy - b.pill / 2.0, b.along, b.depth),
    };
    let clamp = |v: f32, lo: i32, hi: i32, size: f32| v.min(hi as f32 - size).max(lo as f32);
    let (x, y) = (clamp(x, area.left, area.right, w), clamp(y, area.top, area.bottom, h));
    let m = b.margin.ceil() as i32;
    (x.round() as i32 - m, y.round() as i32 - m)
}

/// Dynamic placement with a saved spot: it wins over the HUD pick
/// (`choose`, never asked then); without one, `choose` decides.
pub fn dynamic_spot(saved: Option<SavedSpot>, choose: impl FnOnce() -> Spot) -> (Spot, Option<SavedSpot>) {
    match saved {
        Some(spot) => (Spot { anchor: spot.anchor(), free_px: None }, Some(spot)),
        None => (choose(), None),
    }
}

/// A key the editor holds while it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKey {
    /// Enter, or the editor shortcut again.
    Save,
    /// Esc.
    Cancel,
    /// R: back to automatic.
    Reset,
}

/// One editing run for one game.
#[derive(Clone, Debug, PartialEq)]
pub struct Editor {
    /// The game's exe-stem key.
    pub key: String,
    /// The spot being edited (`None`: automatic until the first drag).
    pub spot: Option<SavedSpot>,
    /// Cursor minus the pill's centre (screen px) while dragging.
    grab: Option<(f32, f32)>,
    last_input: Instant,
}

impl Editor {
    /// Starts on the game's saved spot (`None`: automatic).
    pub fn new(key: &str, saved: Option<SavedSpot>, now: Instant) -> Self {
        Self { key: key.to_string(), spot: saved, grab: None, last_input: now }
    }

    pub fn dragging(&self) -> bool {
        self.grab.is_some()
    }

    /// The button went down at `cursor` on a pill centred at `centre`.
    pub fn press(&mut self, cursor: (f32, f32), centre: (f32, f32), now: Instant) {
        self.grab = Some((cursor.0 - centre.0, cursor.1 - centre.1));
        self.last_input = now;
    }

    /// The cursor moved while dragging: the spot follows (snapping near an
    /// edge). True when the spot changed.
    pub fn drag(&mut self, cursor: (f32, f32), shown: (f32, f32), monitor: &Rect, snap_px: f32, now: Instant) -> bool {
        let Some((gx, gy)) = self.grab else { return false };
        self.last_input = now;
        let next = Some(snap((cursor.0 - gx, cursor.1 - gy), shown, monitor, snap_px));
        let changed = next != self.spot;
        self.spot = next;
        changed
    }

    pub fn release(&mut self, now: Instant) {
        self.grab = None;
        self.last_input = now;
    }

    /// What a key does: `Some(write)` leaves and writes `write` as the
    /// game's spot (`None` clears it); `None` leaves and writes nothing.
    pub fn finish(&self, key: EditKey) -> Option<Option<SavedSpot>> {
        match key {
            EditKey::Save => Some(self.spot),
            EditKey::Cancel => None,
            EditKey::Reset => Some(None),
        }
    }

    /// Leave without saving: idle too long (never mid-drag).
    pub fn timed_out(&self, now: Instant) -> bool {
        !self.dragging() && now.saturating_duration_since(self.last_input) >= IDLE
    }
}

/// The spot the island uses now: the one being edited while the editor
/// runs, else the saved one.
pub fn effective(editor: Option<&Editor>, saved: Option<SavedSpot>) -> Option<SavedSpot> {
    match editor {
        Some(editor) => editor.spot,
        None => saved,
    }
}

#[cfg(test)]
#[path = "spot_edit_tests.rs"]
mod tests;
