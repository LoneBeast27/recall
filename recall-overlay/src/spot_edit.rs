//! The island editor: the user drags the pill where they want it for the
//! game in front, and Dynamic mode uses that spot for the game from then on.
//! The spot is kept per exe stem (never a window title) in `island-hud.json`
//! as the pill's centre in monitor fractions plus the edge it hugs.
//!
//! Mouse only: the editor holds no keyboard keys. Drag the pill; releasing
//! saves the spot for the game. Right-click on the pill cancels (the spot the
//! game had before is put back); the pill's ⟲ button clears the spot (back
//! to automatic). The editor shortcut again, [`IDLE`] without input, [`CAP`]
//! in all, the game losing focus or the mouse capture lost all leave without
//! writing anything more.
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
/// ... and after this long in all, dragging or not.
pub const CAP: Duration = Duration::from_secs(120);

/// The spot for a pill centred at `centre` (screen px) on `monitor`: the
/// edge nearest the point `at` (the cursor while dragging) when that point
/// comes within `snap` px of it, counting half the pill's `depth` (its
/// thickness, the same wide or upright); else free. The pill's own box is
/// not used: it turns upright on a side edge, and in a corner that changed
/// which edge was nearest, so the pill flipped between looks on every move.
/// The cursor decides instead: on the top edge it is a top pill, on a side
/// a side pill, whatever the pill looks like.
pub fn snap_at((cx, cy): (f32, f32), (px, py): (f32, f32), depth: f32, monitor: &Rect, snap: f32) -> SavedSpot {
    let (mw, mh) = ((monitor.right - monitor.left).max(1) as f32, monitor.height().max(1) as f32);
    let (left, top) = (monitor.left as f32, monitor.top as f32);
    let half = depth / 2.0;
    let gaps = [
        (Edge::Top, py - half - top),
        (Edge::Bottom, top + mh - (py + half)),
        (Edge::Left, px - half - left),
        (Edge::Right, left + mw - (px + half)),
    ];
    let (edge, gap) = gaps.into_iter().min_by(|a, b| a.1.total_cmp(&b.1)).unwrap_or((Edge::Free, f32::MAX));
    let edge = if gap <= snap { edge } else { Edge::Free };
    SavedSpot { x: ((cx - left) / mw).clamp(0.0, 1.0), y: ((cy - top) / mh).clamp(0.0, 1.0), edge }
}

/// [`snap_at`] decided by the pill's centre, `shown` = its box `(w, h)`.
#[cfg(test)]
pub fn snap(centre: (f32, f32), (w, h): (f32, f32), monitor: &Rect, snap_px: f32) -> SavedSpot {
    snap_at(centre, centre, w.min(h), monitor, snap_px)
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

/// How the editor ends (none of these is a key: the editor holds no keys).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKey {
    /// Right-click on the pill: the spot the game had before the editor is put back.
    Cancel,
    /// The pill's ⟲ button: the game's spot is cleared (back to automatic).
    Reset,
    /// The editor shortcut again, a timeout, focus loss or the mouse capture
    /// lost: nothing more is written (a drag in progress is dropped; spots
    /// saved by earlier releases stay).
    Leave,
}

/// One editing run for one game.
#[derive(Clone, Debug, PartialEq)]
pub struct Editor {
    /// The game's exe-stem key.
    pub key: String,
    /// The spot shown (`None`: automatic until the first drag).
    pub spot: Option<SavedSpot>,
    /// The game's spot when the editor started (right-click puts it back).
    original: Option<SavedSpot>,
    /// The game's spot on disk now (each release writes it).
    saved: Option<SavedSpot>,
    /// Cursor minus the pill's centre (screen px) while dragging.
    grab: Option<(f32, f32)>,
    last_input: Instant,
    started: Instant,
    /// The island size shown (1.0 = automatic), set on the panel; each
    /// change is written at once.
    pub size: f32,
    /// The game's size when the editor started (Cancel puts it back).
    original_size: f32,
    /// How often a drag moved the pill to another edge (diagnostics: a
    /// pill held in a corner must not flip between looks).
    pub edge_changes: u32,
}

impl Editor {
    /// Starts on the game's saved spot (`None`: automatic).
    pub fn new(key: &str, saved: Option<SavedSpot>, now: Instant) -> Self {
        Self { key: key.to_string(), spot: saved, original: saved, saved, grab: None, last_input: now, started: now, size: 1.0, original_size: 1.0, edge_changes: 0 }
    }

    /// Starts on the game's saved size too.
    pub fn with_size(mut self, size: f32) -> Self {
        (self.size, self.original_size) = (size, size);
        self
    }

    /// Input on the panel: the idle timer starts again.
    pub fn touch(&mut self, now: Instant) {
        self.last_input = now;
    }

    /// A one-click place from the panel: the spot is the game's from now on
    /// (like a drag's release). Returns it, to write.
    pub fn place(&mut self, spot: SavedSpot, now: Instant) -> SavedSpot {
        self.grab = None;
        self.last_input = now;
        self.spot = Some(spot);
        self.saved = self.spot;
        spot
    }

