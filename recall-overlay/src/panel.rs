//! The island editor's panel (pure): a small window of buttons that opens
//! beside the guide grid while the editor runs. It holds the island's size
//! for the game (75% to 150%), one-click places (the four edges and the
//! centre) and the editor's ways out (Auto, Cancel, Done). The window side
//! is `panel_window.rs`, the drawing `panel_render.rs`.
//!
//! Sizes are 1440p px times the monitor's own scale (never the island size
//! the panel sets, so the panel does not grow under the cursor).

use crate::layout::{Measure, Rgba, Role, MUTED, TERTIARY, TEXT};
use crate::placement::{Rect, EDGE};
use crate::spot_edit::{Edge, SavedSpot};

/// The island size range and step, in percent of the automatic size.
pub const MIN_PCT: i32 = 75;
pub const MAX_PCT: i32 = 150;
pub const STEP_PCT: i32 = 5;

/// A saved island size this build can use (anything else counts as 100%).
pub fn sane_size(size: f32) -> bool {
    size.is_finite() && (MIN_PCT as f32 / 100.0 - 0.001..=MAX_PCT as f32 / 100.0 + 0.001).contains(&size)
}

/// The size in whole percent (100 for an unusable one).
pub fn size_pct(size: f32) -> i32 {
    if sane_size(size) { (size * 100.0).round() as i32 } else { 100 }
}

/// One step smaller or larger, on the [`STEP_PCT`] grid, kept in range.
pub fn step_size(size: f32, larger: bool) -> f32 {
    let pct = size_pct(size);
    // The next grid value up, or down (a size off the grid lands on it first).
    let next = if larger { (pct / STEP_PCT + 1) * STEP_PCT } else { ((pct + STEP_PCT - 1) / STEP_PCT - 1) * STEP_PCT };
    next.clamp(MIN_PCT, MAX_PCT) as f32 / 100.0
}

/// What a panel button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Smaller,
    Larger,
    /// The size back to 100%.
    SizeReset,
    /// The pill on that edge, centred along it; `Edge::Free` = the centre.
    Place(Edge),
    /// Leave with the spot cleared (automatic placement); the size stays.
    Auto,
    /// Leave with the spot and size the game had before the editor.
    Cancel,
    /// Leave; what was placed and sized is kept.
    Done,
}

/// The spot for a one-click place on `edge`: centred along that edge of
/// `area` (the centre of `area` for `Edge::Free`), in `monitor` fractions.
pub fn placed_spot(edge: Edge, monitor: &Rect, area: &Rect) -> SavedSpot {
    let (mw, mh) = ((monitor.right - monitor.left).max(1) as f32, monitor.height().max(1) as f32);
    let fx = |x: i32| ((x - monitor.left) as f32 / mw).clamp(0.0, 1.0);
    let fy = |y: i32| ((y - monitor.top) as f32 / mh).clamp(0.0, 1.0);
    let (cx, cy) = (fx((area.left + area.right) / 2), fy((area.top + area.bottom) / 2));
    let (x, y) = match edge {
        Edge::Top => (cx, fy(area.top)),
        Edge::Bottom => (cx, fy(area.bottom)),
        Edge::Left => (fx(area.left), cy),
        Edge::Right => (fx(area.right), cy),
        Edge::Free => (cx, cy),
    };
    SavedSpot { x, y, edge }
}

/// How a button looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Plain,
    /// The place the pill is on now.
    Active,
    /// The main way out (Done).
    Accent,
    /// Nothing to do (the size at its end): drawn dim, never hit.
    Disabled,
}

pub const PANEL: Rgba = Rgba { rgb: 0x121212, a: 0.96 };
pub const OUTLINE: Rgba = Rgba { rgb: 0xffffff, a: 0.10 };
const ACCENT_RGB: u32 = 0x8ab4f8;

impl Tone {
    /// The plate's and the text's colours.
    pub fn colors(self) -> (Rgba, Rgba) {
        match self {
            Tone::Plain => (Rgba { rgb: 0xffffff, a: 0.08 }, TEXT),
            Tone::Active => (Rgba { rgb: ACCENT_RGB, a: 0.26 }, TEXT),
            Tone::Accent => (Rgba { rgb: ACCENT_RGB, a: 0.92 }, Rgba { rgb: 0x121212, a: 1.0 }),
            Tone::Disabled => (Rgba { rgb: 0xffffff, a: 0.04 }, TERTIARY),
        }
    }
}

