//! Offline renders through the real renderer and fonts (evidence, no window):
//! `--preview-sheet PATH.bmp` = every island state on a dark and a light
//! backdrop plus the appear/hide animation frames; `--smoke-render PATH.bmp`
//! = the fixture's settled island. Action notices show at their grown size;
//! their transformation over time is `--preview-notice` (`filmstrip.rs`).

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::anim::{Frame, DURATION};
use crate::fonts::Fonts;
use crate::island::{content, Content, Island};
use crate::layout::{leader_scene, notice_scene, stats_scene, Scene, StatsContent};
use crate::notice::NoticeView;
use crate::placement::{grow, DEFAULT_SPOT};
use crate::hint::{card_scene, CardLayout, HintOption, HintView, GAP};
use crate::presence::{CardLook, Look, FADE_OUT, POP, UNRAVEL};
use crate::reducer::Reducer;
use crate::render::{Layers, Renderer};

pub const DARK: [u8; 3] = [0x23, 0x1e, 0x1b]; // BGR
pub const LIGHT: [u8; 3] = [0xa3, 0x96, 0x8a];

pub struct Tile {
    pub w: usize,
    pub h: usize,
    px: Vec<[u8; 4]>,
    /// The window's margin around the anchored box.
    pub inset: usize,
    /// The anchored box's width.
    pub box_w: usize,
}

impl Tile {
    /// Where the window goes so its box's top-right corner is (`right`, `top`),
    /// as the top-right anchor places it.
    pub fn anchored(&self, right: usize, top: usize) -> (usize, usize) {
        (right.saturating_sub(self.box_w + self.inset), top.saturating_sub(self.inset))
    }
}

pub fn tile(renderer: &mut Renderer, fonts: &Fonts, scene: &Scene, look: Look, scale: f32, layers: Layers) -> Result<Tile, String> {
    let (w, h) = renderer.draw(fonts, scene, look, scale, layers).map_err(|e| format!("draw: {e}"))?;
    let px = renderer.pixels(w, h);
    // Window opacity (UpdateLayeredWindow's constant alpha) applied here for the stills.
    let px = px.into_iter().map(|p| p.map(|c| (c as f32 * look.alpha).round() as u8)).collect();
    Ok(Tile { w: w as usize, h: h as usize, px, inset: look.margin as usize, box_w: look.span_w().ceil() as usize })
}

pub struct Canvas {
    pub w: usize,
    pub h: usize,
    bgr: Vec<[u8; 3]>,
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Self {
        Self { w, h, bgr: vec![[0x10, 0x10, 0x10]; w * h] }
    }

    pub fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, color: [u8; 3]) {
        for row in y..(y + h).min(self.h) {
            for col in x..(x + w).min(self.w) {
                self.bgr[row * self.w + col] = color;
            }
        }
    }

    /// Premultiplied-alpha "over".
    pub fn blit(&mut self, tile: &Tile, x: usize, y: usize) {
        for row in 0..tile.h {
            for col in 0..tile.w {
                let (cx, cy) = (x + col, y + row);
                if cx >= self.w || cy >= self.h {
                    continue;
                }
                let src = tile.px[row * tile.w + col];
                let keep = 1.0 - src[3] as f32 / 255.0;
                let dst = &mut self.bgr[cy * self.w + cx];
                for c in 0..3 {
                    dst[c] = (src[c] as f32 + dst[c] as f32 * keep).round().min(255.0) as u8;
                }
            }
        }
    }

    pub fn write_bmp(&self, path: &Path) -> Result<(), String> {
        let row_bytes = self.w * 3;
        let stride = (row_bytes + 3) & !3;
        let size = stride * self.h;
        let mut bmp = Vec::with_capacity(54 + size);
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&(54 + size as u32).to_le_bytes());
        bmp.extend_from_slice(&[0; 4]);
        bmp.extend_from_slice(&54u32.to_le_bytes());
        bmp.extend_from_slice(&40u32.to_le_bytes());
        bmp.extend_from_slice(&(self.w as i32).to_le_bytes());
        bmp.extend_from_slice(&(self.h as i32).to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&24u16.to_le_bytes());
        bmp.extend_from_slice(&[0; 4]);
        bmp.extend_from_slice(&(size as u32).to_le_bytes());
        bmp.extend_from_slice(&[0; 16]);
        for row in (0..self.h).rev() {
            for px in &self.bgr[row * self.w..(row + 1) * self.w] {
                bmp.extend_from_slice(px);
            }
            bmp.resize(bmp.len() + (stride - row_bytes), 0);
        }
        std::fs::write(path, bmp).map_err(|e| format!("write {}: {e}", path.display()))
    }
}