    /// The size to write as the editor ends: Cancel puts the old one back
    /// when it changed; every other way keeps what was set.
    pub fn finish_size(&self, how: EditKey) -> Option<f32> {
        (how == EditKey::Cancel && self.size != self.original_size).then_some(self.original_size)
    }

    pub fn dragging(&self) -> bool {
        self.grab.is_some()
    }

    /// The button went down at `cursor` on a pill centred at `centre`.
    pub fn press(&mut self, cursor: (f32, f32), centre: (f32, f32), now: Instant) {
        self.grab = Some((cursor.0 - centre.0, cursor.1 - centre.1));
        self.last_input = now;
    }

    /// The cursor moved while dragging: the spot follows, its centre
    /// snapping onto the centre lines of `area` (the grid), and the edge the
    /// cursor is near (`snap_at`), both within `snap_px`. True when the spot changed.
    pub fn drag(&mut self, cursor: (f32, f32), shown: (f32, f32), monitor: &Rect, area: &Rect, snap_px: f32, now: Instant) -> bool {
        let Some((gx, gy)) = self.grab else { return false };
        self.last_input = now;
        let (centre, _) = crate::guide::centre_snap((cursor.0 - gx, cursor.1 - gy), area, snap_px);
        let next = Some(snap_at(centre, cursor, shown.0.min(shown.1), monitor, snap_px));
        let changed = next != self.spot;
        if next.map(|s| s.edge) != self.spot.map(|s| s.edge) {
            self.edge_changes += 1;
        }
        self.spot = next;
        changed
    }

    /// The button came up: the dragged spot is the game's from now on.
    /// `Some(spot)` = write it (None when no drag was under way).
    pub fn release(&mut self, now: Instant) -> Option<SavedSpot> {
        self.grab.take()?;
        self.last_input = now;
        self.saved = self.spot;
        self.spot
    }

    /// A drag ended without a release (the capture was lost, the button came
    /// up somewhere the window never heard of): the pill goes back to the
    /// game's saved spot and nothing is written. True when a drag was dropped.
    pub fn drop_drag(&mut self) -> bool {
        if self.grab.take().is_none() {
            return false;
        }
        self.spot = self.saved;
        true
    }

    /// How the editor ends: `Some(write)` writes `write` as the game's spot
    /// (`None` clears it); `None` writes nothing.
    pub fn finish(&self, how: EditKey) -> Option<Option<SavedSpot>> {
        match how {
            EditKey::Cancel => (self.saved != self.original).then_some(self.original),
            EditKey::Reset => Some(None),
            EditKey::Leave => None,
        }
    }

    /// Leave without saving: [`IDLE`] without input (a held, unmoving
    /// button too: a release that never came), or [`CAP`] since it started.
    pub fn timed_out(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_input) >= IDLE || now.saturating_duration_since(self.started) >= CAP
    }

    /// Each tick: `Some(Leave)` when the editor must end. `in_front`: the game
    /// still has the focus; `captured`: the window still holds the mouse
    /// capture (only asked while dragging).
    pub fn watch(&self, now: Instant, in_front: bool, captured: bool) -> Option<EditKey> {
        let lost_capture = self.dragging() && !captured;
        (self.timed_out(now) || !in_front || lost_capture).then_some(EditKey::Leave)
    }
}

/// The window side of the editor: the click-through style, the mouse
/// capture and the guide grid's window. Every exit goes through [`end`], so
/// the window always goes back to click-through and the grid is gone.
pub trait EditWindow {
    fn set_click_through(&mut self, on: bool);
    fn release_capture(&mut self);
    /// Makes (`true`) or destroys (`false`) the guide grid's window.
    fn show_guide(&mut self, on: bool);
}

/// Why the editor may not start, or None. Exclusive fullscreen is refused:
/// the game owns the screen and the mouse there, so nothing can be dragged.
pub fn refusal(dynamic: bool, has_game: bool, in_front: bool, exclusive: bool) -> Option<&'static str> {
    match () {
        _ if !dynamic => Some("the island is not on Dynamic"),
        _ if !has_game => Some("no game target"),
        _ if !in_front => Some("the game is not in front"),
        _ if exclusive => Some("the game runs in exclusive fullscreen"),
        _ => None,
    }
}

/// Starts the editor unless `refused`; a refusal leaves the window untouched.
pub fn begin(window: &mut dyn EditWindow, refused: Option<&'static str>, key: &str, saved: Option<SavedSpot>, now: Instant) -> Result<Editor, &'static str> {
    if let Some(why) = refused {
        return Err(why);
    }
    window.set_click_through(false);
    window.show_guide(true);
    Ok(Editor::new(key, saved, now))
}

/// Ends the editor: the capture is let go, the window is click-through
/// again, the guide grid is destroyed, and the answer is what to write for
/// the game (see [`Editor::finish`]).
pub fn end(window: &mut dyn EditWindow, editor: &mut Editor, how: EditKey) -> Option<Option<SavedSpot>> {
    editor.drop_drag();
    window.release_capture();
    window.set_click_through(true);
    window.show_guide(false);
    editor.finish(how)
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
