//! `--export-frames DIR`: the island's frames for the web preview, from the
//! real reducer, island state machine, layout and Direct2D renderer (no
//! window), at 1440p scale 1.0 on VALORANT's spot, every 1000/60 ms from the
//! trigger until the island is back at rest. Writes `frames.png` (an atlas of
//! the unique frames, straight-alpha RGBA = window alpha × pixel alpha, i.e.
//! what `UpdateLayeredWindow` composites) and `manifest.json`.
//!
//! Each step mirrors the app: `App::refresh` (reducer tick → content →
//! layout → `Island::update`) then `App::render` (tick, overshoot cap, draw).
//! The `old-*` sequences are the same notices with `emphasis` off, which is
//! the pre-transformation content swap (that path is unchanged).

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::anim::Frame;
use crate::fonts::Fonts;
use crate::island::{self, notice_layout, Content, Island};
use crate::layout::{stats_scene, Scene};
use crate::placement::{free_depth, grow, position, scale, Rect, Spot, Spots, EDGE};
use crate::preview::stats_states;
use crate::reducer::Reducer;
use crate::render::Renderer;

const MONITOR: Rect = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
const ATLAS_W: usize = 4096;
/// The island settles on the stats this long before the trigger.
const LEAD: Duration = Duration::from_secs(2);
const LIMIT: Duration = Duration::from_secs(8);

fn step_at(k: u64) -> Duration {
    Duration::from_nanos(k * 1_000_000_000 / 60)
}

struct Spec {
    name: &'static str,
    notice: &'static str,
    event: Value,
    stats: bool,
    emphasis: bool,
}

fn specs() -> Vec<Spec> {
    let save = json!({"type":"saved","path":"V:\\c\\VALORANT__2026-10-30__21-14-03.mp4","seconds":30.0});
    let fail = json!({"type":"save_failed","reason":"the output folder is unavailable"});
    let (saved, failed) = ("Clip saved · VALORANT · 0:30", "Save failed · the output folder is unavailable");
    let spec = |name, notice, event: &Value, stats, emphasis| Spec { name, notice, event: event.clone(), stats, emphasis };
    vec![
        spec("new-save", saved, &save, true, true),
        spec("old-save", saved, &save, true, false),
        spec("new-fail", failed, &fail, true, true),
        spec("old-fail", failed, &fail, true, false),
        spec("new-save-nostats", saved, &save, false, true),
        spec("old-save-nostats", saved, &save, false, false),
    ]
}

/// One drawn window: straight RGBA pixels and size.
struct Shot {
    w: usize,
    h: usize,
    rgba: Vec<u8>,
}

/// The overlay pipeline for one sequence, driven by timestamps.
struct Sim<'a> {
    fonts: &'a Fonts,
    spot: Spot,
    scale: f32,
    reducer: Reducer,
    island: Island,
    scene: Scene,
    scene_scale: f32,
    emphasis: bool,
}

impl Sim<'_> {
    /// `App::refresh` then `App::render`'s frame: `None` while hidden.
    fn step(&mut self, renderer: &mut Renderer, now: Instant) -> Result<Option<Shot>, String> {
        self.reducer.tick(now);
        let (view, session) = (self.reducer.view(), self.reducer.session());
        let under = island::stats_shown(view, session, true);
        match island::content(view, session, true) {
            Some(content) => {
                let content = match content {
                    Content::Notice(notice) => Content::Notice(crate::notice::NoticeView { emphasis: notice.emphasis && self.emphasis, ..notice }),
                    stats => stats,
                };
                let stats_w = || stats_scene(&stats_states()[0], self.scale, self.fonts).w;
                let (scene, scene_scale, (w, h)) = match &content {
                    Content::Notice(notice) => notice_layout(notice, self.scale, grow(&self.spot), || under.then(stats_w), self.fonts),
                    Content::Stats => {
                        let scene = stats_scene(&stats_states()[0], self.scale, self.fonts);
                        let size = (scene.w, scene.h);
                        (scene, self.scale, size)
                    }
                };
                self.island.update(Some((content, w, h)), now);
                (self.scene, self.scene_scale) = (scene, scene_scale);
            }
            None => self.island.update(None, now),
        }
        self.island.tick(now);
        if !self.island.shown() {
            return Ok(None);
        }
        let frame = self.island.frame(now);
        let max_h = free_depth(&self.spot, &MONITOR, self.scale);
        let frame = Frame { h: frame.h.min(max_h.max(self.scene.h)), ..frame };
        let (fade, sheen) = (self.island.content_alpha(now), self.island.sheen(now));
        let (w, h) = renderer.draw(self.fonts, &self.scene, frame, self.scene_scale, fade, sheen).map_err(|e| format!("draw: {e}"))?;
        let window = (frame.alpha.clamp(0.0, 1.0) * 255.0).round() as u32; // SourceConstantAlpha
        Ok(Some(Shot { w: w as usize, h: h as usize, rgba: straight(&renderer.pixels(w, h), window) }))
    }
}

