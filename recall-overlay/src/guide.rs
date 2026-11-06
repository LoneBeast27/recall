//! The island editor's guide grid (pure): what is drawn over the placement
//! area while the editor runs, the centre-line snap, the pill's position
//! readout, and what a change needs redrawn. The window side is
//! `guide_window.rs`; nothing here exists outside the editor.
//!
//! The grid covers the area the island is placed in (`Placed::area`), in
//! that window's own pixels (0,0 = the area's top-left). Sizes are 1440p px
//! times the island scale.

use crate::layout::{Measure, Rgba, Role, PILL, TEXT};
use crate::placement::{Rect, EDGE};
use crate::spot_edit::{Edge, SavedSpot};

/// The faint grid: every 1/`DIVS` of the width and the height.
pub const DIVS: i32 = 8;
pub const GRID: Rgba = Rgba { rgb: 0xffffff, a: 0.12 };
pub const CENTRE: Rgba = Rgba { rgb: 0xffffff, a: 0.45 };
/// A centre line the pill's centre sits on.
pub const ACCENT: Rgba = Rgba { rgb: 0x8ab4f8, a: 0.95 };
pub const MARGIN: Rgba = Rgba { rgb: 0xffffff, a: 0.22 };
pub const LABEL: Rgba = Rgba { rgb: 0xffffff, a: 0.6 };
/// The readout's role and the room around its text (1440p px).
pub const READOUT_ROLE: Role = Role::NoticeUi;
const READOUT_PAD: (f32, f32) = (8.0, 4.0);
/// The readout's gap from the pill (1440p px).
const READOUT_GAP: f32 = 8.0;

/// Which centre lines the pill's centre is on: `vertical` = the line down
/// the middle (x at 50%), `horizontal` = the one across (y at 50%).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Lines {
    pub vertical: bool,
    pub horizontal: bool,
}

fn centre_of(area: &Rect) -> (f32, f32) {
    ((area.left + area.right) as f32 / 2.0, (area.top + area.bottom) as f32 / 2.0)
}

/// A dragged pill centred at `(cx, cy)` (screen px) snaps its centre onto
/// the area's centre lines when it comes within `snap` px of them, the same
/// distance edges snap at. Returns the centre to use and the lines it is on.
pub fn centre_snap((cx, cy): (f32, f32), area: &Rect, snap: f32) -> ((f32, f32), Lines) {
    let (mx, my) = centre_of(area);
    let lines = Lines { vertical: (cx - mx).abs() <= snap, horizontal: (cy - my).abs() <= snap };
    ((if lines.vertical { mx } else { cx }, if lines.horizontal { my } else { cy }), lines)
}

/// The centre lines a placed `spot` sits on. An edge spot only moves along
/// its edge, so only that axis counts (a top pill can be on the vertical
/// centre line, never on the horizontal one).
pub fn lines_of(spot: Option<SavedSpot>, monitor: &Rect, area: &Rect) -> Lines {
    let Some(spot) = spot else { return Lines::default() };
    let (mw, mh) = ((monitor.right - monitor.left) as f32, monitor.height() as f32);
    let (cx, cy) = (monitor.left as f32 + spot.x * mw, monitor.top as f32 + spot.y * mh);
    let (mx, my) = centre_of(area);
    let (along_x, along_y) = match spot.edge {
        Edge::Top | Edge::Bottom => (true, false),
        Edge::Left | Edge::Right => (false, true),
        Edge::Free => (true, true),
    };
    Lines { vertical: along_x && (cx - mx).abs() < 0.5, horizontal: along_y && (cy - my).abs() < 0.5 }
}

/// The guide window's screen top-left and size: exactly the placement area.
pub fn frame(area: &Rect) -> ((i32, i32), (i32, i32)) {
    ((area.left, area.top), ((area.right - area.left).max(1), (area.bottom - area.top).max(1)))
}

/// The readout's text for a pill centred at `(cx, cy)` (screen px), as
/// percentages of the area: `x 50% · y 12%`.
pub fn readout((cx, cy): (f32, f32), area: &Rect) -> String {
    let pct = |v: f32, lo: i32, hi: i32| ((v - lo as f32) / (hi - lo).max(1) as f32 * 100.0).round().clamp(0.0, 100.0) as i32;
    format!("x {}% · y {}%", pct(cx, area.left, area.right), pct(cy, area.top, area.bottom))
}

/// One straight stroke (window px).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Line {
    pub from: (f32, f32),
    pub to: (f32, f32),
    pub width: f32,
    pub color: Rgba,
    pub dashed: bool,
}

/// A text run: its layout box top-left, and an optional plate behind it.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub text: String,
    pub role: Role,
    pub x: f32,
    pub y: f32,
    pub color: Rgba,
    /// A rounded plate `[l, t, r, b]` drawn under the text.
    pub plate: Option<[f32; 4]>,
}

/// Everything the guide window draws.
#[derive(Clone, Debug, PartialEq)]
pub struct GuideScene {
    pub w: i32,
    pub h: i32,
    pub scale: f32,
    pub lines: Vec<Line>,
    /// The edge gap's outline `[l, t, r, b]`.
    pub margin: [f32; 4],
    pub labels: Vec<Label>,
    /// The readout (also the last of `labels`), kept apart for dirty rects.
    pub readout: Option<Label>,
}

