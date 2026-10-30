//! `--preview-notice PATH.bmp`: the action notice's transformation, frame by
//! frame, through the real island state machine, renderer and fonts (no
//! window). PATH.bmp = "Clip saved" over the stats pill (dark and light
//! backdrops) beside the same notice with no stats under it;
//! `<stem>-before-after.bmp` = the old content swap beside the new
//! transformation. Each image gets `<image>.labels.tsv` (x, y, px, text) for
//! the PNG step to print the row times and column titles.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::fonts::Fonts;
use crate::island::{notice_layout, Content, Island};
use crate::layout::{stats_scene, Scene};
use crate::notice::NoticeView;
use crate::placement::{grow, DEFAULT_SPOT};
use crate::preview::{notice_from, stats_states, tile, Canvas, Tile, DARK, LIGHT};
use crate::render::Renderer;

/// The save lands here; the notice hands back at `SAVED + HOLD`.
const SAVED: Duration = Duration::from_millis(1_000);
const HOLD: Duration = Duration::from_secs(3); // the reducer's SAVED_FOR
const STEP: Duration = Duration::from_millis(40);

/// What the island is told to show from one moment on (`None` = hide).
#[derive(Clone)]
struct Shown {
    content: Content,
    scene: Scene,
    scale: f32,
    size: (f32, f32),
}

/// One island driven through timed updates, drawn at any moment.
struct Film {
    island: Island,
    start: Instant,
    steps: Vec<(Duration, Option<Shown>)>,
    applied: usize,
    drawn: Option<Shown>,
}

impl Film {
    fn new(steps: Vec<(Duration, Option<Shown>)>) -> Self {
        let start = Instant::now();
        Self { island: Island::new(start), start, steps, applied: 0, drawn: None }
    }

    /// Applies every update due by `at` (in order), then draws that moment.
    fn at(&mut self, renderer: &mut Renderer, fonts: &Fonts, at: Duration) -> Result<Option<Tile>, String> {
        while let Some((when, shown)) = self.steps.get(self.applied).filter(|(when, _)| *when <= at).cloned() {
            let now = self.start + when;
            self.island.tick(now);
            self.island.update(shown.as_ref().map(|s| (s.content.clone(), s.size.0, s.size.1)), now);
            if shown.is_some() {
                self.drawn = shown;
            }
            self.applied += 1;
        }
        let now = self.start + at;
        self.island.tick(now);
        let Some(shown) = self.drawn.as_ref().filter(|_| self.island.shown()) else { return Ok(None) };
        let (look, sheen, badge) = (self.island.look(now, f32::INFINITY), self.island.sheen(now), self.island.badge(now));
        tile(renderer, fonts, &shown.scene, look, shown.scale, sheen, badge).map(Some)
    }
}

/// The row times: the stats before the save, every 40 ms through the
/// spring and the sweep, the hold, then every 40 ms of the hand-back.
fn rows() -> Vec<(String, Duration)> {
    let mut rows = vec![("before".to_string(), SAVED - Duration::from_millis(100))];
    rows.extend((0..=16u32).map(|i| (format!("{} ms", i * 40), SAVED + STEP * i)));
    rows.push(("1.5 s".into(), SAVED + Duration::from_millis(1_500)));
    // The hand-back (200 ms) or, with no stats, the fade, roll-up and shrink (550 ms).
    rows.extend((0..=14u32).map(|i| (format!("{:.2} s", 3.0 + i as f32 * 0.04), SAVED + HOLD + STEP * i)));
    rows
}

fn saved_notice() -> Result<NoticeView, String> {
    let event = json!({"mod":"engine","event":{"type":"saved","path":"V:\\c\\VALORANT__2026-10-30__21-14-03.mp4","seconds":30.0}});
    notice_from(&[event]).ok_or_else(|| "the saved event shows no notice".into())
}

