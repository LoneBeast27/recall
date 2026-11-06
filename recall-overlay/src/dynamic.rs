//! The Dynamic corner: the engine's HUD probe (`hud_scan`, a coarse grid of
//! where the game's HUD sits) picks a free corner or side edge for the island.
//! Scans are kept per game in a small file (`%APPDATA%\Recall\island-hud.json`),
//! the newest [`KEEP`] per game, and read together (HUD seen in any of them
//! counts: kill feeds and alerts come and go). [`Prober`] decides when to ask
//! for another scan: soon for a game with no scans, rarely once it has a few.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::placement::{Anchor, EDGE, NOTICE_H, REF_HEIGHT};

/// A scan marking more of the picture than this as HUD came from menus or
/// replays, not play (the engine refuses these too; older saved ones are skipped).
pub const MAX_HUD_SHARE: f32 = 0.30;

fn share(scan: &[u64], cols: u32, rows: u32) -> f32 {
    scan.iter().map(|r| r.count_ones()).sum::<u32>() as f32 / (cols * rows).max(1) as f32
}

/// Scans kept per game.
pub const KEEP: usize = 6;
/// Games kept; the one scanned longest ago goes first.
pub const MAX_GAMES: usize = 64;
/// A game with this many scans is settled: re-checked only now and then.
pub const SETTLED: usize = 3;

/// The island's footprint the pick keeps clear (1440p px): a wide notice along
/// its edge, and its depth from the screen edge (gap, grown notice, a little air).
const ALONG: f32 = 360.0;
const DEPTH: f32 = EDGE + NOTICE_H + 10.0;

/// Spots in the order they are preferred when equally clear: the corners
/// (top first), then the side edges (turned vertical), then the centres.
pub const PREFERENCE: [Anchor; 8] = [
    Anchor::TopRight,
    Anchor::TopLeft,
    Anchor::BottomRight,
    Anchor::BottomLeft,
    Anchor::RightMiddle,
    Anchor::LeftMiddle,
    Anchor::TopCentre,
    Anchor::BottomCentre,
];

/// Where the HUD sits over the game picture: `grid[r]` bit c = cell (c, r).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HudGrid {
    pub cols: u32,
    pub rows: u32,
    pub grid: Vec<u64>,
}

impl HudGrid {
    /// A `hud_scan` event's grid (None when the scan was not `ok` or is malformed).
    pub fn from_scan(event: &Value) -> Option<HudGrid> {
        if event.get("ok").and_then(Value::as_bool) != Some(true) {
            return None;
        }
        let dim = |k: &str| event.get(k).and_then(Value::as_u64).filter(|&n| (1..=64).contains(&n)).map(|n| n as u32);
        let (cols, rows) = (dim("cols")?, dim("rows")?);
        let grid: Vec<u64> = event.get("grid")?.as_array()?.iter().map(Value::as_u64).collect::<Option<_>>()?;
        (grid.len() == rows as usize).then_some(HudGrid { cols, rows, grid })
    }

    fn cell(&self, c: u32, r: u32) -> bool {
        self.grid.get(r as usize).is_some_and(|bits| bits >> c & 1 == 1)
    }

    /// HUD cells under the picture fraction `[x0, x1) x [y0, y1)`.
    fn cover(&self, (x0, y0, x1, y1): (f32, f32, f32, f32)) -> u32 {
        let span = |from: f32, to: f32, n: u32| {
            let first = ((from * n as f32).floor().max(0.0) as u32).min(n);
            let last = ((to * n as f32).ceil().max(0.0) as u32).min(n);
            first..last
        };
        let mut n = 0;
        for r in span(y0, y1, self.rows) {
            for c in span(x0, x1, self.cols) {
                n += self.cell(c, r) as u32;
            }
        }
        n
    }
}