/// The pill's box (screen px `[l, t, r, b]`) as the readout needs it.
pub type PillBox = [i32; 4];

/// The scene for an area of `w`×`h` px at island `scale`: the faint grid,
/// the two centre lines (dashed; solid accent when `lines` says the pill is
/// on one), their `50%` labels, the edge gap's outline, and the readout
/// under (or over, near the bottom) the pill at `pill` (window px).
pub fn scene(w: i32, h: i32, scale: f32, lines: Lines, pill: Option<(PillBox, String)>, m: &dyn Measure) -> GuideScene {
    let (fw, fh) = (w as f32, h as f32);
    let thin = 1.0f32;
    let bold = (2.0 * scale).round().max(2.0);
    // A 1 px line sits on a pixel centre so it stays crisp.
    let crisp = |v: f32| v.floor() + 0.5;
    let mut strokes = Vec::new();
    for i in (1..DIVS).filter(|&i| i * 2 != DIVS) {
        let x = crisp(fw * i as f32 / DIVS as f32);
        let y = crisp(fh * i as f32 / DIVS as f32);
        strokes.push(Line { from: (x, 0.0), to: (x, fh), width: thin, color: GRID, dashed: false });
        strokes.push(Line { from: (0.0, y), to: (fw, y), width: thin, color: GRID, dashed: false });
    }
    let centre = |hot: bool| if hot { (ACCENT, false) } else { (CENTRE, true) };
    let (vc, vd) = centre(lines.vertical);
    let (hc, hd) = centre(lines.horizontal);
    strokes.push(Line { from: (fw / 2.0, 0.0), to: (fw / 2.0, fh), width: bold, color: vc, dashed: vd });
    strokes.push(Line { from: (0.0, fh / 2.0), to: (fw, fh / 2.0), width: bold, color: hc, dashed: hd });
    let e = (EDGE * scale).round();
    let margin = [e + 0.5, e + 0.5, fw - e - 0.5, fh - e - 0.5];
    let gap = 6.0 * scale;
    let tick = |text: &str| m.measure(Role::Small, scale, text);
    let t = tick("50%");
    let label_color = |hot: bool| if hot { ACCENT } else { LABEL };
    let mut labels = vec![
        // Down the middle: beside the line, just inside the bottom margin.
        Label { text: "50%".into(), role: Role::Small, x: fw / 2.0 + gap, y: fh - e - gap - t.height, color: label_color(lines.vertical), plate: None },
        // Across the middle: over the line, just inside the left margin.
        Label { text: "50%".into(), role: Role::Small, x: e + gap, y: fh / 2.0 - gap - t.height, color: label_color(lines.horizontal), plate: None },
    ];
    let readout = pill.map(|(b, text)| readout_label(b, text, (fw, fh), e, scale, m));
    labels.extend(readout.clone());
    GuideScene { w, h, scale, lines: strokes, margin, labels, readout }
}

/// The readout on its plate, centred under the pill (over it when the
/// bottom has no room), kept inside the edge gap.
fn readout_label([l, t, r, b]: PillBox, text: String, (fw, fh): (f32, f32), e: f32, scale: f32, m: &dyn Measure) -> Label {
    let size = m.measure(READOUT_ROLE, scale, &text);
    let (px, py) = (READOUT_PAD.0 * scale, READOUT_PAD.1 * scale);
    let (pw, ph) = (size.width + 2.0 * px, size.height + 2.0 * py);
    let gap = READOUT_GAP * scale;
    let below = b as f32 + gap;
    let top = if below + ph <= fh - e { below } else { (t as f32 - gap - ph).max(e) };
    let cx = (l + r) as f32 / 2.0;
    let left = (cx - pw / 2.0).min(fw - e - pw).max(e);
    let plate = [left.floor(), top.floor(), (left + pw).ceil(), (top + ph).ceil()];
    Label { text, role: READOUT_ROLE, x: left + px, y: top + py, color: TEXT, plate: Some(plate) }
}

/// The readout plate's colour (the pill's own).
pub const PLATE: Rgba = PILL;

/// What a change from `drawn` to `next` needs: nothing, the whole window
/// (first draw, a new size, a centre line lit or dimmed), or only the
/// readout's old and new boxes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Redraw {
    None,
    Full,
    Part([i32; 4]),
}

pub fn plan(drawn: Option<&GuideScene>, next: &GuideScene) -> Redraw {
    let Some(old) = drawn else { return Redraw::Full };
    if (old.w, old.h, old.scale) != (next.w, next.h, next.scale) || old.lines != next.lines || old.labels[..2] != next.labels[..2] {
        return Redraw::Full;
    }
    if old.readout == next.readout {
        return Redraw::None;
    }
    let plate = |l: &Option<Label>| l.as_ref().and_then(|l| l.plate);
    let union = match (plate(&old.readout), plate(&next.readout)) {
        (Some(a), Some(b)) => [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])],
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => return Redraw::None,
    };
    // One px of room for the plate's anti-aliased edge, kept inside the window.
    let [l, t, r, b] = union;
    Redraw::Part([(l as i32 - 1).max(0), (t as i32 - 1).max(0), (r as i32 + 1).min(next.w), (b as i32 + 1).min(next.h)])
}

#[cfg(test)]
#[path = "guide_tests.rs"]
mod tests;