/// A text run's layout box top-left (window px).
#[derive(Clone, Debug, PartialEq)]
pub struct Text {
    pub text: String,
    pub role: Role,
    pub x: f32,
    pub y: f32,
    pub color: Rgba,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Button {
    pub action: Action,
    /// `[l, t, r, b]` window px.
    pub rect: [f32; 4],
    pub tone: Tone,
    pub label: Text,
}

/// Everything the panel window draws.
#[derive(Clone, Debug, PartialEq)]
pub struct PanelScene {
    pub w: i32,
    pub h: i32,
    pub scale: f32,
    /// The panel's and a button's corner radius.
    pub radius: f32,
    pub button_radius: f32,
    pub texts: Vec<Text>,
    pub buttons: Vec<Button>,
}

impl PanelScene {
    /// The button under `(x, y)` (window px); a disabled one is never hit.
    pub fn hit(&self, (x, y): (f32, f32)) -> Option<Action> {
        self.buttons
            .iter()
            .filter(|b| b.tone != Tone::Disabled)
            .find(|b| x >= b.rect[0] && x < b.rect[2] && y >= b.rect[1] && y < b.rect[3])
            .map(|b| b.action)
    }
}

/// What the panel shows: the game's key, the size, and the edge of the
/// spot shown (`None`: automatic).
#[derive(Clone, Debug, PartialEq)]
pub struct PanelState<'a> {
    pub game: &'a str,
    pub size: f32,
    pub edge: Option<Edge>,
}

// 1440p px.
const PAD: f32 = 14.0;
const ROW_GAP: f32 = 10.0;
const BTN_H: f32 = 30.0;
const BTN_PAD_X: f32 = 12.0;
const BTN_GAP: f32 = 6.0;
const LABEL_GAP: f32 = 12.0;
const TITLE_ROLE: Role = Role::NoticeUi;
const LABEL_ROLE: Role = Role::Option;
const BUTTON_ROLE: Role = Role::Option;

const PLACES: [(Edge, &str); 5] = [(Edge::Top, "Top"), (Edge::Bottom, "Bottom"), (Edge::Left, "Left"), (Edge::Right, "Right"), (Edge::Free, "Centre")];

/// A row being laid out left to right from `x` at `top`.
struct Row<'m> {
    x: f32,
    top: f32,
    k: f32,
    m: &'m dyn Measure,
}

impl Row<'_> {
    /// A button `min_w` px wide at least (its text plus padding otherwise).
    fn button(&mut self, action: Action, text: &str, tone: Tone, min_w: f32) -> Button {
        let size = self.m.measure(BUTTON_ROLE, self.k, text);
        let w = (size.width + 2.0 * BTN_PAD_X * self.k).max(min_w).ceil();
        let h = (BTN_H * self.k).round();
        let rect = [self.x, self.top, self.x + w, self.top + h];
        self.x += w + (BTN_GAP * self.k).round();
        let label = Text { text: text.into(), role: BUTTON_ROLE, x: rect[0] + (w - size.width) / 2.0, y: rect[1] + (h - size.height) / 2.0, color: tone.colors().1 };
        Button { action, rect, tone, label }
    }

    /// Plain text vertically centred in the row, `min_w` wide at least.
    fn text(&mut self, text: &str, role: Role, color: Rgba, min_w: f32) -> Text {
        let size = self.m.measure(role, self.k, text);
        let w = size.width.max(min_w);
        let t = Text { text: text.into(), role, x: self.x + (w - size.width) / 2.0, y: self.top + ((BTN_H * self.k).round() - size.height) / 2.0, color };
        self.x += w + (BTN_GAP * self.k).round();
        t
    }

    fn end(&self) -> f32 {
        self.x - (BTN_GAP * self.k).round()
    }
}