/// The settled island for one scene (laid out at `scale`, island `size`), on the dark backdrop.
pub fn snapshot(path: &Path, renderer: &mut Renderer, fonts: &Fonts, scene: &Scene, scale: f32, size: (f32, f32)) -> Result<(), String> {
    let pad = (24.0 * scale) as usize;
    let t = tile(renderer, fonts, scene, Look::open(Frame { w: size.0, h: size.1, alpha: 1.0 }), scale, Layers::default())?;
    let mut canvas = Canvas::new(t.w + pad * 2, t.h + pad * 2);
    canvas.fill(0, 0, canvas.w, canvas.h, DARK);
    canvas.blit(&t, pad, pad);
    canvas.write_bmp(path)
}

pub fn notice_from(events: &[Value]) -> Option<NoticeView> {
    let (mut reducer, now) = (Reducer::new(), Instant::now());
    reducer.apply_envelope(&json!({"mod":"session","event":{"type":"status","state":"recording","overlay":{"enabled":true}}}), now);
    for event in events {
        reducer.apply_envelope(event, now);
    }
    match content(reducer.view(), reducer.session(), true) {
        Some(Content::Notice(notice)) => Some(notice),
        _ => None,
    }
}

fn notices() -> Vec<NoticeView> {
    let session = |kind: &str, title: &str, detail: &str| {
        json!({"mod":"session","event":{"type":"notice","kind":kind,"title":title,"detail":detail}})
    };
    let engine = |event: Value| json!({"mod":"engine","event":event});
    let chord = json!({"mod":"session","event":{"type":"chord","active":true,"leader":"Alt `","options":[
        {"key":"1","label":"Bookmark"},{"key":"2","label":"Save 30 s"},{"key":"3","label":"Screenshot"},
        {"key":"4","label":"Record"},{"key":"5","label":"Performance stats"}]}});
    [
        session("game", "Recording", "VALORANT"),
        session("desktop", "Recording", "Desktop"),
        session("paused", "Recording paused", "VALORANT"),
        session("waiting", "Waiting for a game", "Not recording"),
        session("encoder", "Encoder problem", "Recorder stopped"),
        json!({"mod":"session","event":{"type":"saving","seconds":120}}),
        engine(json!({"type":"saved","path":"V:\\c\\Rocket-League__2026-10-26__10-00-00.mp4","seconds":120.0})),
        engine(json!({"type":"mic_muted","muted":true})),
        engine(json!({"type":"save_failed","reason":"the output folder is unavailable"})),
        engine(json!({"type":"bookmarked","at_secs":12.5})),
        engine(json!({"type":"screenshot","path":"V:\\c\\VALORANT__2026-10-26__10-00-05.png"})),
        chord,
    ]
    .iter()
    .filter_map(|event| notice_from(std::slice::from_ref(event)))
    .collect()
}

pub fn stats_states() -> Vec<StatsContent> {
    let base = StatsContent {
        recording: true,
        replay_secs: 120,
        fps: Some(144),
        low1: Some(98),
        gpu: Some(71),
        gpu_temp: Some(64),
        cpu: Some(23),
        ..StatsContent::default()
    };
    vec![
        base.clone(),
        StatsContent { fps: Some(237), low1: Some(181), gpu: Some(97), gpu_temp: Some(86), cpu: Some(61), ..base.clone() },
        StatsContent { fps: None, low1: None, fps_note: Some("No frame timing".into()), gpu: None, gpu_temp: None, ..base.clone() },
        StatsContent { recording: false, paused: true, replay_secs: 47, ..base.clone() },
        StatsContent { fps: None, low1: None, cpu: None, ..base },
    ]
}