/// Premultiplied BGRA × window alpha → straight RGBA (what the desktop composites).
fn straight(bgra: &[[u8; 4]], window: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(bgra.len() * 4);
    for &[b, g, r, a] in bgra {
        let alpha = (a as u32 * window + 127) / 255;
        let un = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8 };
        out.extend_from_slice(&if alpha == 0 { [0, 0, 0, 0] } else { [un(r), un(g), un(b), alpha as u8] });
    }
    out
}

/// Unique frames, in first-seen order.
#[derive(Default)]
struct Atlas {
    shots: Vec<Shot>,
    index: HashMap<(usize, usize, Vec<u8>), usize>,
}

impl Atlas {
    fn add(&mut self, shot: Shot) -> usize {
        let key = (shot.w, shot.h, shot.rgba.clone());
        *self.index.entry(key).or_insert_with(|| {
            self.shots.push(shot);
            self.shots.len() - 1
        })
    }

    /// Grid cells of the largest frame; returns (atlas w, h, cell origin per frame).
    fn layout(&self) -> (usize, usize, Vec<(usize, usize)>) {
        let cell_w = self.shots.iter().map(|s| s.w).max().unwrap_or(1);
        let cell_h = self.shots.iter().map(|s| s.h).max().unwrap_or(1);
        let cols = (ATLAS_W / cell_w).max(1);
        let rows = self.shots.len().div_ceil(cols).max(1);
        let origins = (0..self.shots.len()).map(|i| (i % cols * cell_w, i / cols * cell_h)).collect();
        (cols.min(self.shots.len().max(1)) * cell_w, rows * cell_h, origins)
    }

    fn png(&self, w: usize, h: usize, origins: &[(usize, usize)]) -> Vec<u8> {
        let mut rgba = vec![0u8; w * h * 4];
        for (shot, &(x, y)) in self.shots.iter().zip(origins) {
            for row in 0..shot.h {
                let dst = ((y + row) * w + x) * 4;
                rgba[dst..dst + shot.w * 4].copy_from_slice(&shot.rgba[row * shot.w * 4..(row + 1) * shot.w * 4]);
            }
        }
        crate::png::encode(w as u32, h as u32, &rgba)
    }
}

/// One timeline entry before the atlas is laid out: step, (frame id, w, h).
type Entry = (u64, Option<(usize, usize, usize)>);

/// One recorded sequence.
struct Run {
    /// The frame at rest before the trigger (`None`: nothing on screen).
    rest: Option<Entry>,
    frames: Vec<Entry>,
    /// The step the notice handed back (to the stats or to hidden).
    handback: Option<u64>,
}

/// Runs one sequence: the rest frame before the trigger, then every step
/// until the island rests (stats back and still, or hidden).
fn record(spec: &Spec, renderer: &mut Renderer, fonts: &Fonts, atlas: &mut Atlas) -> Result<Run, String> {
    let spot = Spots::embedded().lookup("valorant");
    let k = scale(MONITOR.height(), &spot);
    let start = Instant::now();
    let mut sim = Sim { fonts, spot, scale: k, reducer: Reducer::new(), island: Island::new(start), scene: Scene::default(), scene_scale: k, emphasis: spec.emphasis };
    let status = json!({"mod":"session","event":{"type":"status","state":"recording","overlay_stats":spec.stats,"overlay":{"enabled":true}}});
    let engine = json!({"mod":"engine","event":{"type":"status","source":"VALORANT","fps":60,"buffer_secs":120.0,"encoder":"h264_nvenc"}});
    sim.reducer.apply_envelope(&status, start);
    sim.reducer.apply_envelope(&engine, start);
    let mut entry = |shot: Option<Shot>, k: u64| (k, shot.map(|s| (s.w, s.h, atlas.add(s))).map(|(w, h, id)| (id, w, h)));
    sim.step(renderer, start)?;
    let t0 = start + LEAD;
    let rest = sim.step(renderer, t0 - Duration::from_millis(1))?.map(|s| entry(Some(s), 0)).filter(|(_, e)| e.is_some());
    sim.reducer.apply_engine_event(&spec.event, t0);
    let (mut frames, mut handback) = (Vec::new(), None);
    for k in 0.. {
        let now = t0 + step_at(k);
        if k % 60 == 0 && k > 0 {
            sim.reducer.apply_envelope(&engine, now); // the engine's 1 Hz status
        }
        let shot = sim.step(renderer, now)?;
        let showing_notice = matches!(sim.island.content(), Some(Content::Notice(_))) && sim.reducer.view().notice.is_some();
        if handback.is_none() && k > 0 && !showing_notice {
            handback = Some(k);
        }
        let resting = handback.is_some() && (!sim.island.shown() || !sim.island.animating(now));
        frames.push(entry(shot, k));
        if resting || step_at(k) >= LIMIT {
            break;
        }
    }
    Ok(Run { rest, frames, handback })
}

