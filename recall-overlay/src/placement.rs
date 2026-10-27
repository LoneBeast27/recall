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
}

#[derive(Debug, Default)]
pub struct Spots(HashMap<String, Spot>);

impl Spots {
    /// The table compiled into the binary.
    pub fn embedded() -> Self {
        Self::parse(include_str!("../assets/hud-spots.json")).unwrap_or_default()
    }

    pub fn parse(json: &str) -> Result<Self, String> {
        let file: SpotsFile = serde_json::from_str(json).map_err(|e| format!("hud-spots.json: {e}"))?;
        Ok(Self(
            file.spots
                .into_iter()
                .map(|(key, raw)| (key, Spot { anchor: raw.anchor, free_px: Some(raw.free_px) }))
                .collect(),
        ))
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// The spot for a game key; unknown / empty keys use the top-right default.
    /// Exact key first, then the longest scanned key the target's key starts with:
    /// window titles carry suffixes ("Rocket League (64-bit, DX11, Cooked)").
    pub fn lookup(&self, key: &str) -> Spot {
        if let Some(spot) = self.0.get(key) {
            return *spot;
        }
        self.0
            .iter()
            .filter(|(scanned, _)| !scanned.is_empty() && key.starts_with(scanned.as_str()))
            .max_by_key(|(scanned, _)| scanned.len())
            .map_or(DEFAULT_SPOT, |(_, spot)| *spot)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_table_parses_every_scanned_game() {
        let spots = Spots::embedded();
        assert_eq!(spots.len(), 14);
        assert_eq!(spots.lookup("valorant"), Spot { anchor: Anchor::TopRight, free_px: Some(288) });
        assert_eq!(spots.lookup("rocketleague").anchor, Anchor::TopLeft);
        assert_eq!(spots.lookup("apexlegends").anchor, Anchor::TopCentre);
        assert_eq!(spots.lookup("rocketleague64bitdx11cooked").anchor, Anchor::TopLeft, "title suffixes still match");
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
    fn anchors_place_the_island_inside_the_monitor() {
        let monitor = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
        assert_eq!(position(Anchor::TopRight, &monitor, 300, 12), (2248, 12));
        assert_eq!(position(Anchor::TopLeft, &monitor, 300, 12), (12, 12));
        assert_eq!(position(Anchor::TopCentre, &monitor, 300, 12), (1130, 12));
        let left_screen = Rect { left: -1920, top: -200, right: 0, bottom: 880 };
        assert_eq!(position(Anchor::TopRight, &left_screen, 200, 9), (-209, -191));
    }
}
