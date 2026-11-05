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
            Side::Right => Turn::Cw,
            Side::Left => Turn::Ccw,
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
mod tests {
    use super::*;

    /// A HUD grid column occupied over the rows [from, to) (multiples of 8).
    fn rows(from: u32, to: u32) -> u64 {
        (from / 8..to / 8).fold(0, |bits, r| bits | 1 << r)
    }

    #[test]
    fn the_island_keeps_masters_spots_for_every_game() {
        use Anchor::{TopCentre, TopLeft, TopRight};
        // The table on master: the re-scan feeds the hint card only (user's ruling, 2026-10-30).
        let master = [
            ("valorant", TopRight, 288),
            ("rocketleague", TopLeft, 48),
            ("apexlegends", TopCentre, 72),
            ("overwatch", TopCentre, 44),
            ("overwatch2", TopRight, 40),
            ("marvelrivals", TopRight, 56),
            ("genshinimpact", TopCentre, 104),
            ("battlefield6", TopRight, 0),
            ("counterstrike2", TopRight, 96),
            ("tomclancysrainbowsixsiege", TopRight, 376),
            ("rainbowsixsiege", TopRight, 376),
            ("counterstrikeglobaloffensive", TopRight, 64),
            ("forzahorizon6", TopCentre, 172),
            ("lethalcompany", TopCentre, 100),
        ];
        let spots = Spots::embedded();
        assert_eq!(spots.len(), master.len());
        for (key, anchor, free) in master {
            assert_eq!(spots.lookup(key), Spot { anchor, free_px: Some(free) }, "{key}");
            assert_eq!(spots.hud(key).map(|hud| hud.len()), Some(80), "{key} has its HUD grid for the hint card");
        }
    }

    #[test]
    fn embedded_table_parses_every_scanned_game() {
        let spots = Spots::embedded();
        assert_eq!(spots.lookup("rocketleague64bitdx11cooked").anchor, Anchor::TopLeft, "title suffixes still match");
        assert_eq!(spots.lookup("somenewgame"), DEFAULT_SPOT);
        assert_eq!((spots.lookup(""), spots.hud("")), (DEFAULT_SPOT, None), "desktop uses top-right");
    }

