//! The island's mark, drawn apart from the content: it rides the appearing
//! circle (moved and scaled to its pose) and, for a clip save, is the
//! shutter-ring badge (arc, dot, tick) instead of a plain dot.

use std::f32::consts::{PI, TAU};

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN, D2D_SIZE_F};
use windows::Win32::Graphics::Direct2D::{
    ID2D1StrokeStyle, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_ELLIPSE, D2D1_SWEEP_DIRECTION_CLOCKWISE,
};
use windows_numerics::{Matrix3x2, Vector2};

use super::Renderer;
use crate::badge::{Badge, Glyph, TICK};
use crate::layout::Icon;
use crate::presence::{icon_pose, Look};

type Point = (f32, f32);

/// The tick's corners as fractions of its size (left, bottom, right).
const TICK_PTS: [(f32, f32); 3] = [(-0.45, 0.0), (-0.1, 0.35), (0.5, -0.35)];

/// Point at `angle` (radians clockwise from 12 o'clock) on a circle.
fn on_circle(cx: f32, cy: f32, r: f32, angle: f32) -> Vector2 {
    Vector2 { X: cx + r * angle.sin(), Y: cy - r * angle.cos() }
}

/// The first `f` (0..1) of the "!": its bar draws down over the first 75%,
/// then its dot appears. Same size box and stroke as the tick.
pub fn bang_points(cx: f32, cy: f32, size: f32, f: f32) -> (Vec<Point>, Option<Point>) {
    let (top, bottom, dot) = (cy - 0.42 * size, cy + 0.12 * size, (cx, cy + 0.38 * size));
    let bar = (f / 0.75).clamp(0.0, 1.0);
    (vec![(cx, top), (cx, top + (bottom - top) * bar)], (f >= 0.85).then_some(dot))
}

/// The first `f` (0..1) of the tick's two strokes, as points.
pub fn tick_points(cx: f32, cy: f32, size: f32, f: f32) -> Vec<(f32, f32)> {
    let p: Vec<(f32, f32)> = TICK_PTS.iter().map(|&(x, y)| (cx + x * size, cy + y * size)).collect();
    let len = |a: (f32, f32), b: (f32, f32)| ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
    let (first, second) = (len(p[0], p[1]), len(p[1], p[2]));
    let along = (first + second) * f.clamp(0.0, 1.0);
    let lerp = |a: (f32, f32), b: (f32, f32), t: f32| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
    if along <= first {
        vec![p[0], lerp(p[0], p[1], along / first)]
    } else {
        vec![p[0], p[1], lerp(p[1], p[2], (along - first) / second)]
    }
}

impl Renderer {
    /// The mark at its pose for `look` (`content_dx`: where the content sits in the open pill).
    pub(crate) unsafe fn icon(&self, icon: &Icon, look: &Look, content_dx: f32, badge: Option<Badge>) -> Result<()> {
        let (x, y, k) = icon_pose(look, content_dx, icon.cx);
        let place = Matrix3x2::translation(-icon.cx, -icon.cy) * Matrix3x2::scale(k, k) * Matrix3x2::translation(x, y);
        unsafe {
            self.target.SetTransform(&place);
            match icon.badge {
                Some(phase) => self.badge(icon, &badge.unwrap_or(Badge::rest(phase)), look.icon_alpha)?,
                None => self.shape(&icon.shape, look.icon_alpha)?,
            }
        }
        Ok(())
    }

    unsafe fn badge(&self, icon: &Icon, b: &Badge, alpha: f32) -> Result<()> {
        let (t, (cx, cy, r)) = (&self.target, (icon.cx, icon.cy, icon.r));
        let (ring_r, stroke) = (2.0 * r, 0.5 * r);
        let disc = |radius: f32| D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: radius, radiusY: radius };
        unsafe {
            self.paint(b.ring, alpha);
            if b.sweep >= TAU - 1e-3 {
                t.DrawEllipse(&disc(ring_r), &self.brush, stroke, None::<&ID2D1StrokeStyle>);
            } else if b.sweep > 1e-3 {
                let path = self.factory.CreatePathGeometry()?;
                let sink = path.Open()?;
                sink.BeginFigure(on_circle(cx, cy, ring_r, b.start), D2D1_FIGURE_BEGIN_HOLLOW);
                sink.AddArc(&D2D1_ARC_SEGMENT {
                    point: on_circle(cx, cy, ring_r, b.start + b.sweep),
                    size: D2D_SIZE_F { width: ring_r, height: ring_r },
                    rotationAngle: 0.0,
                    sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                    arcSize: if b.sweep > PI { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                });
                sink.EndFigure(D2D1_FIGURE_END_OPEN);
                sink.Close()?;
                t.DrawGeometry(&path, &self.brush, stroke, &self.round);
            }
            self.paint(b.dot_color, alpha);
            t.FillEllipse(&disc(r * b.dot), &self.brush);
            if b.drawn > 0.0 {
                let (points, dot) = match b.glyph {
                    Glyph::Tick => (tick_points(cx, cy, ring_r, b.drawn), None),
                    Glyph::Bang => bang_points(cx, cy, ring_r, b.drawn),
                };
                self.paint(TICK, alpha);
                self.polyline(&points, stroke)?;
                if let Some((x, y)) = dot {
                    t.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: x, Y: y }, radiusX: stroke / 2.0, radiusY: stroke / 2.0 }, &self.brush);
                }
            }
        }
        Ok(())
    }

    /// A round-capped open polyline in the current brush.
    unsafe fn polyline(&self, points: &[(f32, f32)], stroke: f32) -> Result<()> {
        unsafe {
            let path = self.factory.CreatePathGeometry()?;
            let sink = path.Open()?;
            sink.BeginFigure(Vector2 { X: points[0].0, Y: points[0].1 }, D2D1_FIGURE_BEGIN_HOLLOW);
            for &(x, y) in &points[1..] {
                sink.AddLine(Vector2 { X: x, Y: y });
            }
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.Close()?;
            self.target.DrawGeometry(&path, &self.brush, stroke, &self.round);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bang_draws_its_bar_then_its_dot() {
        let (bar, dot) = bang_points(0.0, 0.0, 10.0, 0.375);
        assert_eq!((bar[0], dot), ((0.0, -4.2), None));
        assert!((bar[1].1 - (-4.2 + 5.4 * 0.5)).abs() < 1e-4, "half the bar at half its share");
        let (bar, dot) = bang_points(0.0, 0.0, 10.0, 1.0);
        assert!((bar[1].1 - 1.2).abs() < 1e-4 && dot == Some((0.0, 3.8)));
    }

    #[test]
    fn the_tick_draws_its_short_stroke_then_its_long_one() {
        assert_eq!(tick_points(0.0, 0.0, 10.0, 0.0), vec![(-4.5, 0.0), (-4.5, 0.0)]);
        let full = tick_points(0.0, 0.0, 10.0, 1.0);
        assert_eq!(full.len(), 3);
        assert!((full[2].0 - 5.0).abs() < 1e-4 && (full[2].1 + 3.5).abs() < 1e-4);
        let early = tick_points(0.0, 0.0, 10.0, 0.2);
        assert_eq!(early.len(), 2, "the short stroke first");
        let late = tick_points(0.0, 0.0, 10.0, 0.8);
        assert!(late.len() == 3 && late[2].0 > -1.0 && late[2].0 < 5.0);
    }
}
