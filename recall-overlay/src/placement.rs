//! Per-game island placement from the HUD scan (`assets/hud-spots.json`):
//! game key → anchor + free top depth at 1440p. Sizes are designed at 1440p
//! and scale by monitor height / 1440; where the scan measured less free depth
//! than the island needs (edge gap + height), the island shrinks to fit.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Design reference: sizes are in 1440p pixels.
pub const REF_HEIGHT: f32 = 1440.0;
/// Island height at 1440p.
pub const ISLAND_H: f32 = 44.0;
/// Gap from the screen edges at 1440p.
pub const EDGE: f32 = 12.0;
/// The island never shrinks below this fraction to fit a tight HUD.
pub const MIN_FIT: f32 = 0.6;

/// The scan's anchor vocabulary (`top-left`, `top-centre`, `top-right`).
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Anchor {
    #[serde(rename = "top-left")]
    TopLeft,
    #[serde(rename = "top-centre", alias = "top-center")]
    TopCentre,
    #[serde(rename = "top-right")]
    TopRight,
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
    /// Free rows per `SKYLINE_BIN` px column at 1440p (HUD that comes and goes counted).
    #[serde(default)]
    skyline: Option<Vec<u16>>,
}

/// The skyline's column width at 1440p.
pub const SKYLINE_BIN: f32 = 32.0;
/// Design reference width (16:9 at 1440p).
pub const REF_WIDTH: f32 = 2560.0;