    /// The controller's target name is the window title, not the scan's key:
    /// each name here takes the path a status takes to `App::place`.
    #[test]
    fn real_target_names_reach_their_scanned_spots() {
        use crate::session::Session;
        let names = [
            // Seen as `record: target -> "…"` in recall-hdr.log.
            ("Rocket League (64-bit, DX11, Cooked)", "RocketLeague.exe", "rocketleague"),
            ("VALORANT", "VALORANT-Win64-Shipping.exe", "valorant"),
            ("Counter-Strike 2", "cs2.exe", "counterstrike2"),
            ("Genshin Impact", "GenshinImpact.exe", "genshinimpact"),
            // As the clip library names them.
            ("Rocket League®", "RocketLeague.exe", "rocketleague"),
            ("Apex Legends", "r5apex.exe", "apexlegends"),
            ("Overwatch", "Overwatch.exe", "overwatch"),
            ("Overwatch® 2", "Overwatch.exe", "overwatch2"),
            ("Marvel Rivals", "Marvel-Win64-Shipping.exe", "marvelrivals"),
            ("GenshinImpact", "GenshinImpact.exe", "genshinimpact"),
            ("Battlefield™ 6", "bf6.exe", "battlefield6"),
            ("Tom Clancys Rainbow Six Siege", "RainbowSix.exe", "tomclancysrainbowsixsiege"),
            ("Counter-Strike Global Offensive", "csgo.exe", "counterstrikeglobaloffensive"),
            ("Forza Horizon 6", "forza_gaming.desktop.x64_release_final.exe", "forzahorizon6"),
            ("Lethal Company", "Lethal Company.exe", "lethalcompany"),
        ];
        let spots = Spots::embedded();
        for (name, exe, scanned) in names {
            let mut session = Session::default();
            session.apply_status(&serde_json::json!({"type":"status","state":"recording","target":{"name":name,"exe":exe,"pid":42}}));
            assert_eq!(spots.lookup(&session.game_key()), spots.lookup(scanned), "{name}");
            assert_ne!(spots.lookup(&session.game_key()), DEFAULT_SPOT, "{name} is not the default spot");
        }
        let monitor = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
        let mut rocket = Session::default();
        rocket.apply_status(&serde_json::json!({"type":"status","state":"recording","target":{"name":names[0].0,"exe":names[0].1,"pid":42}}));
        assert_eq!(position(spots.lookup(&rocket.game_key()).anchor, &monitor, 300, 12), (12, 12), "Rocket League sits top-left");

        // An empty window title sends the exe stem as the name (`display_name` in recall-hdr).
        let builds = [("r5apex_dx12.exe", "apexlegends"), ("RainbowSix_Vulkan.exe", "tomclancysrainbowsixsiege")];
        let stems = names.iter().map(|&(_, exe, scanned)| (exe, scanned)).chain(builds);
        let mut misses = Vec::new();
        for (exe, scanned) in stems {
            // Overwatch.exe runs both games: its stem cannot tell them apart.
            let scanned = if scanned == "overwatch2" { "overwatch" } else { scanned };
            let stem = exe.strip_suffix(".exe").unwrap();
            let mut session = Session::default();
            session.apply_status(&serde_json::json!({"type":"status","state":"recording","target":{"name":stem,"exe":exe,"pid":42}}));
            let key = session.game_key();
            if spots.lookup(&key) != spots.lookup(scanned) || spots.hud(&key) != spots.hud(scanned) {
                misses.push(stem);
            }
        }
        assert!(misses.is_empty(), "exe stems that miss their scanned spot: {misses:?}");
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
    fn a_card_covers_the_hud_under_its_own_columns() {
        // VALORANT-like: HUD from the top as far as x 2080, then the kill feed from row 120.
        let hud: Vec<u64> = (0..80).map(|i| if i < 65 { rows(0, 512) } else { rows(120, 512) }).collect();
        let spot = Spot { anchor: Anchor::TopRight, free_px: Some(288) };
        let qhd = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
        let cover = |w: f32, h: f32| card_cover(&spot, Some(&hud), &qhd, 1.0, (w, h), 50.0);
        assert_eq!(cover(172.0, 52.0), Some(0.0), "clear above the kill feed (rows 62-114)");
        assert_eq!(cover(299.0, 116.0), Some(58.0 * 299.0), "rows 120-178 over the kill feed");
        assert_eq!(cover(636.0, 52.0), Some(168.0 * 52.0), "its 168 px left of x 2080, all 52 rows");
        let uhd = Rect { left: 0, top: 0, right: 3840, bottom: 2160 };
        assert_eq!(card_cover(&spot, Some(&hud), &uhd, 1.5, (448.5, 174.0), 75.0), Some(58.0 * 299.0), "4K: the same 1440p area");
        let left = Spot { anchor: Anchor::TopLeft, ..spot };
        let wall: Vec<u64> = (0..80).map(|i| if i < 5 { u64::MAX } else { 0 }).collect();
        assert_eq!(card_cover(&left, Some(&wall), &qhd, 1.0, (200.0, 52.0), 50.0), Some(148.0 * 52.0), "from the left edge: x 12-160");
        let centre = Spot { anchor: Anchor::TopCentre, ..spot };
        assert_eq!(card_cover(&centre, Some(&wall), &qhd, 1.0, (200.0, 52.0), 50.0), Some(0.0), "centred, far from the wall");
    }

    #[test]
    fn only_the_rows_that_hold_hud_count_not_everything_below_them() {
        // A thin banner across the screen, rows 96-112, and nothing under it.
        let hud = vec![rows(96, 112); 80];
        let spot = Spot { anchor: Anchor::TopRight, free_px: Some(288) };
        let qhd = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
        assert_eq!(card_cover(&spot, Some(&hud), &qhd, 1.0, (172.0, 180.0), 50.0), Some(16.0 * 172.0), "16 rows of the card's 180");
    }

    #[test]
    fn without_a_grid_the_one_free_depth_stands_in_and_unscanned_covers_nothing_known() {
        let qhd = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
        let flat = Spot { anchor: Anchor::TopRight, free_px: Some(200) };
        assert_eq!(card_cover(&flat, None, &qhd, 1.0, (172.0, 180.0), 50.0), Some(42.0 * 172.0), "rows 200-242");
        assert_eq!(card_cover(&flat, None, &qhd, 1.0, (299.0, 116.0), 50.0), Some(0.0));
        assert_eq!(card_cover(&DEFAULT_SPOT, None, &qhd, 1.0, (172.0, 180.0), 50.0), None, "no scan data");
        let none_free = Spot { anchor: Anchor::TopRight, free_px: Some(0) };
        assert_eq!(card_cover(&none_free, None, &qhd, 1.0, (172.0, 180.0), 50.0), None);
    }

    #[test]
    fn spots_parse_their_hud_grids() {
        let hud: Vec<u64> = vec![u64::MAX; 80];
        let json = format!(r#"{{"spots":{{"game":{{"anchor":"top-right","free_px":124,"hud":{hud:?}}},"old":{{"anchor":"top-left","free_px":48}}}}}}"#);
        let spots = Spots::parse(&json).unwrap();
        assert_eq!((spots.hud("game"), spots.hud("game64bit").is_some()), (Some(hud), true), "suffixes match too");
        assert_eq!(spots.hud("old"), None, "a spot without a grid");
    }

    #[test]
    fn a_window_with_a_margin_keeps_its_box_on_the_anchor() {
        let monitor = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
        assert_eq!(window_origin(Anchor::TopRight, &monitor, 44.0, 0.0, 12), (2504, 12), "the circle sits at the right end");
        assert_eq!(window_origin(Anchor::TopRight, &monitor, 44.0, 2.0, 12), (2502, 10), "the margin wraps the same box");
        assert_eq!(window_origin(Anchor::TopRight, &monitor, 299.4, 0.0, 12), position(Anchor::TopRight, &monitor, 300, 12));
    }

    #[test]
    fn bottom_and_side_spots_keep_the_box_on_their_edge() {
        let qhd = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
        // A 300 x 44 pill with a 100 px card unfolded: 300 along, 144 deep.
        assert_eq!(look_origin(Anchor::BottomRight, &qhd, 300.0, 144.0, 0.0, 12), (2248, 1440 - 12 - 144), "grows upwards");
        assert_eq!(look_origin(Anchor::BottomLeft, &qhd, 300.0, 44.0, 2.0, 12), (10, 1440 - 12 - 44 - 2));
        assert_eq!(look_origin(Anchor::BottomCentre, &qhd, 300.0, 44.0, 0.0, 12), (1130, 1384));
        // Side spots: the turned window is `depth` wide and `along` high, centred down the edge.
        assert_eq!(look_origin(Anchor::RightMiddle, &qhd, 300.0, 44.0, 0.0, 12), (2560 - 12 - 44, 570));
        assert_eq!(look_origin(Anchor::LeftMiddle, &qhd, 300.0, 144.0, 0.0, 12), (12, 570), "the card reaches inwards");
        let left_screen = Rect { left: -1920, top: -200, right: 0, bottom: 880 };
        assert_eq!(look_origin(Anchor::RightMiddle, &left_screen, 200.0, 44.0, 0.0, 9), (-53, 240));
    }

    #[test]
    fn corners_turns_and_alignment_per_anchor() {
        use crate::presence::Turn;
        assert_eq!(Anchor::from_corner("bottom-left"), Some(Anchor::BottomLeft));
        assert_eq!(Anchor::from_corner("top-right"), Some(Anchor::TopRight));
        assert_eq!(Anchor::from_corner("dynamic"), None);
        assert_eq!((Anchor::RightMiddle.turn(), Anchor::LeftMiddle.turn(), Anchor::BottomRight.turn()), (Turn::Cw, Turn::Ccw, Turn::None));
        assert_eq!((align(Anchor::BottomLeft), align(Anchor::RightMiddle), align(Anchor::BottomRight)), (0.0, 0.5, 1.0));
        let qhd = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
        let low = Spot { anchor: Anchor::BottomRight, free_px: None };
        assert_eq!(card_cover(&low, Some(&[u64::MAX; 80]), &qhd, 1.0, (200.0, 52.0), 50.0), None, "the scan's grid is the top rows only");
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