/// Milliseconds of step `k`, to the microsecond.
fn ms(k: u64) -> f64 {
    (step_at(k).as_secs_f64() * 1e6).round() / 1e3
}

/// One sequence's manifest entry; `place` turns (frame id, w, h) into its atlas rect and offset.
fn sequence(spec: &Spec, run: &Run, place: &dyn Fn((usize, usize, usize)) -> Value) -> Value {
    let frames: Vec<Value> = run
        .frames
        .iter()
        .map(|&(k, shot)| {
            let mut entry = shot.map(place).unwrap_or_else(|| json!({"frame_file":null,"frame":null,"w":0,"h":0}));
            entry["k"] = json!(k);
            entry["t_ms"] = json!(ms(k));
            entry
        })
        .collect();
    json!({
        "notice": spec.notice, "stats": spec.stats, "transformation": spec.emphasis,
        "rest": run.rest.and_then(|(_, shot)| shot).map(place),
        "handback_ms": run.handback.map(ms),
        "end_ms": run.frames.last().map_or(0.0, |&(k, _)| ms(k)),
        "frames": frames,
    })
}

/// Both files (see the module doc).
pub fn export(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let fonts = Fonts::new().map_err(|e| format!("fonts: {e}"))?;
    let mut renderer = Renderer::new().map_err(|e| format!("renderer: {e}"))?;
    let mut atlas = Atlas::default();
    let mut runs = Vec::new();
    for spec in specs() {
        let run = record(&spec, &mut renderer, &fonts, &mut atlas)?;
        runs.push((spec, run));
    }
    let (aw, ah, origins) = atlas.layout();
    let spot = Spots::embedded().lookup("valorant");
    let k = scale(MONITOR.height(), &spot);
    let edge = (EDGE * k).round() as i32;
    let anchor = (MONITOR.right - edge, MONITOR.top + edge); // top-right: the window's top-right corner
    let place = |(id, w, h): (usize, usize, usize)| {
        let (x, y) = position(spot.anchor, &MONITOR, w as i32, edge);
        let (sx, sy) = origins[id];
        json!({"frame_file":"frames.png","frame":id,"sx":sx,"sy":sy,"x":x - anchor.0,"y":y - anchor.1,"w":w,"h":h})
    };
    let sequences: serde_json::Map<String, Value> =
        runs.iter().map(|(spec, run)| (spec.name.to_string(), sequence(spec, run, &place))).collect();
    let stats_w = stats_scene(&stats_states()[0], k, &fonts).w.ceil() as i32;
    let (sx, sy) = position(spot.anchor, &MONITOR, stats_w, edge);
    let manifest = json!({
        "source": "recall-overlay --export-frames: real reducer, island state machine, layout and Direct2D renderer; no window",
        "fps": 60, "step_ms": 1000.0 / 60.0, "scale": k, "monitor": [2560, 1440], "game": "VALORANT",
        "anchor": {"corner": "top-right", "point": [anchor.0, anchor.1],
            "rule": "the window's top-right corner sits on `point` (12 px in from the monitor's top-right at 1440p); a frame's top-left on screen = point + (x, y), x = -w, y = 0"},
        "valorant_stats_rect": {"x": sx, "y": sy, "w": stats_w, "h": (crate::placement::ISLAND_H * k).ceil() as i32},
        "atlas": {"file": "frames.png", "w": aw, "h": ah, "frames": atlas.shots.len(),
            "pixels": "straight (un-premultiplied) RGBA; alpha = window alpha x pixel alpha"},
        "playback": "before the trigger show `rest`; entry k shows from t_ms until the next entry; after `end_ms` show `rest` (null = nothing)",
        "sequences": sequences,
    });
    let png = atlas.png(aw, ah, &origins);
    std::fs::write(dir.join("frames.png"), png).map_err(|e| format!("write frames.png: {e}"))?;
    let text = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("manifest.json"), text).map_err(|e| format!("write manifest.json: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiplied_window_pixels_become_straight_rgba_times_window_alpha() {
        // The pill body (18 grey at 94%), a half-covered edge, a clear corner.
        let bgra = [[17, 17, 17, 240], [9, 9, 9, 120], [0, 0, 0, 0]];
        assert_eq!(straight(&bgra, 255), vec![18, 18, 18, 240, 19, 19, 19, 120, 0, 0, 0, 0]);
        let faded = straight(&bgra, 128);
        assert_eq!(&faded[..4], &[18, 18, 18, 120], "window alpha scales alpha, never the colour");
        assert_eq!(straight(&[[255, 0, 0, 255]], 255), vec![0, 0, 255, 255], "BGRA in, RGBA out");
        assert_eq!(straight(&[[3, 3, 3, 1]], 0), vec![0, 0, 0, 0], "fully faded: clear");
    }

    #[test]
    fn steps_land_on_whole_milliseconds_every_three_frames() {
        assert_eq!(step_at(180), Duration::from_secs(3), "SAVED_FOR lands exactly on a step");
        assert_eq!(step_at(3), Duration::from_millis(50));
    }
}
