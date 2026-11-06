//! The island's square cell grid, one layout for both orientations.
//!
//! Every field (the mark, a word, a timer part, an fps number, a ring, a
//! label) fills a whole number of square cells, S × S where S is the pill's
//! thickness, with one gap between cells. A horizontal pill runs the cells
//! left to right; a side spot's pill runs the very same cells top to bottom
//! with each cell's content upright ([`crate::stack`]), so the two read as
//! rotations of each other cell by cell and have the same length:
//! `cells × S + (cells − 1) × gap`.
//!
//! A cell's content is centred in it. Cells of a kind share one size: every
//! number cell (a timer part, fps, 1% low, temperature) is the same face and
//! size, so 3 tabular digits fit with [`MARGIN`] either side.

use crate::cell_text::{pieces, tokens, Token};
use crate::layout::{tone_color, Icon, Measure, Rgba, Role, Scene, Shape, Text, TERTIARY};
use crate::notice::{SavePhase, Tone};
use crate::placement::ISLAND_H;

/// Room between two cells (1440p px).
pub const GAP: f32 = 4.0;
/// The least room between a cell's content and its edge (1440p px).
pub const MARGIN: f32 = 6.0;
/// A ring's diameter and stroke (1440p px).
const RING_D: f32 = 30.0;
const RING_STROKE: f32 = 2.75;
/// The separator dots in the gap: radius, and a colon's half spread (1440p px).
const JOINT_R: f32 = 1.2;
const COLON_SPREAD: f32 = 3.5;
/// The separator dots are dim.
const JOINT: Rgba = Rgba { rgb: TERTIARY.rgb, a: 0.6 };
/// Room between the two lines of a [`Fill::Pair`] (1440p px).
const PAIR_GAP: f32 = 3.5;

/// What one cell shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Fill {
    /// The tone mark (dot, pause bars, hollow ring, a save's shutter ring).
    Mark(Tone, Option<SavePhase>),
    Text(Role, String, Rgba),
    /// Two small lines centred together ("1%" over "low").
    Pair(Role, [&'static str; 2], Rgba),
    /// A usage ring with its number inside (a dash before the first reading).
    Ring(Option<u32>, Rgba),
    /// Held room (a save's longer title to come).
    Blank,
}

/// What is drawn in the gap after a cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Joint {
    #[default]
    None,
    /// Two dim dots across the pill: a colon on a horizontal pill, two dots
    /// side by side on a side spot's (a timer's minutes | seconds).
    Colon,
    /// One dim dot ("VALORANT · 0:30").
    Dot,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub fill: Fill,
    pub joint: Joint,
}

/// Builds the cells of a pill, splitting text runs to fit a cell.
pub struct Cells<'a> {
    s: f32,
    m: &'a dyn Measure,
    pub out: Vec<Cell>,
}

impl<'a> Cells<'a> {
    pub fn new(s: f32, m: &'a dyn Measure) -> Self {
        Self { s, m, out: Vec::new() }
    }

    pub fn push(&mut self, fill: Fill) {
        self.out.push(Cell { fill, joint: Joint::None });
    }

    fn join(&mut self, joint: Joint) {
        if let Some(last) = self.out.last_mut() {
            last.joint = joint;
        }
    }

    /// Whether `text` fits one cell with [`MARGIN`] either side.
    pub fn fits(&self, role: Role, text: &str) -> bool {
        self.m.measure(role, self.s, text).width <= (ISLAND_H - 2.0 * MARGIN) * self.s + 0.01
    }

    /// `run` as cells: words in `word`, digits in `number`, all in `color`.
    pub fn run(&mut self, run: &str, word: Role, number: Role, color: Rgba) {
        for token in tokens(run) {
            match token {
                Token::Timer(parts) => {
                    for (i, part) in parts.into_iter().enumerate() {
                        if i > 0 {
                            self.join(Joint::Colon);
                        }
                        self.push(Fill::Text(number, part, color));
                    }
                }
                Token::Number(cells) => cells.into_iter().for_each(|c| self.push(Fill::Text(number, c, color))),
                Token::Word(w) => {
                    let role = if w.starts_with(|c: char| c.is_ascii_digit()) { number } else { word };
                    for piece in pieces(&w, |p| self.fits(role, p)) {
                        self.push(Fill::Text(role, piece, color));
                    }
                }
                Token::Dot => self.join(Joint::Dot),
                Token::Ellipsis => self.push(Fill::Text(word, "…".into(), color)),
            }
        }
    }
}

