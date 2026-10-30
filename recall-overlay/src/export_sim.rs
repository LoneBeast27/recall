//! `--export-frames`' per-step pipeline: each step mirrors the app
//! (`App::refresh`: reducer tick → content → layout → `Island::update`; then
//! `App::render`: tick, look, draw), plus the unique-frame atlas.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::anim::{ease_out, Frame, DURATION};
use crate::fonts::Fonts;
use crate::island::{self, notice_layout, Content, Island};
use crate::layout::{stats_scene, Scene};
use crate::notice::NoticeView;
use crate::placement::{free_depth, grow, Rect, Spot};
use crate::presence::Look;
use crate::preview::stats_states;
use crate::reducer::Reducer;
use crate::render::Renderer;

/// One drawn window: straight RGBA pixels, size, and the box the anchor places.
pub struct Shot {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
    pub box_w: f32,
    pub margin: f32,
}

/// Master's appear / hide from nothing, before the circle: a transparent
/// pill 35% wide (at least round) and 75% high eased out to full size in
/// 200 ms, content fading in with it; hiding eased back with the content
/// fading with the window. Reproduced (export only) so `old-*-nostats`
/// stays the real "before".
pub fn legacy_look(full: Frame, shown_for: Duration, hidden_for: Option<Duration>) -> Look {
    let small = Frame { w: (full.w * 0.35).max(full.h).min(full.w), h: full.h * 0.75, alpha: 0.0 };
    let t = |d: Duration| ease_out(d.as_secs_f32() / DURATION.as_secs_f32());
    let (frame, fade) = match hidden_for {
        None => (small.lerp(full, t(shown_for)), t(shown_for)),
        Some(gone) => {
            let f = full.lerp(small, t(gone));
            (f, f.alpha)
        }
    };
    Look { content_alpha: fade, icon_alpha: fade, ..Look::open(frame) }
}

/// The overlay pipeline for one sequence, driven by timestamps.
pub struct Sim<'a> {
    pub fonts: &'a Fonts,
    pub spot: Spot,
    pub monitor: Rect,
    pub scale: f32,
    pub reducer: Reducer,
    pub island: Island,
    scene: Scene,
    scene_scale: f32,
    /// Show notices the pre-change way: no transformation, no save badge,
    /// master's appear / hide from nothing.
    pub legacy: bool,
    /// (shown since, hidden since) for the legacy appear / hide.
    legacy_times: (Option<Instant>, Option<Instant>),
}

impl<'a> Sim<'a> {
    pub fn new(fonts: &'a Fonts, spot: Spot, monitor: Rect, scale: f32, legacy: bool, now: Instant) -> Self {
        Self {
            fonts,
            spot,
            monitor,
            scale,
            reducer: Reducer::new(),
            island: Island::new(now),
            scene: Scene::default(),
            scene_scale: scale,
            legacy,
            legacy_times: (None, None),
        }
    }

    /// The content now, laid out: (content, scene, scene scale, island size).
    fn layout(&self) -> Option<(Content, Scene, f32, (f32, f32))> {
        let (view, session) = (self.reducer.view(), self.reducer.session());
        let under = island::stats_shown(view, session, true);
        let content = match island::content(view, session, true)? {
            Content::Notice(n) if self.legacy => Content::Notice(NoticeView { emphasis: false, save: None, ..n }),
            other => other,
        };
        let stats = || stats_scene(&stats_states()[0], self.scale, self.fonts);
        let (scene, scale, size) = match &content {
            Content::Notice(notice) => notice_layout(notice, self.scale, grow(&self.spot), || under.then(|| stats().w), self.fonts),
            Content::Stats => {
                let scene = stats();
                let size = (scene.w, scene.h);
                (scene, self.scale, size)
            }
        };
        Some((content, scene, scale, size))
    }

