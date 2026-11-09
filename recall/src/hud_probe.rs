//! HUD probe for the island's Dynamic placement (`hud_probe` on stdin → one
//! `hud_scan` line). A background thread (below-normal + EcoQoS) samples a few
//! short bursts of the frames the encoder already has (zero-copy path), each
//! read back once and box-averaged to about 640 px wide grey, and finds the HUD
//! the way `scripts/dev/hud-scan.py` does over the clip library: in a burst
//! where the world clearly moved, a pixel that keeps an edge in every frame at
//! an unchanged brightness is HUD. The result is a coarse grid
//! ([`GRID_COLS`] x [`GRID_ROWS`] over the game picture) the island picks a
//! free corner or edge from.
//!
//! Cost: at most [`MAX_BURSTS`] x 3 readbacks over at most [`PROBE_FOR`], one
//! probe at a time and none within [`COOLDOWN`] of the last. The GPU filters each
//! frame down before it is read back (about 1-2 MB per grab, `gpu_frames::GrayReader`),
//! and the device lock is never held while the GPU works or the CPU reads.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::capture::CaptureShared;
use crate::protocol::{emit_or_log, Outbound};

/// The analysis frame's target width (the clip scan's size; thresholds are tuned to it).
pub const ANALYSIS_W: u32 = 640;
/// The grid the island reads: 64 columns (one u64 per row) x 36 rows (40 px cells at 1440p).
pub const GRID_COLS: usize = 64;
pub const GRID_ROWS: usize = 36;

const EDGE_THRESHOLD: f32 = 9.0;
/// Mean grey change between a burst's first and last frame: the world moved.
const MOTION: f32 = 6.0;
/// A HUD pixel's brightness stays within this through a burst.
const STEADY: f32 = 16.0;
/// Occupied pixels in a pixel's 9x9 neighbourhood for it to count (drops specks).
const DENSE: u32 = 12;
/// Moving bursts a pixel must be steady HUD in (HUD that comes and goes counts).
const MIN_HITS: u16 = 2;
/// Occupied analysis pixels a grid cell needs to count as HUD.
const CELL_MIN: u32 = 3;

const BURST_FRAMES: usize = 3;
const BURST_STEP: Duration = Duration::from_millis(200);
const BETWEEN_BURSTS: Duration = Duration::from_millis(1500);
/// Enough moving bursts: stop early.
const ENOUGH: u32 = 8;
const MAX_BURSTS: u32 = 24;
const PROBE_FOR: Duration = Duration::from_secs(60);
/// A probe asked for this soon after the last one ended is refused.
const COOLDOWN: Duration = Duration::from_secs(60);
/// No frame at all for this long: stop (nothing is captured on the GPU path).
const NO_FRAME_FOR: Duration = Duration::from_secs(10);
/// A burst where more of the picture than this holds steady edges is a menu,
/// a scoreboard or a replay over a moving background, not play: skipped.
const MENU_SHARE: f32 = 0.15;
/// A finished scan whose grid marks more of the picture than this as HUD is
/// not trusted (`ok` false): real HUDs leave most of the screen clear.
pub const MAX_HUD_SHARE: f32 = 0.30;
/// A scan with fewer moving bursts than this says nothing (menus, a still camera).
pub const MIN_MOVING: u32 = 4;
/// This many bursts with the world never moving: a still picture (an app, a
/// menu). The probe stops early and reads the still frame instead.
const STILL_ENOUGH: u32 = 8;

/// One grey analysis frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub px: Vec<f32>,
}

impl Gray {
    /// Box-average `src` (BGRA8 rows, `pitch` bytes apart) over the crop `(x, y, w, h)`
    /// down by the whole factor that brings `w` to about [`ANALYSIS_W`].
    pub fn from_bgra(src: &[u8], pitch: usize, (x0, y0, w, h): (usize, usize, usize, usize)) -> Gray {
        let s = (w / ANALYSIS_W as usize).max(1);
        let (ow, oh) = (w / s, h / s);
        let mut px = vec![0f32; ow * oh];
        let norm = 1.0 / (s * s) as f32;
        for oy in 0..oh {
            let out = &mut px[oy * ow..(oy + 1) * ow];
            for dy in 0..s {
                let row = (y0 + oy * s + dy) * pitch + x0 * 4;
                let Some(row) = src.get(row..row + ow * s * 4) else { continue };
                for (ox, cell) in out.iter_mut().enumerate() {
                    for p in row[ox * s * 4..(ox + 1) * s * 4].chunks_exact(4) {
                        // BT.601 luma from B, G, R.
                        *cell += 0.114 * p[0] as f32 + 0.587 * p[1] as f32 + 0.299 * p[2] as f32;
                    }
                }
            }
            out.iter_mut().for_each(|v| *v *= norm);
        }
        Gray { w: ow, h: oh, px }
    }
}