/// Cells of `run` (as [`Cells::run`] lays them), counted.
pub fn count(run: &str, word: Role, number: Role, s: f32, m: &dyn Measure) -> usize {
    let mut cells = Cells::new(s, m);
    cells.run(run, word, number, Rgba { rgb: 0, a: 1.0 });
    cells.out.len()
}

/// The cell pitch (a cell and the gap after it) at scale `s`.
pub fn pitch(s: f32) -> f32 {
    (ISLAND_H + GAP) * s
}

/// The length of `n` cells at scale `s`.
pub fn length(n: usize, s: f32) -> f32 {
    if n == 0 { 0.0 } else { n as f32 * ISLAND_H * s + (n - 1) as f32 * GAP * s }
}

/// `cells` laid out left to right at scale `s`: a horizontal scene (text
/// boxes by their top-left). [`crate::stack`] turns it into the side spot's.
pub fn grid(cells: &[Cell], s: f32, m: &dyn Measure) -> Scene {
    let side = ISLAND_H * s;
    let cy = side / 2.0;
    let mut scene = Scene { h: side, w: length(cells.len(), s).max(side).ceil(), cells: cells.len(), pitch: pitch(s), ..Scene::default() };
    let centred = |role: Role, text: &str, cx: f32, cy: f32, color: Rgba| {
        let metrics = m.measure(role, s, text);
        Text { role, text: text.into(), x: cx - metrics.width / 2.0, y: cy - (metrics.baseline - metrics.cap / 2.0), color }
    };
    for (i, cell) in cells.iter().enumerate() {
        let cx = i as f32 * pitch(s) + side / 2.0;
        match &cell.fill {
            Fill::Mark(tone, badge) => scene.icon = Some(mark(*tone, *badge, cx, cy, s)),
            Fill::Text(role, text, color) => scene.texts.push(centred(*role, text, cx, cy, *color)),
            Fill::Pair(role, [top, bottom], color) => {
                let (c1, c2) = (m.measure(*role, s, top).cap, m.measure(*role, s, bottom).cap);
                let start = cy - (c1 + PAIR_GAP * s + c2) / 2.0;
                scene.texts.push(centred(*role, top, cx, start + c1 / 2.0, *color));
                scene.texts.push(centred(*role, bottom, cx, start + c1 + PAIR_GAP * s + c2 / 2.0, *color));
            }
            Fill::Ring(value, color) => {
                let (d, stroke) = (RING_D * s, RING_STROKE * s);
                let value = value.map(|v| v.min(100));
                let frac = value.unwrap_or(0) as f32 / 100.0;
                scene.shapes.push(Shape::Ring { cx, cy, r: (d - stroke) / 2.0, stroke, frac, color: *color });
                let number = value.map_or("—".to_string(), |v| v.to_string());
                scene.texts.push(centred(Role::RingNumber, &number, cx, cy, crate::layout::TEXT));
            }
            Fill::Blank => {}
        }
        if i + 1 < cells.len() {
            let x = cx + side / 2.0 + GAP * s / 2.0;
            let dot = |cy: f32| Shape::Dot { cx: x, cy, r: JOINT_R * s, color: JOINT };
            match cell.joint {
                Joint::Colon => scene.shapes.extend([dot(cy - COLON_SPREAD * s), dot(cy + COLON_SPREAD * s)]),
                Joint::Dot => scene.shapes.push(dot(cy)),
                Joint::None => {}
            }
        }
    }
    scene
}

/// The tone mark centred on (`cx`, `cy`).
fn mark(tone: Tone, badge: Option<SavePhase>, cx: f32, cy: f32, s: f32) -> Icon {
    let (r, color) = (4.0 * s, tone_color(tone));
    let shape = match tone {
        Tone::Waiting => Shape::Hollow { cx, cy, r, stroke: 1.5 * s, color },
        Tone::Paused => {
            let (bar_w, gap) = (2.5 * s, 2.0 * s);
            Shape::Pause { x: cx - bar_w - gap / 2.0, cy, bar_w, bar_h: 9.0 * s, gap, color }
        }
        _ => Shape::Dot { cx, cy, r, color },
    };
    Icon { shape, cx, cy, r, badge }
}

#[cfg(test)]
#[path = "cells_tests.rs"]
mod tests;
