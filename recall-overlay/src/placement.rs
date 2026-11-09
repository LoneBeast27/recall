//! Per-game island placement from the HUD scan (`assets/hud-spots.json`):
//! game key → anchor + free top depth at 1440p. Sizes are designed at 1440p
//! and scale by monitor height / 1440; where the scan measured less free depth
//! than the island needs (edge gap + height), the island shrinks to fit.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::presence::Turn;

/// Design reference: sizes are in 1440p pixels.
pub const REF_HEIGHT: f32 = 1440.0;
/// Island height at 1440p.
pub const ISLAND_H: f32 = 44.0;
/// Gap from the screen edges at 1440p.
pub const EDGE: f32 = 12.0;
/// The island never shrinks below this fraction to fit a tight HUD.
pub const MIN_FIT: f32 = 0.6;

/// Where the island sits: the scan's top spots (`top-left`, `top-centre`,
/// `top-right`), the bottom ones the corner setting and the Dynamic pick add,
/// and the middles of the side edges (the island turns vertical there).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Anchor {
    #[serde(rename = "top-left")]
    TopLeft,
    #[serde(rename = "top-centre", alias = "top-center")]
    TopCentre,
    #[serde(rename = "top-right")]
    TopRight,
    #[serde(rename = "bottom-left")]
    BottomLeft,
    #[serde(rename = "bottom-centre", alias = "bottom-center")]
    BottomCentre,
    #[serde(rename = "bottom-right")]
    BottomRight,
    #[serde(rename = "left-middle")]
    LeftMiddle,
    #[serde(rename = "right-middle")]
    RightMiddle,
}

/// The screen edge an anchor sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Top,
    Bottom,
    Left,
    Right,
}

impl Anchor {
    pub fn side(self) -> Side {
        match self {
            Anchor::TopLeft | Anchor::TopCentre | Anchor::TopRight => Side::Top,
            Anchor::BottomLeft | Anchor::BottomCentre | Anchor::BottomRight => Side::Bottom,
            Anchor::LeftMiddle => Side::Left,
            Anchor::RightMiddle => Side::Right,
        }
    }

    /// The quarter turn the look takes here (side edges only).
    pub fn turn(self) -> Turn {
        match self.side() {
            Side::Right => Turn::Right,
            Side::Left => Turn::Left,
            Side::Top | Side::Bottom => Turn::None,
        }
    }