/// `np.hypot(*np.gradient(image))`: central differences, one-sided at the borders.
fn edges(f: &Gray) -> Vec<f32> {
    let (w, h) = (f.w, f.h);
    let at = |x: usize, y: usize| f.px[y * w + x];
    let diff = |a: f32, b: f32, span: f32| (b - a) / span;
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let gx = match (x, w) {
                (_, 1) => 0.0,
                (0, _) => diff(at(0, y), at(1, y), 1.0),
                (x, w) if x == w - 1 => diff(at(x - 1, y), at(x, y), 1.0),
                (x, _) => diff(at(x - 1, y), at(x + 1, y), 2.0),
            };
            let gy = match (y, h) {
                (_, 1) => 0.0,
                (0, _) => diff(at(x, 0), at(x, 1), 1.0),
                (y, h) if y == h - 1 => diff(at(x, y - 1), at(x, y), 1.0),
                (y, _) => diff(at(x, y - 1), at(x, y + 1), 2.0),
            };
            out[y * w + x] = gx.hypot(gy);
        }
    }
    out
}

/// Drop isolated pixels (keep 3+ of 8 neighbours) and the frame's top row.
fn tidy(mask: &[bool], w: usize, h: usize) -> Vec<bool> {
    let mut out = vec![false; w * h];
    for y in 1..h {
        for x in 0..w {
            if !mask[y * w + x] {
                continue;
            }
            let mut n = 0;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if (dx, dy) != (0, 0) && nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h && mask[ny as usize * w + nx as usize] {
                        n += 1;
                    }
                }
            }
            out[y * w + x] = n >= 3;
        }
    }
    out
}

/// While the world moved: edges in every frame at an unchanged brightness (HUD
/// on screen at that moment). None when the world held still or sizes differ.
pub fn steady_edges(frames: &[Gray]) -> Option<Vec<bool>> {
    let (first, last) = (frames.first()?, frames.last()?);
    let (w, h) = (first.w, first.h);
    if w * h == 0 || frames.iter().any(|f| (f.w, f.h) != (w, h)) {
        return None;
    }
    let motion = first.px.iter().zip(&last.px).map(|(a, b)| (a - b).abs()).sum::<f32>() / (w * h) as f32;
    if motion < MOTION {
        return None;
    }
    let grads: Vec<Vec<f32>> = frames.iter().map(edges).collect();
    let mask: Vec<bool> = (0..w * h)
        .map(|i| {
            let edge = grads.iter().map(|g| g[i]).fold(f32::INFINITY, f32::min) > EDGE_THRESHOLD;
            let (lo, hi) = frames.iter().map(|f| f.px[i]).fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| (lo.min(v), hi.max(v)));
            edge && hi - lo < STEADY
        })
        .collect();
    let mask = tidy(&mask, w, h);
    let steady = mask.iter().filter(|m| **m).count();
    (steady as f32 <= MENU_SHARE * (w * h) as f32).then_some(mask)
}

/// Keep occupied pixels with at least [`DENSE`] occupied pixels in their 9x9 box.
fn dense(mask: &[bool], w: usize, h: usize) -> Vec<bool> {
    let mut integral = vec![0u32; (w + 1) * (h + 1)];
    for y in 0..h {
        for x in 0..w {
            integral[(y + 1) * (w + 1) + x + 1] =
                mask[y * w + x] as u32 + integral[y * (w + 1) + x + 1] + integral[(y + 1) * (w + 1) + x] - integral[y * (w + 1) + x];
        }
    }
    let r = 4usize;
    (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let (x0, x1, y0, y1) = (x.saturating_sub(r), (x + r + 1).min(w), y.saturating_sub(r), (y + r + 1).min(h));
            let at = |x: usize, y: usize| integral[y * (w + 1) + x];
            mask[i] && at(x1, y1) + at(x0, y0) - at(x0, y1) - at(x1, y0) >= DENSE
        })
        .collect()
}

/// Steady-edge counts over the moving bursts of one probe.
#[derive(Debug, Default)]
pub struct Tally {
    w: usize,
    h: usize,
    counts: Vec<u16>,
    pub moving: u32,
    pub bursts: u32,
    /// The newest frame read (the still picture a `ui` scan is made from).
    still: Option<Gray>,
}