/// The picture fraction an anchor's footprint covers on a `aspect` (w / h) screen.
fn footprint(anchor: Anchor, aspect: f32) -> (f32, f32, f32, f32) {
    let width = REF_HEIGHT * aspect.max(0.5);
    let (along_x, along_y) = (ALONG / width, ALONG / REF_HEIGHT);
    let (deep_x, deep_y) = (DEPTH / width, DEPTH / REF_HEIGHT);
    let edge_x = EDGE / width;
    let centre = |along: f32| ((1.0 - along) / 2.0, (1.0 + along) / 2.0);
    let (x0, x1) = match anchor {
        Anchor::TopLeft | Anchor::BottomLeft => (edge_x, edge_x + along_x),
        Anchor::TopRight | Anchor::BottomRight => (1.0 - edge_x - along_x, 1.0 - edge_x),
        Anchor::TopCentre | Anchor::BottomCentre => centre(along_x),
        Anchor::LeftMiddle => (0.0, deep_x),
        Anchor::RightMiddle => (1.0 - deep_x, 1.0),
    };
    let (y0, y1) = match anchor {
        Anchor::TopLeft | Anchor::TopCentre | Anchor::TopRight => (0.0, deep_y),
        Anchor::BottomLeft | Anchor::BottomCentre | Anchor::BottomRight => (1.0 - deep_y, 1.0),
        Anchor::LeftMiddle | Anchor::RightMiddle => centre(along_y),
    };
    (x0, y0, x1, y1)
}

