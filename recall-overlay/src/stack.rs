//! The side spots' tall, narrow island: the very same square cells as the
//! horizontal pill ([`crate::cells`]), in the same order, running top to
//! bottom with each cell's content upright. Nothing is re-flowed or
//! re-spaced: the side pill is the horizontal one turned a quarter, with
//! every cell turned back, so both have the same length and cell count.
//!
//! The result stays in the island's unturned coordinates (x along the
//! edge, y across the pill), so the pill's size, unroll and placement math
//! carry on unchanged: the renderer maps each cell's centre to the window
//! and draws its content there without a rotation. In a stacked scene a
//! [`Text`]'s `x`, `y` is its centre, not its box's top-left.

use crate::cells::length;
use crate::layout::{Measure, Role, Scene, Shape, Text};

/// `scene` (laid out at scale `s`) for a side spot.
pub fn stack(scene: &Scene, s: f32, m: &dyn Measure) -> Scene {
    stack_within(scene, s, m, f32::INFINITY)
}

/// [`stack`], at most `max` long (px): a column that would leave the side's
/// room (a long notice) ends in a "…" cell instead of running off the monitor.
pub fn stack_within(scene: &Scene, s: f32, m: &dyn Measure, max: f32) -> Scene {
    let mut out = scene.clone();
    out.stacked = true;
    let side = scene.h;
    for text in &mut out.texts {
        let metrics = m.measure(text.role, s, &text.text);
        let (x, y) = (text.x + metrics.width / 2.0, text.y + metrics.baseline - metrics.cap / 2.0);
        // Inside its cell the content turns back upright: a line above the
        // cell's centre (a "1%" over "low") stays above it, down the pill.
        let mid = if scene.pitch > 0.0 { (x / scene.pitch).floor() * scene.pitch + side / 2.0 } else { x };
        (text.x, text.y) = (mid + (y - side / 2.0), side / 2.0 - (x - mid));
    }
    if scene.pitch <= 0.0 || scene.w <= max {
        return out;
    }
    // The most cells that fit, the last of them the "…".
    let mut keep = scene.cells;
    while keep > 1 && length(keep, s) > max {
        keep -= 1;
    }
    let last = keep - 1;
    let cell = |x: f32| ((x / scene.pitch).floor().max(0.0) as usize, x - (x / scene.pitch).floor() * scene.pitch > side + 1e-3);
    let color = out.texts.iter().find(|t| cell(t.x).0 == last).map(|t| t.color);
    // A cell's content, and a separator in the gap before the "…", go.
    let gone = |x: f32| {
        let (i, in_gap) = cell(x);
        i >= last || (in_gap && i + 1 >= last)
    };
    out.texts.retain(|t| !gone(t.x));
    out.shapes.retain(|shape| !gone(crate::stack::shape_centre(shape).0));
    let cx = last as f32 * scene.pitch + side / 2.0;
    let color = color.or(out.texts.last().map(|t| t.color)).unwrap_or(crate::layout::TEXT);
    out.texts.push(Text { role: Role::NoticeUi, text: "…".into(), x: cx, y: side / 2.0, color });
    out.cells = keep;
    out.w = length(keep, s).max(side).ceil();
    out
}

pub fn shape_centre(shape: &Shape) -> (f32, f32) {
    match *shape {
        Shape::Dot { cx, cy, .. } | Shape::Hollow { cx, cy, .. } | Shape::Ring { cx, cy, .. } => (cx, cy),
        Shape::Pause { x, cy, bar_w, gap, .. } => (x + bar_w + gap / 2.0, cy),
    }
}
