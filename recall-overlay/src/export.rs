//! `--export-frames DIR`: the island's frames for the web preview, from the
//! real reducer, island state machine, layout and Direct2D renderer (no
//! window), at 1440p scale 1.0 on VALORANT's spot, every 1000/60 ms from the
//! trigger until the island rests. Writes `frames.png` (an atlas of the
//! unique frames, straight-alpha RGBA = window alpha × pixel alpha, i.e.
//! what `UpdateLayeredWindow` composites) and `manifest.json`.
//!
//! Sequences are timelines of pipe events (`saving`, engine `saved` /
//! `save_failed`, a status turning the stats on or off). `old-*` show the
//! notice the pre-change way (`export_sim::Sim::legacy`).

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::export_sim::{Atlas, Sim};
use crate::fonts::Fonts;
use crate::layout::stats_scene;
use crate::placement::{position, scale, window_origin, Anchor, Rect, Spot, Spots, EDGE, ISLAND_H};
use crate::preview::stats_states;
use crate::render::Renderer;

const MONITOR: Rect = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
const ATLAS_W: usize = 4096;
/// The island settles before the trigger this long.
const LEAD: Duration = Duration::from_secs(2);
const LIMIT: Duration = Duration::from_secs(10);

fn step_at(k: u64) -> Duration {
    Duration::from_nanos(k * 1_000_000_000 / 60)
}

/// Milliseconds of step `k`, to the microsecond.
fn ms(k: u64) -> f64 {
    (step_at(k).as_secs_f64() * 1e6).round() / 1e3
}

/// The controller's session status: recording VALORANT, the stats toggle.
fn status(stats: bool) -> Value {
    json!({"mod":"session","event":{"type":"status","state":"recording","overlay_stats":stats,"overlay":{"enabled":true},
        "target":{"name":"VALORANT","exe":"VALORANT-Win64-Shipping.exe","pid":42}}})
}

struct Spec {
    name: &'static str,
    notice: &'static str,
    /// The stats are on before the trigger.
    stats: bool,
    legacy: bool,
    /// (ms after the trigger, pipe envelope).
    events: Vec<(u64, Value)>,
    /// When the save's outcome lands (ms), if the sequence has one.
    saved_ms: Option<u64>,
    /// A spot other than VALORANT's (the forced two-column card).
    spot: Option<Spot>,
}

fn specs() -> Vec<Spec> {
    let saved = json!({"mod":"engine","event":{"type":"saved","path":"V:\\c\\VALORANT__2026-10-30__21-14-03.mp4","seconds":30.0}});
    let failed = json!({"mod":"engine","event":{"type":"save_failed","reason":"the output folder is unavailable"}});
    let saving = json!({"mod":"session","event":{"type":"saving","seconds":30}});
    let (clip, fail) = ("Clip saved · VALORANT · 0:30", "Save failed · the output folder is unavailable");
    let at = |ms: u64, v: &Value| (ms, v.clone());
    let chord = json!({"mod":"session","event":{"type":"chord","active":true,"leader":"Alt `","options":[
        {"key":"1","label":"Bookmark"},{"key":"2","label":"Save 30 s"},{"key":"3","label":"Screenshot"},
        {"key":"4","label":"Record"},{"key":"5","label":"Performance stats"}]}});
    let spec = |name, notice, stats, legacy, events: Vec<(u64, Value)>, saved_ms| Spec { name, notice, stats, legacy, events, saved_ms, spot: None };
    let hint = |name, stats, spot| Spec { spot, ..spec(name, "Shortcut hint (Alt ` then a key)", stats, false, vec![at(0, &chord)], None) };
    // 200 free rows: 138 px under the pill, too shallow for five rows (180), room for three (116).
    let shallow = Some(Spot { anchor: Anchor::TopRight, free_px: Some(200) });
    let unscanned = Some(Spot { anchor: Anchor::TopRight, free_px: None });
    vec![
        spec("new-save", clip, true, false, vec![at(0, &saved)], Some(0)),
        spec("old-save", clip, true, true, vec![at(0, &saved)], Some(0)),
        spec("new-fail", fail, true, false, vec![at(0, &failed)], Some(0)),
        spec("old-fail", fail, true, true, vec![at(0, &failed)], Some(0)),
        spec("new-save-nostats", clip, false, false, vec![at(0, &saved)], Some(0)),
        spec("old-save-nostats", clip, false, true, vec![at(0, &saved)], Some(0)),
        spec("ring-save", clip, true, false, vec![at(0, &saving), at(1_800, &saved)], Some(1_800)),
        spec("ring-save-long", clip, true, false, vec![at(0, &saving), at(4_000, &saved)], Some(4_000)),
        spec("ring-fail", fail, true, false, vec![at(0, &saving), at(1_800, &failed)], Some(1_800)),
        spec("ring-save-nostats", clip, false, false, vec![at(0, &saving), at(1_800, &saved)], Some(1_800)),
        spec("stats-on", "Stats on", false, false, vec![at(0, &status(true))], None),
        spec("stats-off", "Stats off", true, false, vec![at(0, &status(false))], None),
        // VALORANT's own scan picks the card; the forced spots show the other layouts.
        hint("hint-valorant", false, None),
        hint("hint-valorant-stats", true, None),
        hint("hint-dropdown", false, unscanned),
        hint("hint-twocol", true, shallow),
    ]
}