#[derive(Debug, Default)]
pub struct Spots {
    spots: HashMap<String, Spot>,
    skylines: HashMap<String, Vec<u16>>,
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
            if let Some(skyline) = raw.skyline {
                spots.skylines.insert(key.clone(), skyline);
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
    /// suffixes: "Rocket League (64-bit, DX11, Cooked)").
    fn matched(&self, key: &str) -> Option<&str> {
        if let Some((scanned, _)) = self.spots.get_key_value(key) {
            return Some(scanned.as_str());
        }
        self.spots
            .keys()
            .filter(|scanned| !scanned.is_empty() && key.starts_with(scanned.as_str()))
            .max_by_key(|scanned| scanned.len())
            .map(String::as_str)
    }

    /// The spot for a game key; unknown / empty keys use the top-right default.
    pub fn lookup(&self, key: &str) -> Spot {
        self.matched(key).map_or(DEFAULT_SPOT, |scanned| self.spots[scanned])
    }

    /// The game's skyline, when the scan measured one.
    pub fn skyline(&self, key: &str) -> Option<Vec<u16>> {
        self.matched(key).and_then(|scanned| self.skylines.get(scanned).cloned())
    }
}

/// The skyline's free rows (1440p px) under the columns [x0, x1) (1440p px).
pub fn skyline_free(skyline: &[u16], x0: f32, x1: f32) -> f32 {
    let end = ((x1 / SKYLINE_BIN).ceil().max(0.0) as usize).min(skyline.len());
    let start = ((x0.max(0.0) / SKYLINE_BIN) as usize).min(end);
    skyline[start..end].iter().copied().min().map_or(f32::INFINITY, f32::from)
}

/// Where a `w`-wide box (1440p px) sits along the top at `anchor`, as the island does.
pub fn footprint(anchor: Anchor, w: f32) -> (f32, f32) {
    match anchor {
        Anchor::TopLeft => (EDGE, EDGE + w),
        Anchor::TopRight => (REF_WIDTH - EDGE - w, REF_WIDTH - EDGE),
        Anchor::TopCentre => ((REF_WIDTH - w) / 2.0, (REF_WIDTH + w) / 2.0),
    }
}

/// The room (physical px) for a `card_w`-wide card under a `pill_h`-tall
/// pill and `gap`: the scan's free depth under the card's own columns (its
/// skyline), else under the island's footprint (`free_px`), less the island's
/// top gap. `None`: unscanned (anything fits).
pub fn card_room(spot: &Spot, skyline: Option<&[u16]>, monitor: &Rect, scale: f32, card_w: f32, pill_h: f32, gap: f32) -> Option<f32> {
    let k = monitor.height().max(1) as f32 / REF_HEIGHT;
    let free = match skyline {
        Some(sky) => {
            let (x0, x1) = footprint(spot.anchor, card_w / k);
            skyline_free(sky, x0, x1) * k - EDGE * scale
        }
        None => free_depth(spot, monitor, scale),
    };
    free.is_finite().then_some(free - pill_h - gap)
}

/// How much the island shrinks to sit above a game's measured HUD.
pub fn fit(free_px: Option<u32>) -> f32 {
    match free_px {
        Some(free) if free > 0 => (free as f32 / (EDGE + ISLAND_H)).clamp(MIN_FIT, 1.0),
        _ => 1.0,
    }
}

/// Physical pixels per 1440p design pixel on this monitor for this spot.
pub fn scale(monitor_height: i32, spot: &Spot) -> f32 {
    monitor_height.max(1) as f32 / REF_HEIGHT * fit(spot.free_px)
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
}

/// Top-left corner of a `width`-wide island at `anchor`, `edge` px from the edges.
pub fn position(anchor: Anchor, monitor: &Rect, width: i32, edge: i32) -> (i32, i32) {
    let y = monitor.top + edge;
    let x = match anchor {
        Anchor::TopLeft => monitor.left + edge,
        Anchor::TopRight => monitor.right - edge - width,
        Anchor::TopCentre => monitor.left + (monitor.right - monitor.left - width) / 2,
    };
    (x, y)
}

/// How the island lines up its pill and the hint card for an anchor
/// (0 = left edges, 0.5 = centres, 1 = right edges).
pub fn align(anchor: Anchor) -> f32 {
    match anchor {
        Anchor::TopLeft => 0.0,
        Anchor::TopCentre => 0.5,
        Anchor::TopRight => 1.0,
    }
}

/// The window's top-left for a drawn look: the anchor places the look's
/// `box_w`-wide box; the window adds `margin` around it (the appearing
/// circle's overshoot).
pub fn window_origin(anchor: Anchor, monitor: &Rect, box_w: f32, margin: f32, edge: i32) -> (i32, i32) {
    let (x, y) = position(anchor, monitor, box_w.ceil() as i32, edge);
    let m = margin.ceil() as i32;
    (x - m, y - m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_table_parses_every_scanned_game() {
        let spots = Spots::embedded();
        assert_eq!(spots.len(), 14);
        // The 2026-10-30 scan (HUD that comes and goes counted): VALORANT's kill feed at 124.
        assert_eq!(spots.lookup("valorant"), Spot { anchor: Anchor::TopRight, free_px: Some(124) });
        assert_eq!(spots.lookup("battlefield6").anchor, Anchor::TopLeft);
        assert_eq!(spots.lookup("apexlegends").anchor, Anchor::TopCentre);
        assert_eq!(spots.lookup("battlefield64bit").anchor, Anchor::TopLeft, "title suffixes still match");
        assert!(spots.keys().iter().all(|key| spots.skyline(key).is_some_and(|sky| sky.len() == 80)), "a skyline for every game");
        assert_eq!(spots.lookup("somenewgame"), DEFAULT_SPOT);
        assert_eq!(spots.lookup(""), DEFAULT_SPOT, "desktop uses top-right");
    }

    #[test]
    fn scale_follows_monitor_height() {
        assert_eq!(scale(1440, &DEFAULT_SPOT), 1.0);
        assert_eq!(scale(2160, &DEFAULT_SPOT), 1.5);
        assert_eq!(scale(1080, &DEFAULT_SPOT), 0.75);
        let roomy = Spot { anchor: Anchor::TopRight, free_px: Some(288) };
        assert_eq!(scale(1440, &roomy), 1.0, "plenty of room: full size");
    }

    #[test]
    fn tight_huds_shrink_the_island_to_fit_but_not_below_the_floor() {
        // Overwatch 2: 40 free rows < 12 + 44 → the island's bottom lands on row 40.
        let tight = Spot { anchor: Anchor::TopRight, free_px: Some(40) };
        let k = scale(1440, &tight);
        assert!(((EDGE + ISLAND_H) * k - 40.0).abs() < 1e-3);
        assert_eq!(fit(Some(56)), 1.0);
        assert_eq!(fit(Some(10)), MIN_FIT);
        assert_eq!(fit(Some(0)), 1.0, "no free spot at all: keep full size (the scan says it overlaps)");
        assert_eq!(fit(None), 1.0);
    }

    #[test]
    fn action_notices_grow_only_into_the_free_depth() {
        let full = NOTICE_H / ISLAND_H;
        assert_eq!(grow(&DEFAULT_SPOT), full, "unscanned: full growth");
        let roomy = Spot { anchor: Anchor::TopRight, free_px: Some(288) };
        assert_eq!(grow(&roomy), full, "288 free rows");
        for free in [48, 44, 40, 56] {
            let tight = Spot { anchor: Anchor::TopRight, free_px: Some(free) };
            assert_eq!(grow(&tight), 1.0, "{free} free rows: no room below, sideways only");
            let k = fit(Some(free));
            assert!((EDGE + ISLAND_H) * k <= free as f32 + 1e-3);
        }
        let csgo = Spot { anchor: Anchor::TopRight, free_px: Some(64) };
        let g = grow(&csgo);
        assert!(g > 1.0 && g < full, "partial growth: {g}");
        assert!(EDGE + ISLAND_H * g * OVERSHOOT_H <= 64.0 + 1e-3, "peak stays above the HUD");
    }

    #[test]
    fn free_depth_is_exactly_the_tight_island_and_unbounded_when_unscanned() {
        for height in [1080, 1440, 2160] {
            let monitor = Rect { left: 0, top: 0, right: height * 16 / 9, bottom: height };
            let tight = Spot { anchor: Anchor::TopRight, free_px: Some(40) };
            let k = scale(height, &tight);
            assert!((free_depth(&tight, &monitor, k) - ISLAND_H * k).abs() < 1e-3, "the fitted island fills it at {height}p");
            assert_eq!(free_depth(&DEFAULT_SPOT, &monitor, 1.0), f32::INFINITY);
            let none_free = Spot { anchor: Anchor::TopRight, free_px: Some(0) };
            assert_eq!(free_depth(&none_free, &monitor, 1.0), f32::INFINITY, "no free spot: nothing to protect");
        }
    }

    #[test]
    fn a_skyline_gives_each_box_the_free_rows_under_its_own_columns() {
        // VALORANT-like: the top bar and a stats line down to 12 px as far as x 2080, then 124 (the kill feed).
        let sky: Vec<u16> = (0..80).map(|i| if i < 65 { 12 } else { 124 }).collect();
        assert_eq!(skyline_free(&sky, 2148.0, 2548.0), 124.0, "the island's 400 px footprint");
        assert_eq!(skyline_free(&sky, 1912.0, 2548.0), 12.0, "a 636 px row reaches the top bar");
        assert_eq!(skyline_free(&sky, 5000.0, 6000.0), f32::INFINITY, "off the scan");
        assert_eq!(footprint(Anchor::TopRight, 300.0), (2248.0, 2548.0));
        assert_eq!(footprint(Anchor::TopLeft, 300.0), (12.0, 312.0));
        assert_eq!(footprint(Anchor::TopCentre, 300.0), (1130.0, 1430.0));
        let spot = Spot { anchor: Anchor::TopRight, free_px: Some(124) };
        let qhd = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
        assert_eq!(card_room(&spot, Some(&sky), &qhd, 1.0, 299.0, 44.0, 6.0), Some(62.0), "124 - 12 - 44 - 6");
        assert_eq!(card_room(&spot, Some(&sky), &qhd, 1.0, 636.0, 44.0, 6.0), Some(-50.0));
        assert_eq!(card_room(&spot, None, &qhd, 1.0, 636.0, 44.0, 6.0), Some(62.0), "no skyline: the footprint's depth");
        assert_eq!(card_room(&DEFAULT_SPOT, None, &qhd, 1.0, 299.0, 44.0, 6.0), None, "unscanned");
        let uhd = Rect { left: 0, top: 0, right: 3840, bottom: 2160 };
        assert_eq!(card_room(&spot, Some(&sky), &uhd, 1.5, 448.5, 66.0, 9.0), Some(124.0 * 1.5 - 18.0 - 66.0 - 9.0), "4K: same columns, scaled");
        let json = format!(r#"{{"spots":{{"game":{{"anchor":"top-right","free_px":124,"skyline":{sky:?}}},"old":{{"anchor":"top-left","free_px":48}}}}}}"#);
        let spots = Spots::parse(&json).unwrap();
        assert_eq!((spots.skyline("game"), spots.skyline("game64bit").is_some()), (Some(sky.clone()), true), "suffixes match too");
        assert_eq!(spots.skyline("old"), None, "a spot without a skyline");
    }

    #[test]
    fn a_window_with_a_margin_keeps_its_box_on_the_anchor() {
        let monitor = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
        assert_eq!(window_origin(Anchor::TopRight, &monitor, 44.0, 0.0, 12), (2504, 12), "the circle sits at the right end");
        assert_eq!(window_origin(Anchor::TopRight, &monitor, 44.0, 2.0, 12), (2502, 10), "the margin wraps the same box");
        assert_eq!(window_origin(Anchor::TopRight, &monitor, 299.4, 0.0, 12), position(Anchor::TopRight, &monitor, 300, 12));
    }

    #[test]
    fn anchors_place_the_island_inside_the_monitor() {
        let monitor = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
        assert_eq!(position(Anchor::TopRight, &monitor, 300, 12), (2248, 12));
        assert_eq!(position(Anchor::TopLeft, &monitor, 300, 12), (12, 12));
        assert_eq!(position(Anchor::TopCentre, &monitor, 300, 12), (1130, 12));
        let left_screen = Rect { left: -1920, top: -200, right: 0, bottom: 880 };
        assert_eq!(position(Anchor::TopRight, &left_screen, 200, 9), (-209, -191));
    }
}