    /// One app tick at `now`: `None` while nothing is on screen.
    pub fn step(&mut self, renderer: &mut Renderer, now: Instant) -> Result<Option<Shot>, String> {
        self.reducer.tick(now);
        match self.layout() {
            Some((content, scene, scale, (w, h))) => {
                self.island.update(Some((content, w, h)), now);
                (self.scene, self.scene_scale) = (scene, scale);
                if self.legacy_times.0.is_none() || self.legacy_times.1.is_some() {
                    self.legacy_times = (Some(now), None);
                }
            }
            None => {
                self.island.update(None, now);
                if self.legacy_times.0.is_some() && self.legacy_times.1.is_none() {
                    self.legacy_times.1 = Some(now);
                }
            }
        }
        self.island.tick(now);
        let max_h = free_depth(&self.spot, &self.monitor, self.scale).max(self.scene.h);
        let look = self.look(now, max_h);
        let Some(look) = look else { return Ok(None) };
        let (sheen, badge) = (self.island.sheen(now), self.island.badge(now));
        let (w, h) = renderer.draw(self.fonts, &self.scene, look, self.scene_scale, sheen, badge).map_err(|e| format!("draw: {e}"))?;
        let window = (look.alpha.clamp(0.0, 1.0) * 255.0).round() as u32; // SourceConstantAlpha
        let rgba = straight(&renderer.pixels(w, h), window);
        Ok(Some(Shot { w: w as usize, h: h as usize, rgba, box_w: look.box_w, margin: look.margin }))
    }

    /// The island's look, or master's for a legacy appear / hide from nothing.
    fn look(&self, now: Instant, max_h: f32) -> Option<Look> {
        let stats_ever = self.reducer.session().overlay_stats;
        if self.legacy && !stats_ever {
            let (Some(shown), hidden) = self.legacy_times else { return None };
            let gone = hidden.map(|at| now.saturating_duration_since(at));
            if gone.is_some_and(|d| d >= DURATION) {
                return None;
            }
            return Some(legacy_look(self.island.frame(now), now.saturating_duration_since(shown), gone));
        }
        self.island.shown().then(|| self.island.look(now, max_h))
    }
}

/// Premultiplied BGRA × window alpha → straight RGBA (what the desktop composites).
pub fn straight(bgra: &[[u8; 4]], window: u32) -> Vec<u8> {
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
pub struct Atlas {
    pub shots: Vec<Shot>,
    index: HashMap<(usize, usize, Vec<u8>), usize>,
}

impl Atlas {
    pub fn add(&mut self, shot: Shot) -> usize {
        let key = (shot.w, shot.h, shot.rgba.clone());
        *self.index.entry(key).or_insert_with(|| {
            self.shots.push(shot);
            self.shots.len() - 1
        })
    }

    /// Grid cells of the largest frame, at most `width` wide; returns
    /// (atlas w, h, cell origin per frame).
    pub fn layout(&self, width: usize) -> (usize, usize, Vec<(usize, usize)>) {
        let cell_w = self.shots.iter().map(|s| s.w).max().unwrap_or(1);
        let cell_h = self.shots.iter().map(|s| s.h).max().unwrap_or(1);
        let cols = (width / cell_w).max(1);
        let rows = self.shots.len().div_ceil(cols).max(1);
        let origins = (0..self.shots.len()).map(|i| (i % cols * cell_w, i / cols * cell_h)).collect();
        (cols.min(self.shots.len().max(1)) * cell_w, rows * cell_h, origins)
    }

    pub fn png(&self, w: usize, h: usize, origins: &[(usize, usize)]) -> Vec<u8> {
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
    fn the_legacy_appear_is_masters_collapsed_pill_easing_out() {
        let full = Frame { w: 300.0, h: 44.0, alpha: 1.0 };
        let first = legacy_look(full, Duration::ZERO, None);
        assert_eq!((first.box_w, first.box_h, first.alpha, first.content_alpha), (105.0, 33.0, 0.0, 0.0));
        let open = legacy_look(full, DURATION, None);
        assert_eq!((open.box_w, open.alpha, open.content_alpha), (300.0, 1.0, 1.0));
        let leaving = legacy_look(full, Duration::from_secs(3), Some(DURATION / 2));
        assert!(leaving.box_w < 300.0 && leaving.alpha < 1.0 && leaving.content_alpha == leaving.alpha);
    }
}