impl Tally {
    pub fn add(&mut self, frames: &[Gray]) {
        self.bursts += 1;
        self.still = frames.last().cloned();
        let Some(mask) = steady_edges(frames) else { return };
        let (w, h) = (frames[0].w, frames[0].h);
        if (self.w, self.h) != (w, h) {
            // The picture changed size (a resize, a new canvas): start over.
            *self = Tally { w, h, counts: vec![0; w * h], moving: 0, bursts: self.bursts, still: self.still.take() };
        }
        for (count, hit) in self.counts.iter_mut().zip(mask) {
            *count += hit as u16;
        }
        self.moving += 1;
    }

    /// One u64 per grid row: bit c = the cell (c, row) holds HUD.
    pub fn grid(&self) -> Vec<u64> {
        let (w, h) = (self.w, self.h);
        if w * h == 0 {
            return vec![0; GRID_ROWS];
        }
        let occupied: Vec<bool> = self.counts.iter().map(|&c| c >= MIN_HITS).collect();
        to_grid(&dense(&occupied, w, h), w, h)
    }

    /// The picture's width / height (0 before any frame).
    fn aspect(&self) -> f32 {
        self.still.as_ref().filter(|f| f.h > 0).map_or(0.0, |f| f.w as f32 / f.h as f32)
    }
}

/// Occupied analysis pixels binned into the grid: a cell with [`CELL_MIN`] of them is set.
fn to_grid(occupied: &[bool], w: usize, h: usize) -> Vec<u64> {
    let mut cells = vec![0u32; GRID_COLS * GRID_ROWS];
    for (i, _) in occupied.iter().enumerate().filter(|(_, o)| **o) {
        let (c, r) = ((i % w) * GRID_COLS / w, (i / w) * GRID_ROWS / h);
        cells[r * GRID_COLS + c] += 1;
    }
    (0..GRID_ROWS)
        .map(|r| (0..GRID_COLS).filter(|&c| cells[r * GRID_COLS + c] >= CELL_MIN).fold(0u64, |bits, c| bits | 1 << c))
        .collect()
}

/// A still picture's occupied cells: dense edges and text, with the same edge
/// threshold, tidy and density steps as the steady scan, from one frame.
pub fn ui_grid(still: &Gray) -> Vec<u64> {
    let (w, h) = (still.w, still.h);
    if w * h == 0 || still.px.len() != w * h {
        return vec![0; GRID_ROWS];
    }
    let mask: Vec<bool> = edges(still).iter().map(|&g| g > EDGE_THRESHOLD).collect();
    to_grid(&dense(&tidy(&mask, w, h), w, h), w, h)
}

/// The share of grid cells marked HUD.
pub fn grid_share(grid: &[u64]) -> f32 {
    let set: u32 = grid.iter().map(|row| row.count_ones()).sum();
    set as f32 / (GRID_COLS * GRID_ROWS) as f32
}

/// The probe's one-at-a-time guard and its thread.
pub struct HudProbe {
    running: Arc<AtomicBool>,
    /// When the last probe ended.
    ended: Arc<std::sync::Mutex<Option<Instant>>>,
    shared: Arc<CaptureShared>,
    json: bool,
}

impl HudProbe {
    pub fn new(shared: Arc<CaptureShared>, json: bool) -> Self {
        Self { running: Arc::new(AtomicBool::new(false)), ended: Arc::default(), shared, json }
    }

    /// Start a probe unless one is running (a second request joins the first);
    /// within [`COOLDOWN`] of the last one, answer at once that it is too soon.
    pub fn request(&self) {
        let last = self.ended.lock().ok().and_then(|e| *e);
        if let Some(wait) = last.and_then(|at| COOLDOWN.checked_sub(at.elapsed())) {
            let reason = format!("too soon after the last probe (again in {} s)", wait.as_secs() + 1);
            let scan = refused(reason);
            return emit_or_log(self.json, &scan, || "hud probe refused: too soon".into());
        }
        if self.running.swap(true, Relaxed) {
            return;
        }
        let (running, ended, shared, json) = (self.running.clone(), self.ended.clone(), self.shared.clone(), self.json);
        let spawned = std::thread::Builder::new().name("recall-hud".into()).spawn(move || {
            crate::thread_qos::eco_background();
            let scan = probe(&shared);
            emit_or_log(json, &scan, || format!("hud probe: {}", serde_json::to_string(&scan).unwrap_or_default()));
            if let Ok(mut at) = ended.lock() {
                *at = Some(Instant::now());
            }
            running.store(false, Relaxed);
        });
        if spawned.is_err() {
            self.running.store(false, Relaxed);
        }
    }
}