/// The clearest spot: the least HUD under the island's footprint, the
/// current spot kept on a tie (no hopping), then [`PREFERENCE`] order.
pub fn choose(hud: &HudGrid, aspect: f32, current: Option<Anchor>) -> Anchor {
    let cover = |a: Anchor| hud.cover(footprint(a, aspect));
    let best = PREFERENCE.iter().map(|&a| cover(a)).min().unwrap_or(0);
    match current.filter(|&a| cover(a) == best) {
        Some(keep) => keep,
        None => PREFERENCE.into_iter().find(|&a| cover(a) == best).unwrap_or(Anchor::TopRight),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Entry {
    cols: u32,
    rows: u32,
    /// Newest last.
    scans: Vec<Vec<u64>>,
    /// When it was last scanned, as a count of scans kept in this file.
    #[serde(default)]
    seq: u64,
}

impl Entry {
    /// What `HudGrid::from_scan` would accept: 1..=64 columns and rows, a row per scan line.
    fn sane(&self) -> bool {
        (1..=64).contains(&self.cols) && (1..=64).contains(&self.rows) && self.scans.iter().all(|s| s.len() == self.rows as usize)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    games: BTreeMap<String, Entry>,
    #[serde(default)]
    seq: u64,
}

/// The per-game scans, on disk at `path` (None: in memory only, for tests).
#[derive(Debug, Default)]
pub struct HudCache {
    path: Option<PathBuf>,
    file: CacheFile,
}

impl HudCache {
    /// `%APPDATA%\Recall\island-hud.json`.
    pub fn default_path() -> Option<PathBuf> {
        std::env::var_os("APPDATA").map(|dir| PathBuf::from(dir).join("Recall").join("island-hud.json"))
    }

    pub fn load(path: Option<PathBuf>) -> Self {
        let file = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let mut cache = Self { path, file };
        cache.file.games.retain(|_, e| e.sane());
        cache.trim();
        cache
    }

    pub fn scans(&self, key: &str) -> usize {
        self.file.games.get(key).map_or(0, |e| e.scans.len())
    }

    /// Every kept scan of the game, together (a cell is HUD when any scan saw it).
    pub fn grid(&self, key: &str) -> Option<HudGrid> {
        let entry = self.file.games.get(key)?;
        let good: Vec<&Vec<u64>> = entry.scans.iter().filter(|s| share(s, entry.cols, entry.rows) <= MAX_HUD_SHARE).collect();
        if good.is_empty() {
            return None;
        }
        let grid = (0..entry.rows as usize).map(|r| good.iter().fold(0, |bits, s| bits | s.get(r).copied().unwrap_or(0))).collect();
        Some(HudGrid { cols: entry.cols, rows: entry.rows, grid })
    }

    /// Keeps a new scan (a grid of another size starts the game over) and saves.
    pub fn add(&mut self, key: &str, scan: HudGrid) {
        if key.is_empty() || share(&scan.grid, scan.cols, scan.rows) > MAX_HUD_SHARE {
            return;
        }
        let entry = self.file.games.entry(key.to_string()).or_default();
        if (entry.cols, entry.rows) != (scan.cols, scan.rows) {
            *entry = Entry { cols: scan.cols, rows: scan.rows, scans: Vec::new(), seq: 0 };
        }
        entry.scans.push(scan.grid);
        let extra = entry.scans.len().saturating_sub(KEEP);
        entry.scans.drain(..extra);
        self.file.seq += 1;
        entry.seq = self.file.seq;
        if !entry.sane() {
            self.file.games.remove(key);
        }
        self.trim();
        self.save();
    }

    /// At most [`MAX_GAMES`]: the games scanned longest ago go.
    fn trim(&mut self) {
        while self.file.games.len() > MAX_GAMES {
            let Some(oldest) = self.file.games.iter().min_by_key(|(_, e)| e.seq).map(|(k, _)| k.clone()) else { break };
            self.file.games.remove(&oldest);
        }
    }

    /// Written whole to `<path>.tmp`, then renamed over the old file.
    fn save(&self) {
        let Some(path) = self.path.as_ref() else { return };
        let Ok(text) = serde_json::to_string(&self.file) else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

/// First probe after a game appears: soon for an unscanned game (the probe
/// itself waits for moving play), later once some scans are kept.
const FIRST_UNSCANNED: Duration = Duration::from_secs(20);
const FIRST_SCANNED: Duration = Duration::from_secs(90);
const FIRST_SETTLED: Duration = Duration::from_secs(10 * 60);
/// After a good scan, until the game is settled / once it is.
const AGAIN_SOON: Duration = Duration::from_secs(3 * 60);
const AGAIN_LATE: Duration = Duration::from_secs(30 * 60);
/// A failed (menus, a still camera) or unanswered probe: try again after this.
const RETRY: Duration = Duration::from_secs(2 * 60);
/// The engine answers within ~70 s; past this the probe counts as failed.
const PENDING_FOR: Duration = Duration::from_secs(90);
/// Failed probes per game session before Dynamic stops asking.
const MAX_FAILURES: u32 = 5;

/// When to ask the engine for another HUD scan of the current game.
#[derive(Debug, Default)]
pub struct Prober {
    key: String,
    next: Option<Instant>,
    /// The game a sent probe is for, and when it was sent.
    pending: Option<(String, Instant)>,
    failures: u32,
}

impl Prober {
    /// Each tick: the game's cache key ("" = no game), whether a probe may run
    /// now (Dynamic, the game recorded and in front) and its kept scans. True =
    /// send `hud_probe` now.
    pub fn due(&mut self, key: &str, active: bool, scans: usize, now: Instant) -> bool {
        if key != self.key {
            let first = match scans {
                0 => FIRST_UNSCANNED,
                n if n < SETTLED => FIRST_SCANNED,
                _ => FIRST_SETTLED,
            };
            *self = Prober { key: key.to_string(), next: Some(now + first), pending: None, failures: 0 };
        }
        if let Some((_, at)) = self.pending {
            if now.saturating_duration_since(at) < PENDING_FOR {
                return false;
            }
            self.pending = None;
            self.failed(now);
        }
        if key.is_empty() || !active || self.failures >= MAX_FAILURES {
            return false;
        }
        match self.next {
            Some(at) if now >= at => {
                self.pending = Some((key.to_string(), now));
                self.next = None;
                true
            }
            _ => false,
        }
    }

    /// A `hud_scan` came back: the game it belongs to (None when nothing was
    /// asked, or the target changed since), and the next probe is planned.
    /// `scans` = the game's kept scans once this one is added.
    pub fn answered(&mut self, ok: bool, scans: usize, now: Instant) -> Option<String> {
        let (key, _) = self.pending.take()?;
        if key != self.key {
            return None;
        }
        if ok {
            self.next = Some(now + if scans < SETTLED { AGAIN_SOON } else { AGAIN_LATE });
        } else {
            self.failed(now);
        }
        Some(key)
    }

    fn failed(&mut self, now: Instant) {
        self.failures += 1;
        self.next = Some(now + RETRY);
    }
}

#[cfg(test)]
#[path = "dynamic_tests.rs"]
mod tests;
