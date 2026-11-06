//! The side spots' tall, narrow island: a laid-out (horizontal) scene
//! re-flowed so it reads top to bottom with nothing rotated. The mark sits
//! at the top; every text run becomes upright glyphs, one per line, each
//! centred across the pill ("REC" → R, E, C); a space is a small gap; a
//! usage ring stays a ring with its number upright inside.
//!
//! The result stays in the island's unturned coordinates (x along the
//! edge, y across the pill), so the pill's size, unroll and placement math
//! carry on unchanged: the renderer maps each glyph's centre to the window
//! and draws the glyph there without a rotation. In a stacked scene a
//! [`Text`]'s `x`, `y` is the glyph's centre, not its box's top-left.

use crate::layout::{Icon, Measure, Role, Scene, Shape, Text};

/// Room before the mark and after the last glyph, the gap between glyph
/// lines, between runs, and for a space (1440p px).
const PAD: f32 = 14.0;
const LINE_GAP: f32 = 3.5;
const RUN_GAP: f32 = 9.0;
const SPACE: f32 = 5.0;

/// `scene` (laid out at scale `s`) re-flowed for a side spot.
pub fn stack(scene: &Scene, s: f32, m: &dyn Measure) -> Scene {
    let cy = scene.h / 2.0;
    let mut out = Scene { h: scene.h, lead: scene.lead, stacked: true, ..Scene::default() };
    let mut cursor = PAD * s;
    if let Some(icon) = scene.icon {
        let along = icon_along(&icon);
        let (cx, dx, dy) = (cursor + along / 2.0, cursor + along / 2.0 - icon.cx, cy - icon.cy);
        out.icon = Some(Icon { shape: moved(icon.shape, dx, dy), cx, cy, ..icon });
        cursor += along + RUN_GAP * s;
    }
    for item in items(scene) {
        match item {
            Item::Shape(shape) => {
                let (centre, along) = (shape_centre(&shape), shape_along(&shape));
                let to = (cursor + along / 2.0, cy);
                out.shapes.push(moved(shape, to.0 - centre.0, to.1 - centre.1));
                // A ring's number stays inside it, whole and upright.
                for number in scene.texts.iter().filter(|t| t.role == Role::RingNumber && inside(t, &shape, m, s)) {
                    out.texts.push(Text { x: to.0, y: to.1, ..number.clone() });
                }
                cursor += along + RUN_GAP * s;
            }
            Item::Text(text) => {
                for c in text.text.chars() {
                    if c.is_whitespace() {
                        cursor += SPACE * s;
                        continue;
                    }
                    let glyph = c.to_string();
                    let cap = m.measure(text.role, s, &glyph).cap.max(m.measure(text.role, s, "0").cap * 0.6);
                    out.texts.push(Text { role: text.role, text: glyph, x: cursor + cap / 2.0, y: cy, color: text.color });
                    cursor += cap + LINE_GAP * s;
                }
                cursor += (RUN_GAP - LINE_GAP) * s;
            }
        }
    }
    out.w = (cursor - RUN_GAP * s + PAD * s).max(scene.h * 2.0).ceil();
    out
}

enum Item<'a> {
    Text(&'a Text),
    Shape(Shape),
}

/// The scene's runs and shapes in reading order (left to right, then top
/// to bottom); ring numbers ride their rings.
fn items(scene: &Scene) -> Vec<Item<'_>> {
    let mut all: Vec<(f32, f32, Item)> = scene
        .texts
        .iter()
        .filter(|t| t.role != Role::RingNumber)
        .map(|t| (t.x, t.y, Item::Text(t)))
        .chain(scene.shapes.iter().map(|&shape| {
            let ((cx, cy), along) = (shape_centre(&shape), shape_along(&shape));
            (cx - along / 2.0, cy, Item::Shape(shape))
        }))
        .collect();
    all.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    all.into_iter().map(|(_, _, item)| item).collect()
}

fn inside(text: &Text, shape: &Shape, m: &dyn Measure, s: f32) -> bool {
    let Shape::Ring { cx, cy, r, .. } = *shape else { return false };
    let metrics = m.measure(text.role, s, &text.text);
    let (tx, ty) = (text.x + metrics.width / 2.0, text.y + metrics.baseline - metrics.cap / 2.0);
    (tx - cx).abs() <= r && (ty - cy).abs() <= r
}

/// How much of the pill's length the mark takes.
fn icon_along(icon: &Icon) -> f32 {
    if icon.badge.is_some() { 4.5 * icon.r } else { shape_along(&icon.shape).max(2.0 * icon.r) }
}

pub fn shape_centre(shape: &Shape) -> (f32, f32) {
    match *shape {
        Shape::Dot { cx, cy, .. } | Shape::Hollow { cx, cy, .. } | Shape::Ring { cx, cy, .. } => (cx, cy),
        Shape::Pause { x, cy, bar_w, gap, .. } => (x + bar_w + gap / 2.0, cy),
    }
}

/// The shape's length down the pill (drawn upright).
fn shape_along(shape: &Shape) -> f32 {
    match *shape {
        Shape::Dot { r, .. } | Shape::Hollow { r, .. } => 2.0 * r,
        Shape::Ring { r, stroke, .. } => 2.0 * r + stroke,
        Shape::Pause { bar_h, .. } => bar_h,
    }
}

fn moved(shape: Shape, dx: f32, dy: f32) -> Shape {
    match shape {
        Shape::Dot { cx, cy, r, color } => Shape::Dot { cx: cx + dx, cy: cy + dy, r, color },
        Shape::Hollow { cx, cy, r, stroke, color } => Shape::Hollow { cx: cx + dx, cy: cy + dy, r, stroke, color },
        Shape::Pause { x, cy, bar_w, bar_h, gap, color } => Shape::Pause { x: x + dx, cy: cy + dy, bar_w, bar_h, gap, color },
        Shape::Ring { cx, cy, r, stroke, frac, color } => Shape::Ring { cx: cx + dx, cy: cy + dy, r, stroke, frac, color },
    }
}

#[cfg(test)]
#[path = "stack_tests.rs"]
mod tests;