    /// The `--corner` setting's fixed spots; None for `dynamic` (and anything else).
    pub fn from_corner(corner: &str) -> Option<Anchor> {
        match corner {
            "top-left" => Some(Anchor::TopLeft),
            "top-right" => Some(Anchor::TopRight),
            "bottom-left" => Some(Anchor::BottomLeft),
            "bottom-right" => Some(Anchor::BottomRight),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Spot {
    pub anchor: Anchor,
    /// Free rows from the top edge at 1440p; `None` = not scanned, `Some(0)` =
    /// the scan found no free spot (the island may overlap the HUD).
    pub free_px: Option<u32>,
}

/// Unknown games and the desktop.
pub const DEFAULT_SPOT: Spot = Spot { anchor: Anchor::TopRight, free_px: None };

#[derive(Deserialize)]
struct SpotsFile {
    spots: HashMap<String, RawSpot>,
}

#[derive(Deserialize)]
struct RawSpot {
    anchor: Anchor,
    free_px: u32,
    /// Where the HUD sits, HUD that comes and goes counted: one number per
    /// `HUD_COLUMN` px column at 1440p, bit r = the `HUD_ROW` px row r holds
    /// HUD. Only the shortcut hint's card reads it. The island's spot and
    /// scale stay on `anchor` / `free_px` (always-on HUD).
    #[serde(default)]
    hud: Option<Vec<u64>>,
}

/// The HUD grid's cell size at 1440p (its 64 rows reach deeper than any card).
const HUD_COLUMN: f32 = 32.0;
const HUD_ROW: f32 = 8.0;
/// Design reference width (16:9 at 1440p).
pub const REF_WIDTH: f32 = 2560.0;

/// Game keys of exe stems that do not start with their scanned key. A window
/// with an empty title goes out under its exe stem (`display_name` in recall-hdr).
const EXE_KEYS: [(&str, &str); 9] = [
    ("r5apex", "apexlegends"),
    ("r5apexdx12", "apexlegends"),
    ("marvelwin64shipping", "marvelrivals"),
    ("bf6", "battlefield6"),
    ("cs2", "counterstrike2"),
    ("rainbowsix", "tomclancysrainbowsixsiege"),
    ("rainbowsixvulkan", "tomclancysrainbowsixsiege"),
    ("csgo", "counterstrikeglobaloffensive"),
    ("forzagamingdesktopx64releasefinal", "forzahorizon6"),
];

#[derive(Debug, Default)]
pub struct Spots {
    spots: HashMap<String, Spot>,
    huds: HashMap<String, Vec<u64>>,
}

impl Spots {
    /// The table compiled into the binary.
    pub fn embedded() -> Self {
        Self::parse(include_str!("../assets/hud-spots.json")).unwrap_or_default()
    }

    pub fn parse(json: &str) -> Result<Self, String> {
        let file: SpotsFile = serde_json::from_str(json).map_err(|e| format!("hud-spots.json: {e}"))?;
        let mut spots = Self::default();
        for (key, raw) in file.spots {
            if let Some(hud) = raw.hud {
                spots.huds.insert(key.clone(), hud);
            }
            spots.spots.insert(key, Spot { anchor: raw.anchor, free_px: Some(raw.free_px) });
        }
        Ok(spots)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.spots.len()
    }

    #[cfg(test)]
    pub fn keys(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self.spots.keys().map(String::as_str).collect();
        keys.sort();
        keys
    }

    /// The scanned key a game key matches: exact first, then the longest
    /// scanned key the target's key starts with (window titles carry
    /// suffixes: "Rocket League (64-bit, DX11, Cooked)"), then the exe stem
    /// of a window with an empty title.
    fn matched(&self, key: &str) -> Option<&str> {
        if let Some((scanned, _)) = self.spots.get_key_value(key) {
            return Some(scanned.as_str());
        }
        self.spots
            .keys()
            .filter(|scanned| !scanned.is_empty() && key.starts_with(scanned.as_str()))
            .max_by_key(|scanned| scanned.len())
            .map(String::as_str)
            .or_else(|| {
                let (_, scanned) = EXE_KEYS.iter().find(|(exe, _)| *exe == key)?;
                self.spots.get_key_value(*scanned).map(|(scanned, _)| scanned.as_str())
            })
    }

    /// The spot for a game key; unknown / empty keys use the top-right default.
    pub fn lookup(&self, key: &str) -> Spot {
        self.matched(key).map_or(DEFAULT_SPOT, |scanned| self.spots[scanned])
    }

    /// The game's HUD grid, when the scan measured one (for the hint card).
    pub fn hud(&self, key: &str) -> Option<Vec<u64>> {
        self.matched(key).and_then(|scanned| self.huds.get(scanned).cloned())
    }
}

/// How much the island shrinks to sit above a game's measured HUD.
pub fn fit(free_px: Option<u32>) -> f32 {
    match free_px {
        Some(free) if free > 0 => (free as f32 / (EDGE + ISLAND_H)).clamp(MIN_FIT, 1.0),
        _ => 1.0,
    }
}

/// Physical pixels per 1440p design pixel on this monitor for this spot: by
/// the monitor's short side, so a portrait panel is not scaled by its height.
pub fn scale(monitor: &Rect, spot: &Spot) -> f32 {
    let short = (monitor.right - monitor.left).min(monitor.height());
    short.max(1) as f32 / REF_HEIGHT * fit(spot.free_px)
}

/// An action notice's pill height at 1440p (the island grows into it).
pub const NOTICE_H: f32 = 58.0;
/// The spring's height overshoot as a share of the grown height (growing
/// from the 0.75-high collapsed pill or the 44 px stats pill: ≈3%).
const OVERSHOOT_H: f32 = 1.04;

/// How much an action notice grows (multiplies the island scale): up to
/// `NOTICE_H / ISLAND_H`, and only as far as the scan's free depth allows,
/// overshoot included. Unscanned spots and spots with no free depth at all
/// grow fully, as `fit` keeps them at full size.
pub fn grow(spot: &Spot) -> f32 {
    let full = NOTICE_H / ISLAND_H;
    match spot.free_px {
        Some(free) if free > 0 => {
            let k = fit(spot.free_px);
            let room = (free as f32 / k - EDGE) / (ISLAND_H * OVERSHOOT_H);
            room.clamp(1.0, full)
        }
        _ => full,
    }
}

/// The scan's free depth under the island's top edge in physical px (the
/// island's own top gap taken off); unbounded where the scan has no number.
/// A spring's overshoot is clipped here, never the settled island.
pub fn free_depth(spot: &Spot, monitor: &Rect, scale: f32) -> f32 {
    match spot.free_px {
        Some(free) if free > 0 => free as f32 * monitor.height().max(1) as f32 / REF_HEIGHT - EDGE * scale,
        _ => f32::INFINITY,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }

    /// Width / height (0 for an empty rect).
    pub fn aspect(&self) -> f32 {
        if self.height() > 0 { (self.right - self.left) as f32 / self.height() as f32 } else { 0.0 }
    }
}

/// Top-left corner of a `width`-wide island at a top or bottom `anchor`,
/// `edge` px from the edges (a bottom spot's box ends `edge` above the bottom).
pub fn position(anchor: Anchor, monitor: &Rect, width: i32, edge: i32) -> (i32, i32) {
    place(anchor, monitor, width, 0, edge)
}

/// Top-left of a box `along` px long on its edge and `depth` px deep into
/// the screen (unturned sizes: a side spot's box is `depth` wide, `along` high).
fn place(anchor: Anchor, monitor: &Rect, along: i32, depth: i32, edge: i32) -> (i32, i32) {
    let x = match anchor {
        Anchor::TopLeft | Anchor::BottomLeft => monitor.left + edge,
        Anchor::TopRight | Anchor::BottomRight => monitor.right - edge - along,
        Anchor::TopCentre | Anchor::BottomCentre => monitor.left + (monitor.right - monitor.left - along) / 2,
        Anchor::LeftMiddle => monitor.left + edge,
        Anchor::RightMiddle => monitor.right - edge - depth,
    };
    let y = match anchor.side() {
        Side::Top => monitor.top + edge,
        Side::Bottom => monitor.bottom - edge - depth,
        Side::Left | Side::Right => monitor.top + (monitor.height() - along) / 2,
    };
    (x, y)
}

/// How the island lines up its pill and the hint card along its edge
/// (0 = left edges, 0.5 = centres, 1 = right edges; unturned).
pub fn align(anchor: Anchor) -> f32 {
    match anchor {
        Anchor::TopLeft | Anchor::BottomLeft => 0.0,
        Anchor::TopCentre | Anchor::BottomCentre | Anchor::LeftMiddle | Anchor::RightMiddle => 0.5,
        Anchor::TopRight | Anchor::BottomRight => 1.0,
    }
}

/// Where a `w`-wide box sits along the top at `anchor`, `edge` in from the
/// screen's sides (1440p px), as the island places it.
fn footprint(anchor: Anchor, w: f32, edge: f32) -> Option<(f32, f32)> {
    match anchor {
        Anchor::TopLeft => Some((edge, edge + w)),
        Anchor::TopRight => Some((REF_WIDTH - edge - w, REF_WIDTH - edge)),
        Anchor::TopCentre => Some(((REF_WIDTH - w) / 2.0, (REF_WIDTH + w) / 2.0)),
        // The scan's grid covers the top rows only.
        _ => None,
    }
}

/// The HUD area (1440p px2) under the rows [top, bottom) and columns
/// [x0, x1): a grid cell counts whole when the scan found HUD in it.
fn covered(hud: &[u64], (x0, x1): (f32, f32), (top, bottom): (f32, f32)) -> f32 {
    let overlap = |from: f32, to: f32, low: f32, high: f32| (to.min(high) - from.max(low)).max(0.0);
    let row = |r: u32| overlap(r as f32 * HUD_ROW, (r + 1) as f32 * HUD_ROW, top, bottom);
    let column = |(i, bits): (usize, &u64)| {
        let width = overlap(i as f32 * HUD_COLUMN, (i + 1) as f32 * HUD_COLUMN, x0, x1);
        width * (0..u64::BITS).filter(|r| bits >> r & 1 == 1).map(row).sum::<f32>()
    };
    hud.iter().enumerate().map(column).sum()
}

/// The HUD area (1440p px2) a `size` card (physical px) would cover, hanging
/// `below` physical px under the island's top edge: by the game's HUD grid
/// under the card; a spot with no grid uses its one free depth for every
/// column. `None`: unscanned, nothing known to cover.
pub fn card_cover(spot: &Spot, hud: Option<&[u64]>, monitor: &Rect, scale: f32, size: (f32, f32), below: f32) -> Option<f32> {
    let k = monitor.height().max(1) as f32 / REF_HEIGHT;
    let edge = EDGE * scale / k;
    let (w, top) = (size.0 / k, edge + below / k);
    let rows = (top, top + size.1 / k);
    let columns = footprint(spot.anchor, w, edge)?;
    match (hud, spot.free_px) {
        (Some(hud), _) => Some(covered(hud, columns, rows)),
        (None, Some(free)) if free > 0 => Some(w * (rows.1 - rows.0.max(free as f32)).max(0.0)),
        _ => None,
    }
}

/// The window's top-left for a drawn look: the anchor places the look's
/// `box_w`-wide box; the window adds `margin` around it (the appearing
/// circle's overshoot).
pub fn window_origin(anchor: Anchor, monitor: &Rect, box_w: f32, margin: f32, edge: i32) -> (i32, i32) {
    look_origin(anchor, monitor, box_w, 0.0, margin, edge)
}

/// The window's top-left for a look `along` long on its edge and `depth`
/// deep (its unturned `span_w()` and `extent()`), with `margin` around it.
/// A bottom spot keeps the box's bottom `edge` above the screen's, so the
/// island grows upwards; a side spot's window is the turned one.
pub fn look_origin(anchor: Anchor, monitor: &Rect, along: f32, depth: f32, margin: f32, edge: i32) -> (i32, i32) {
    let (x, y) = place(anchor, monitor, along.ceil() as i32, depth.ceil() as i32, edge);
    let m = margin.ceil() as i32;
    (x - m, y - m)
}

#[cfg(test)]
#[path = "placement_tests.rs"]
mod tests;