fn refused(reason: String) -> Outbound {
    let (cols, rows) = (GRID_COLS as u32, GRID_ROWS as u32);
    Outbound::HudScan { ok: false, kind: "game", cols, rows, grid: Vec::new(), moving: 0, bursts: 0, aspect: 0.0, reason: Some(reason) }
}

fn probe(shared: &CaptureShared) -> Outbound {
    let started = Instant::now();
    let mut tally = Tally::default();
    let mut failure = None;
    let mut got_frame = false;
    let mut reader = Reader::default();
    while tally.moving < ENOUGH && tally.bursts < MAX_BURSTS && started.elapsed() < PROBE_FOR {
        if shared.quit.load(Relaxed) {
            failure = Some("engine quitting".to_string());
            break;
        }
        let mut frames = Vec::with_capacity(BURST_FRAMES);
        for i in 0..BURST_FRAMES {
            if i > 0 {
                std::thread::sleep(BURST_STEP);
            }
            match grab(shared, &mut reader) {
                Ok(frame) => {
                    got_frame = true;
                    frames.push(frame);
                }
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            }
        }
        if frames.len() == BURST_FRAMES {
            tally.add(&frames);
            failure = None;
        } else if !got_frame && started.elapsed() > NO_FRAME_FOR {
            break; // nothing to read back at all
        } else {
            tally.bursts += 1;
        }
        if tally.moving == 0 && tally.bursts >= STILL_ENOUGH && tally.still.is_some() {
            break; // a still picture: the ui scan below reads it
        }
        std::thread::sleep(BETWEEN_BURSTS);
    }
    verdict(&tally, failure)
}

/// The probe's answer. Moving play gives a `game` scan (steady HUD, refused
/// past [`MAX_HUD_SHARE`]: menus or replays over moving play). Too little
/// motion with a frame in hand gives a `ui` scan of the still picture (apps
/// such as Discord are almost all static UI); it has no share limit.
pub fn verdict(tally: &Tally, failure: Option<String>) -> Outbound {
    let (cols, rows, aspect) = (GRID_COLS as u32, GRID_ROWS as u32, tally.aspect());
    let (moving, bursts) = (tally.moving, tally.bursts);
    if moving < MIN_MOVING {
        if let Some(still) = tally.still.as_ref() {
            let grid = ui_grid(still);
            return Outbound::HudScan { ok: true, kind: "ui", cols, rows, grid, moving, bursts, aspect, reason: None };
        }
    }
    let grid = tally.grid();
    let share = grid_share(&grid);
    let ok = moving >= MIN_MOVING && share <= MAX_HUD_SHARE;
    let reason = match (ok, failure) {
        (true, _) => None,
        (false, _) if moving >= MIN_MOVING => Some(format!("{:.0}% of the picture looked like HUD: menus or replays, not play", share * 100.0)),
        (false, Some(e)) => Some(e),
        (false, None) => Some(format!("only {moving} of {bursts} bursts had the world moving")),
    };
    let grid = if ok { grid } else { Vec::new() };
    Outbound::HudScan { ok, kind: "game", cols, rows, grid, moving, bursts, aspect, reason }
}

/// The newest encoder frame as an analysis frame (the picture only, without the
/// canvas bars of a windowed game).
#[cfg(feature = "encode")]
type Reader = crate::gpu_frames::GrayReader;
#[cfg(not(feature = "encode"))]
type Reader = ();

#[cfg(feature = "encode")]
fn grab(shared: &CaptureShared, reader: &mut Reader) -> Result<Gray, String> {
    let latest = shared.latest_gpu.lock().map_err(|_| "frame lock poisoned".to_string())?;
    let (frame, content) = latest.as_ref().ok_or("no captured frame yet (the zero-copy path is not running)")?;
    let (frame, content) = (frame.try_clone().ok_or("frame clone failed")?, *content);
    let seq = shared.latest_gpu_seq.load(std::sync::atomic::Ordering::Relaxed);
    drop(latest);
    let gray = reader.read(&frame, content);
    // R2-detect-r10-p6-3: a frame stored after `stop_capture` is never analysed.
    if !crate::stop_capture::frame_admitted(seq) {
        return Err("capture stopped".into());
    }
    gray
}

#[cfg(not(feature = "encode"))]
fn grab(_shared: &CaptureShared, _reader: &mut Reader) -> Result<Gray, String> {
    Err("engine built without the encode feature".into())
}

#[cfg(test)]
#[path = "hud_probe_tests.rs"]
mod tests;