/// The panel laid out at monitor scale `k`.
pub fn scene(state: &PanelState, k: f32, m: &dyn Measure) -> PanelScene {
    let pad = (PAD * k).round();
    let row_h = (BTN_H * k).round();
    let gap = (ROW_GAP * k).round();
    let title_text = if state.game.is_empty() { "Island editor".to_string() } else { format!("Island editor · {}", state.game) };
    let title_size = m.measure(TITLE_ROLE, k, &title_text);
    let label_w = ["Size", "Place"].iter().map(|t| m.measure(LABEL_ROLE, k, t).width).fold(0.0, f32::max).ceil();
    let col = pad + label_w + (LABEL_GAP * k).round();
    let mut texts = vec![Text { text: title_text, role: TITLE_ROLE, x: pad, y: pad, color: TEXT }];
    let mut buttons = Vec::new();

    let size_top = pad + title_size.height.ceil() + gap;
    let pct = size_pct(state.size);
    texts.push(Row { x: pad, top: size_top, k, m }.text("Size", LABEL_ROLE, MUTED, 0.0));
    let mut row = Row { x: col, top: size_top, k, m };
    let square = row_h;
    buttons.push(row.button(Action::Smaller, "−", if pct <= MIN_PCT { Tone::Disabled } else { Tone::Plain }, square));
    let widest = m.measure(BUTTON_ROLE, k, "150%").width.ceil();
    texts.push(row.text(&format!("{pct}%"), BUTTON_ROLE, TEXT, widest));
    buttons.push(row.button(Action::Larger, "+", if pct >= MAX_PCT { Tone::Disabled } else { Tone::Plain }, square));
    buttons.push(row.button(Action::SizeReset, "Reset", if pct == 100 { Tone::Disabled } else { Tone::Plain }, 0.0));
    let mut right = row.end();

    let place_top = size_top + row_h + gap;
    texts.push(Row { x: pad, top: place_top, k, m }.text("Place", LABEL_ROLE, MUTED, 0.0));
    let mut row = Row { x: col, top: place_top, k, m };
    for (edge, text) in PLACES {
        let tone = if state.edge == Some(edge) { Tone::Active } else { Tone::Plain };
        buttons.push(row.button(Action::Place(edge), text, tone, 0.0));
    }
    right = right.max(row.end());

    // The ways out: Auto on the left, Cancel and Done on the right.
    let out_top = place_top + row_h + gap;
    let mut row = Row { x: pad, top: out_top, k, m };
    let auto_tone = if state.edge.is_none() { Tone::Active } else { Tone::Plain };
    buttons.push(row.button(Action::Auto, "⟲ Auto", auto_tone, 0.0));
    let auto_end = row.end();
    let mut probe = Row { x: 0.0, top: out_top, k, m };
    let (cancel, done) = (probe.button(Action::Cancel, "Cancel", Tone::Plain, 0.0), probe.button(Action::Done, "Done", Tone::Accent, 0.0));
    let tail = probe.end();
    right = right.max(auto_end + (BTN_GAP * 2.0 * k).round() + tail).max(pad + title_size.width.ceil());
    let shift = right - tail;
    for mut b in [cancel, done] {
        b.rect[0] += shift;
        b.rect[2] += shift;
        b.label.x += shift;
        buttons.push(b);
    }

    let w = (right + pad).ceil() as i32;
    let h = (out_top + row_h + pad).ceil() as i32;
    PanelScene { w, h, scale: k, radius: (12.0 * k).round(), button_radius: (8.0 * k).round(), texts, buttons }
}

/// The panel's screen top-left: on the far side of `area` from the pill
/// (`pill`, screen `[l, t, r, b]`), along the axis the pill is further out
/// on, a quarter of the area from its centre; always inside the edge gap.
pub fn origin((w, h): (i32, i32), area: &Rect, pill: [i32; 4], k: f32) -> (i32, i32) {
    let (aw, ah) = ((area.right - area.left).max(1) as f32, area.height().max(1) as f32);
    let (mx, my) = (area.left as f32 + aw / 2.0, area.top as f32 + ah / 2.0);
    let (px, py) = ((pill[0] + pill[2]) as f32 / 2.0, (pill[1] + pill[3]) as f32 / 2.0);
    let (dx, dy) = ((px - mx) / aw, (py - my) / ah);
    // Away from the pill; a pill on the centre puts the panel below it.
    let away = |d: f32| if d > 0.0 { -1.0 } else { 1.0 };
    let (cx, cy) = if dy.abs() >= dx.abs() { (mx, my + away(dy) * ah / 4.0) } else { (mx + away(dx) * aw / 4.0, my) };
    let e = EDGE * k;
    let clamp = |v: f32, lo: i32, hi: i32, size: i32| v.min(hi as f32 - e - size as f32).max(lo as f32 + e);
    let x = clamp(cx - w as f32 / 2.0, area.left, area.right, w);
    let y = clamp(cy - h as f32 / 2.0, area.top, area.bottom, h);
    (x.round() as i32, y.round() as i32)
}

#[cfg(test)]
#[path = "panel_tests.rs"]
mod tests;