/// The island's updates: stats from 0 (when `stats`), the save at `SAVED`,
/// the hand-back after the hold. `emphasis` false = the old content swap.
fn steps(fonts: &Fonts, scale: f32, stats: bool, emphasis: bool) -> Result<Vec<(Duration, Option<Shown>)>, String> {
    // The old content swap: no transformation, no save badge.
    let saved = saved_notice()?;
    let notice = if emphasis { saved } else { NoticeView { emphasis: false, save: None, ..saved } };
    let resting = stats_scene(&stats_states()[0], scale, fonts);
    let stats_shown = Shown { content: Content::Stats, size: (resting.w, resting.h), scene: resting, scale };
    // The app's own sizing, at an unscanned spot (full growth).
    let under = || stats.then_some(stats_shown.size.0);
    let (scene, notice_scale, size) = notice_layout(&notice, scale, grow(&DEFAULT_SPOT), under, fonts);
    let notice_shown = Shown { content: Content::Notice(notice), scene, scale: notice_scale, size };
    let back = stats.then(|| stats_shown.clone());
    let mut steps = vec![(SAVED, Some(notice_shown)), (SAVED + HOLD, back.clone())];
    if stats {
        steps.insert(0, (Duration::ZERO, back));
    }
    Ok(steps)
}

/// A filmstrip image: a time gutter, then one column per (film, backdrop).
struct Sheet {
    canvas: Canvas,
    labels: Vec<String>,
}

const GUTTER: usize = 96;
const HEADER: usize = 40;

fn sheet(renderer: &mut Renderer, fonts: &Fonts, scale: f32, columns: Vec<(&str, Film, [u8; 3])>) -> Result<Sheet, String> {
    let pad = (20.0 * scale) as usize;
    let rows = rows();
    let mut frames: Vec<Vec<Option<Tile>>> = Vec::new();
    let mut titles = Vec::new();
    for (title, mut film, backdrop) in columns {
        let tiles = rows.iter().map(|(_, at)| film.at(renderer, fonts, *at)).collect::<Result<Vec<_>, _>>()?;
        frames.push(tiles);
        titles.push((title.to_string(), backdrop));
    }
    let tiles = || frames.iter().flatten().flatten();
    let cell_w = tiles().map(|t| t.w).max().unwrap_or(0) + pad * 2;
    let row_h = tiles().map(|t| t.h).max().unwrap_or(0) + pad;
    let mut canvas = Canvas::new(GUTTER + cell_w * frames.len(), HEADER + rows.len() * row_h + pad);
    let mut labels = Vec::new();
    for (col, ((title, backdrop), column)) in titles.iter().zip(&frames).enumerate() {
        let x = GUTTER + col * cell_w;
        canvas.fill(x, HEADER, cell_w, canvas.h - HEADER, *backdrop);
        labels.push(format!("{}\t{}\t13\t{title}", x + pad, 12));
        for (row, tile) in column.iter().enumerate() {
            let Some(tile) = tile else { continue };
            // Top-right placement: the box's right edge and top stay put.
            let (bx, by) = tile.anchored(x + cell_w - pad, HEADER + pad + row * row_h);
            canvas.blit(tile, bx, by);
        }
    }
    for (row, (label, _)) in rows.iter().enumerate() {
        labels.push(format!("8\t{}\t12\t{label}", HEADER + pad + row * row_h + 4));
    }
    Ok(Sheet { canvas, labels })
}

fn write(sheet: &Sheet, path: &Path) -> Result<(), String> {
    sheet.canvas.write_bmp(path)?;
    let labels = path.with_extension("bmp.labels.tsv");
    std::fs::write(&labels, sheet.labels.join("\n")).map_err(|e| format!("write {}: {e}", labels.display()))
}

/// Both images (see the module doc).
pub fn notice_filmstrip(path: &Path, scale: f32) -> Result<(), String> {
    let fonts = Fonts::new().map_err(|e| format!("fonts: {e}"))?;
    let mut renderer = Renderer::new().map_err(|e| format!("renderer: {e}"))?;
    let film = |stats: bool, emphasis: bool| steps(&fonts, scale, stats, emphasis).map(Film::new);
    let transform = sheet(&mut renderer, &fonts, scale, vec![
        ("In game, stats on", film(true, true)?, DARK),
        ("In game, stats on", film(true, true)?, LIGHT),
        ("No stats showing", film(false, true)?, DARK),
        ("No stats showing", film(false, true)?, LIGHT),
    ])?;
    write(&transform, path)?;
    let pair = sheet(&mut renderer, &fonts, scale, vec![
        ("Before: content swap", film(true, false)?, DARK),
        ("After: transformation", film(true, true)?, DARK),
    ])?;
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("notice");
    write(&pair, &PathBuf::from(path).with_file_name(format!("{stem}-before-after.bmp")))
}