/// One timeline entry before the atlas is laid out: step, (frame id, box w, margin).
type Entry = (u64, Option<(usize, f32, f32)>);

struct Run {
    /// The frame at rest before the trigger (`None`: nothing on screen).
    rest: Option<(usize, f32, f32)>,
    frames: Vec<Entry>,
    /// The step the notice handed back (to the stats or to nothing).
    handback: Option<u64>,
    /// The shortcut hint card's layout, when the sequence opened one.
    card: Option<crate::hint::CardLayout>,
}

/// Runs one sequence: the rest frame before the trigger, then every step
/// until the island rests after the last event.
fn record(spec: &Spec, renderer: &mut Renderer, fonts: &Fonts, atlas: &mut Atlas) -> Result<Run, String> {
    let spots = Spots::embedded();
    let spot = spec.spot.unwrap_or_else(|| spots.lookup("valorant"));
    let start = Instant::now();
    let mut sim = Sim::new(fonts, spot, MONITOR, scale(MONITOR.height(), &spot), spec.legacy, start);
    sim.hud = if spec.spot.is_some() { None } else { spots.hud("valorant") };
    let engine = json!({"mod":"engine","event":{"type":"status","source":"VALORANT","fps":60,"buffer_secs":120.0,"encoder":"h264_nvenc"}});
    sim.reducer.apply_envelope(&status(spec.stats), start);
    sim.reducer.apply_envelope(&engine, start);
    let mut keep = |shot: Option<crate::export_sim::Shot>| shot.map(|s| (s.box_w, s.margin, atlas.add(s))).map(|(b, m, id)| (id, b, m));
    sim.step(renderer, start)?;
    let t0 = start + LEAD;
    let rest = keep(sim.step(renderer, t0 - Duration::from_millis(1))?);
    let last = spec.events.iter().map(|(at, _)| *at).max().unwrap_or(0);
    let (mut frames, mut handback, mut events, mut seen) = (Vec::new(), None, spec.events.iter().peekable(), false);
    for k in 0.. {
        let now = t0 + step_at(k);
        while let Some((_, event)) = events.next_if(|(at, _)| step_at(k) >= Duration::from_millis(*at)) {
            sim.reducer.apply_envelope(event, now);
        }
        if k % 60 == 0 && k > 0 {
            sim.reducer.apply_envelope(&engine, now); // the engine's 1 Hz status
        }
        let shot = sim.step(renderer, now)?;
        let after = step_at(k) >= Duration::from_millis(last);
        let notice = sim.reducer.view().notice.is_some() || sim.reducer.view().hint.is_some();
        seen |= notice;
        if handback.is_none() && seen && after && !notice {
            handback = Some(k);
        }
        let gone = shot.is_none() || !sim.island.shown();
        let resting = after && !notice && (gone || !sim.island.animating(now));
        frames.push((k, keep(shot)));
        if resting || step_at(k) >= LIMIT {
            break;
        }
    }
    Ok(Run { rest, frames, handback, card: sim.card_layout() })
}