/// The shortcut hint: the "Alt `" pill with its card, each layout.
fn hint_tiles(renderer: &mut Renderer, fonts: &Fonts, scale: f32) -> Result<Vec<Tile>, String> {
    let option = |key: &str, label: &str| HintOption { key: key.into(), label: label.into() };
    let hint = HintView {
        leader: "Alt `".into(),
        options: vec![option("1", "Bookmark"), option("2", "Save 30 s"), option("3", "Screenshot"), option("4", "Record"), option("5", "Performance stats")],
    };
    let pill = leader_scene(&hint.leader, scale, fonts);
    [CardLayout::Dropdown, CardLayout::TwoColumn, CardLayout::Row]
        .into_iter()
        .map(|layout| {
            let card = card_scene(&hint, layout, scale, fonts);
            let card_look = CardLook { w: card.w, h: card.h, gap: GAP * scale, progress: 1.0 };
            let look = Look { card: Some(card_look), ..Look::open(Frame { w: pill.w, h: pill.h, alpha: 1.0 }) };
            tile(renderer, fonts, &pill, look, scale, Layers { card: Some(&card), ..Layers::default() })
        })
        .collect()
}

/// Every state on both backdrops, then the appear and hide frames.
pub fn sheet(path: &Path, scale: f32) -> Result<(), String> {
    let fonts = Fonts::new().map_err(|e| format!("fonts: {e}"))?;
    let mut renderer = Renderer::new().map_err(|e| format!("renderer: {e}"))?;
    // Action notices at their grown size (an unscanned spot: full growth).
    let notice_scale = |n: &NoticeView| if n.emphasis { scale * grow(&DEFAULT_SPOT) } else { scale };
    let mut scenes: Vec<(Scene, f32)> = notices().iter().map(|n| (notice_scene(n, notice_scale(n), &fonts), notice_scale(n))).collect();
    scenes.extend(stats_states().iter().map(|s| (stats_scene(s, scale, &fonts), scale)));
    let pad = (24.0 * scale) as usize;
    let mut tiles = Vec::new();
    for (scene, s) in &scenes {
        tiles.push(tile(&mut renderer, &fonts, scene, Look::open(Frame { w: scene.w, h: scene.h, alpha: 1.0 }), *s, Layers::default())?);
    }
    tiles.extend(hint_tiles(&mut renderer, &fonts, scale)?);
    let scenes: Vec<Scene> = scenes.into_iter().map(|(scene, _)| scene).collect();
    // Appear (left column: circle, unravel, values) and hide (right column,
    // the reverse) of the stats island, 10 steps each.
    let stats = &scenes[scenes.len() - stats_states().len()];
    let (mut appear, mut hide) = (Vec::new(), Vec::new());
    let start = Instant::now();
    let mut island = Island::new(start);
    island.update(Some((Content::Stats, stats.w, stats.h)), start);
    let hide_at = start + Duration::from_secs(2);
    let mut leaving = Island::new(start);
    leaving.update(Some((Content::Stats, stats.w, stats.h)), start);
    leaving.update(None, hide_at);
    let (appear_for, hide_for) = (POP + UNRAVEL + DURATION, FADE_OUT + POP + UNRAVEL);
    for step in 0..=10u32 {
        let (a, b) = (start + appear_for * step / 10, hide_at + hide_for * step / 10);
        appear.push(tile(&mut renderer, &fonts, stats, island.look(a, f32::INFINITY), scale, Layers::default())?);
        hide.push(tile(&mut renderer, &fonts, stats, leaving.look(b, f32::INFINITY), scale, Layers::default())?);
    }
    let cell_w = tiles.iter().map(|t| t.w).max().unwrap_or(0) + pad * 2;
    let row_h = tiles.iter().map(|t| t.h).max().unwrap_or(0) + pad;
    let rows = tiles.len() + appear.len();
    let mut canvas = Canvas::new(cell_w * 2, rows * row_h + pad);
    canvas.fill(0, 0, cell_w, canvas.h, DARK);
    canvas.fill(cell_w, 0, cell_w, canvas.h, LIGHT);
    for (row, t) in tiles.iter().enumerate() {
        canvas.blit(t, pad, pad + row * row_h);
        canvas.blit(t, cell_w + pad, pad + row * row_h);
    }
    let right = pad + stats.w.ceil() as usize;
    for (step, (a, b)) in appear.iter().zip(&hide).enumerate() {
        let y = pad + (tiles.len() + step) * row_h;
        // Right edges on the settled island's, as the top-right anchor places them.
        let ((ax, ay), (bx, by)) = (a.anchored(right, y), b.anchored(right, y));
        canvas.blit(a, ax, ay);
        canvas.blit(b, cell_w + bx, by);
    }
    canvas.write_bmp(path)
}