/// One sequence's manifest entry; `place` turns (frame id, box w, margin)
/// into its atlas rect and offset from the anchor point.
fn sequence(spec: &Spec, run: &Run, place: &dyn Fn((usize, f32, f32)) -> Value) -> Value {
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
        "notice": spec.notice, "stats_before": spec.stats, "legacy": spec.legacy,
        "free_px": spec.spot.and_then(|s| s.free_px),
        "card_layout": run.card,
        "events_ms": spec.events.iter().map(|(at, v)| json!({"t_ms": at, "event": v})).collect::<Vec<_>>(),
        "saved_ms": spec.saved_ms,
        "rest": run.rest.map(place),
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
    let (aw, ah, origins) = atlas.layout(ATLAS_W);
    let spot = Spots::embedded().lookup("valorant");
    let k = scale(MONITOR.height(), &spot);
    let edge = (EDGE * k).round() as i32;
    let anchor = (MONITOR.right - edge, MONITOR.top + edge); // top-right: the box's top-right corner
    let place = |(id, box_w, margin): (usize, f32, f32)| {
        let (x, y) = window_origin(spot.anchor, &MONITOR, box_w, margin, edge);
        let (shot, (sx, sy)) = (&atlas.shots[id], origins[id]);
        json!({"frame_file":"frames.png","frame":id,"sx":sx,"sy":sy,"x":x - anchor.0,"y":y - anchor.1,"w":shot.w,"h":shot.h})
    };
    let sequences: serde_json::Map<String, Value> =
        runs.iter().map(|(spec, run)| (spec.name.to_string(), sequence(spec, run, &place))).collect();
    let stats_w = stats_scene(&stats_states()[0], k, &fonts).w.ceil() as i32;
    let (sx, sy) = position(spot.anchor, &MONITOR, stats_w, edge);
    let manifest = json!({
        "source": "recall-overlay --export-frames: real reducer, island state machine, layout and Direct2D renderer; no window",
        "fps": 60, "step_ms": 1000.0 / 60.0, "scale": k, "monitor": [2560, 1440], "game": "VALORANT",
        "anchor": {"corner": "top-right", "point": [anchor.0, anchor.1],
            "rule": "the island's box has its top-right corner on `point` (12 px in from the monitor's top-right at 1440p); the window may add a margin around the box while the circle pops (its overshoot). A frame's top-left on screen = point + (x, y)"},
        "valorant_stats_rect": {"x": sx, "y": sy, "w": stats_w, "h": (ISLAND_H * k).ceil() as i32},
        "atlas": {"file": "frames.png", "w": aw, "h": ah, "frames": atlas.shots.len(),
            "pixels": "straight (un-premultiplied) RGBA; alpha = window alpha x pixel alpha"},
        "playback": "before the trigger show `rest` (null = nothing); entry k shows from t_ms until the next entry; the last entry stays after `end_ms`",
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
    fn steps_land_on_whole_milliseconds_every_three_frames() {
        assert_eq!(step_at(180), Duration::from_secs(3), "SAVED_FOR lands exactly on a step");
        assert_eq!(step_at(108), Duration::from_millis(1_800), "the ring sequences' saved lands on a step");
        assert_eq!(step_at(3), Duration::from_millis(50));
    }
}
