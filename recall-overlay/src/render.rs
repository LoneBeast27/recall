//! Direct2D drawing of the island: a software DC render target into a 32-bit
//! premultiplied top-down DIB, pushed with `UpdateLayeredWindow(ULW_ALPHA)`.
//! Software rendering keeps the overlay off the GPU and its memory small.

use windows::core::Result;
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN, D2D1_GRADIENT_STOP,
    D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, ID2D1LinearGradientBrush, ID2D1SolidColorBrush,
    ID2D1StrokeStyle, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_CAP_STYLE_ROUND,
    D2D1_DASH_STYLE_SOLID, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_GAMMA_2_2, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
    D2D1_LINE_JOIN_ROUND, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE,
    D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES, D2D1_SWEEP_DIRECTION_CLOCKWISE,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA};
use windows_numerics::{Matrix3x2, Vector2};

use crate::badge::Badge;
use crate::cue::{band, Sheen, CUE_PEAK};
use crate::fonts::Fonts;
use crate::hint::CardScene;
use crate::presence::{content_dx, content_scale, Look, Turn};
use crate::layout::{sweep, Rgba, Role, Scene, Shape, Sweep, EDGE_LINE, PILL, TRACK};

#[path = "render_icon.rs"]
mod icon;
#[path = "render_card.rs"]
mod card;

/// The old content while an open pill crossfades it into the new.
#[derive(Clone, Copy)]
pub struct Outgoing<'a> {
    pub scene: &'a Scene,
    /// The scale `scene` was laid out at.
    pub scale: f32,
    pub alpha: f32,
    /// Draw its mark too (`false`: the new scene's mark carries on from it).
    pub icon: bool,
}

/// What draws over (and under) a frame's scene besides the pill itself.
#[derive(Clone, Copy, Default)]
pub struct Layers<'a> {
    /// An action notice's light sweep.
    pub sheen: Option<Sheen>,
    /// A clip save's shutter ring.
    pub badge: Option<Badge>,
    /// The shortcut hint's card under the pill.
    pub card: Option<&'a CardScene>,
    /// The content a transformation is leaving.
    pub outgoing: Option<Outgoing<'a>>,
}

pub struct Renderer {
    factory: ID2D1Factory,
    target: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    round: ID2D1StrokeStyle,
    dc: HDC,
    dib: Option<(HBITMAP, HGDIOBJ, *mut u8)>,
    capacity: (i32, i32),
    /// The sweep's gradient for the last tone colour it was drawn in.
    sheen: Option<(u32, ID2D1LinearGradientBrush)>,
    /// The look's quarter turn (a side spot), applied after every transform.
    base: std::cell::Cell<Matrix3x2>,
    /// Every upright transform of the last draws (tests check none rotates).
    #[cfg(test)]
    pub(crate) upright_log: std::cell::RefCell<Vec<Matrix3x2>>,
}

impl Renderer {
    pub fn new() -> Result<Self> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let target = factory.CreateDCRenderTarget(&props)?;
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            target.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            let brush = target.CreateSolidColorBrush(&color(PILL, 1.0), None)?;
            let round = factory.CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES {
                    startCap: D2D1_CAP_STYLE_ROUND,
                    endCap: D2D1_CAP_STYLE_ROUND,
                    dashCap: D2D1_CAP_STYLE_ROUND,
                    lineJoin: D2D1_LINE_JOIN_ROUND,
                    miterLimit: 1.0,
                    dashStyle: D2D1_DASH_STYLE_SOLID,
                    dashOffset: 0.0,
                },
                None,
            )?;
            let dc = CreateCompatibleDC(None);
            Ok(Self { factory, target, brush, round, dc, dib: None, capacity: (0, 0), sheen: None,
                base: std::cell::Cell::new(Matrix3x2::identity()),
                #[cfg(test)]
                upright_log: Default::default(),
            })
        }
    }

    /// Grows the DIB to hold at least `w`×`h` (never shrinks: sizes animate).
    fn ensure(&mut self, w: i32, h: i32) -> Result<()> {
        if w <= self.capacity.0 && h <= self.capacity.1 && self.dib.is_some() {
            return Ok(());
        }
        let (cw, ch) = (w.max(self.capacity.0).max(1), h.max(self.capacity.1).max(1));
        unsafe {
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: cw,
                    biHeight: -ch,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(Some(self.dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
            // Selecting the new bitmap deselects the old one, which can then go;
            // the DC's original bitmap is restored only on drop.
            let previous = SelectObject(self.dc, HGDIOBJ(bitmap.0));
            match self.dib.take() {
                Some((old_bitmap, original, _)) => {
                    let _ = DeleteObject(HGDIOBJ(old_bitmap.0));
                    self.dib = Some((bitmap, original, bits.cast()));
                }
                None => self.dib = Some((bitmap, previous, bits.cast())),
            }
        }
        self.capacity = (cw, ch);
        Ok(())
    }

    /// Draws `look`: the pill (or the circle it grows from) with `scene` in
    /// the open pill, the action notice's sweep over the pill under the
    /// content, the mark (or the save badge) on top, and the hint card
    /// below. Returns the window size.
    pub fn draw(&mut self, fonts: &Fonts, scene: &Scene, look: Look, scale: f32, layers: Layers) -> Result<(i32, i32)> {
        let Layers { sheen, badge, card, outgoing } = layers;
        let (uw, uh) = look.window();
        let (w, h) = look.shown_window();
        self.base.set(turn(look.turn, uw as f32, uh as f32));
        self.ensure(w, h)?;
        let sheen = match sheen {
            Some(sheen) => Some((self.sheen_brush(sheen.color)?, sheen.progress)),
            None => None,
        };
        let t = &self.target;
        unsafe {
            t.BindDC(self.dc, &RECT { left: 0, top: 0, right: w, bottom: h })?;
            t.BeginDraw();
            self.set_transform(Matrix3x2::identity());
            t.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            let pill = self.pill(&look, sheen);
            t.PushAxisAlignedClip(&pill.rect, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            if let Some(old) = outgoing {
                self.content(fonts, old.scene, &look, old.scale, (old.alpha, old.alpha, old.alpha))?;
                if let Some(icon) = old.scene.icon.filter(|_| old.icon) {
                    self.icon(&icon, &look, placed(&look, old.scene), None, old.alpha)?;
                }
            }
            if look.content_alpha > 0.01 {
                self.content(fonts, scene, &look, scale, (look.content_alpha, look.title_alpha, look.detail_alpha))?;
            }
            if let Some(icon) = scene.icon.filter(|_| look.icon_alpha > 0.01) {
                self.icon(&icon, &look, placed(&look, scene), badge, look.icon_alpha)?;
            }
            self.set_transform(Matrix3x2::identity());
            t.PopAxisAlignedClip();
            if let (Some(card), Some(spot)) = (card, look.card_origin()) {
                self.card(fonts, card, &look, spot, scale)?;
            }
            t.EndDraw(None, None)?;
        }
        Ok((w, h))
    }

    /// `scene`'s shapes and text (not its mark) in the pill, scaled with it;
    /// `alphas` = (content, title line, detail line).
    unsafe fn content(&self, fonts: &Fonts, scene: &Scene, look: &Look, scale: f32, alphas: (f32, f32, f32)) -> Result<()> {
        let t = &self.target;
        let (dx, k) = placed(look, scene);
        let ([x, y, _, ph], (ox, oy)) = (look.pill, look.pill_origin());
        let (left, top) = (ox + x + dx, oy + y + (ph - scene.h * k) / 2.0);
        if scene.stacked {
            return unsafe { self.stacked(fonts, scene, (left, top), k, scale, alphas) };
        }
        unsafe {
            self.set_transform(Matrix3x2::scale(k, k) * Matrix3x2::translation(left, top));
            for shape in &scene.shapes {
                self.shape(shape, alphas.0)?;
            }
            for text in &scene.texts {
                let layout = fonts.layout(text.role, scale, &text.text)?;
                let alpha = match text.role {
                    Role::NoticeTitle => alphas.1,
                    Role::NoticeGame | Role::NoticeUi => alphas.2,
                    _ => alphas.0,
                };
                self.paint(text.color, alpha);
                t.DrawTextLayout(Vector2 { X: text.x, Y: text.y }, &layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
        }
        Ok(())
    }

    /// A side spot's re-flowed `scene` at `(left, top)`: each glyph and
    /// shape upright at its mapped centre.
    unsafe fn stacked(&self, fonts: &Fonts, scene: &Scene, (left, top): (f32, f32), k: f32, scale: f32, alphas: (f32, f32, f32)) -> Result<()> {
        let at = |(x, y): (f32, f32)| (left + x * k, top + y * k);
        unsafe {
            for shape in &scene.shapes {
                let c = crate::stack::shape_centre(shape);
                self.upright(c, k, at(c));
                self.shape(shape, alphas.0)?;
            }
            for text in &scene.texts {
                let layout = fonts.layout(text.role, scale, &text.text)?;
                let metrics = crate::layout::Measure::measure(fonts, text.role, scale, &text.text);
                let alpha = match text.role {
                    Role::NoticeTitle => alphas.1,
                    Role::NoticeGame | Role::NoticeUi => alphas.2,
                    _ => alphas.0,
                };
                self.upright((text.x, text.y), k, at((text.x, text.y)));
                self.paint(text.color, alpha);
                let origin = Vector2 { X: text.x - metrics.width / 2.0, Y: text.y - (metrics.baseline - metrics.cap / 2.0) };
                self.target.DrawTextLayout(origin, &layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
        }
        Ok(())
    }

    /// The pill body (fill, the sweep's band, hairline edge) at `look.pill`;
    /// returns its shape (window coordinates).
    unsafe fn pill(&self, look: &Look, sheen: Option<(ID2D1LinearGradientBrush, f32)>) -> D2D1_ROUNDED_RECT {
        let t = &self.target;
        let ([x, y, w, h], (ox, oy)) = (look.pill, look.pill_origin());
        let (left, top) = (ox + x, oy + y);
        let pill = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F { left, top, right: left + w, bottom: top + h },
            radiusX: h / 2.0,
            radiusY: h / 2.0,
        };
        let edge = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F { left: left + 0.5, top: top + 0.5, right: left + w - 0.5, bottom: top + h - 0.5 },
            radiusX: (h / 2.0 - 0.5).max(0.0),
            radiusY: (h / 2.0 - 0.5).max(0.0),
        };
        unsafe {
            self.paint(PILL, 1.0);
            t.FillRoundedRectangle(&pill, &self.brush);
            if let Some((brush, progress)) = sheen {
                let ((x0, y0), (x1, y1)) = band(w, h, progress);
                brush.SetStartPoint(Vector2 { X: left + x0, Y: top + y0 });
                brush.SetEndPoint(Vector2 { X: left + x1, Y: top + y1 });
                t.FillRoundedRectangle(&pill, &brush);
            }
            self.paint(EDGE_LINE, 1.0);
            t.DrawRoundedRectangle(&edge, &self.brush, 1.0, None::<&ID2D1StrokeStyle>);
        }
        pill
    }

    /// A soft band of `rgba`: clear at both ends, `CUE_PEAK` in the middle.
    fn sheen_brush(&mut self, rgba: Rgba) -> Result<ID2D1LinearGradientBrush> {
        if let Some((_, brush)) = self.sheen.as_ref().filter(|(rgb, _)| *rgb == rgba.rgb) {
            return Ok(brush.clone());
        }
        let stop = |position: f32, alpha: f32| D2D1_GRADIENT_STOP { position, color: color(rgba, alpha) };
        let stops = [stop(0.0, 0.0), stop(0.3, CUE_PEAK * 0.45), stop(0.5, CUE_PEAK), stop(0.7, CUE_PEAK * 0.45), stop(1.0, 0.0)];
        let brush = unsafe {
            let collection = self.target.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
            let axis = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: Vector2 { X: 0.0, Y: 0.0 }, endPoint: Vector2 { X: 1.0, Y: 0.0 } };
            self.target.CreateLinearGradientBrush(&axis, None, &collection)?
        };
        self.sheen = Some((rgba.rgb, brush.clone()));
        Ok(brush)
    }

    /// `m`, then the look's side-spot mapping.
    pub(crate) fn set_transform(&self, m: Matrix3x2) {
        unsafe { self.target.SetTransform(&(m * self.base.get())) };
    }

    /// Draws upright: content point `c` lands where the unturned window
    /// point `at` is shown, scaled `k`, with no rotation or mirror.
    pub(crate) fn upright(&self, c: (f32, f32), k: f32, at: (f32, f32)) {
        let (x, y) = apply(&self.base.get(), at);
        let m = Matrix3x2::translation(-c.0, -c.1) * Matrix3x2::scale(k, k) * Matrix3x2::translation(x, y);
        #[cfg(test)]
        self.upright_log.borrow_mut().push(m);
        unsafe { self.target.SetTransform(&m) };
    }

    pub(crate) fn paint(&self, rgba: Rgba, alpha: f32) {
        unsafe { self.brush.SetColor(&color(rgba, alpha)) };
    }

    pub(crate) unsafe fn shape(&self, shape: &Shape, alpha: f32) -> Result<()> {
        let t = &self.target;
        let ellipse = |cx: f32, cy: f32, r: f32| D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r };
        unsafe {
            match *shape {
                Shape::Dot { cx, cy, r, color } => {
                    self.paint(color, alpha);
                    t.FillEllipse(&ellipse(cx, cy, r), &self.brush);
                }
                Shape::Hollow { cx, cy, r, stroke, color } => {
                    self.paint(color, alpha);
                    t.DrawEllipse(&ellipse(cx, cy, r - stroke / 2.0), &self.brush, stroke, None::<&ID2D1StrokeStyle>);
                }
                Shape::Pause { x, cy, bar_w, bar_h, gap, color } => {
                    self.paint(color, alpha);
                    for left in [x, x + bar_w + gap] {
                        let bar = D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F { left, top: cy - bar_h / 2.0, right: left + bar_w, bottom: cy + bar_h / 2.0 },
                            radiusX: bar_w / 2.0,
                            radiusY: bar_w / 2.0,
                        };
                        t.FillRoundedRectangle(&bar, &self.brush);
                    }
                }
                Shape::Ring { cx, cy, r, stroke, frac, color } => {
                    self.paint(TRACK, alpha);
                    t.DrawEllipse(&ellipse(cx, cy, r), &self.brush, stroke, None::<&ID2D1StrokeStyle>);
                    self.paint(color, alpha);
                    match sweep(cx, cy, r, frac) {
                        Sweep::Empty => {}
                        Sweep::Full => t.DrawEllipse(&ellipse(cx, cy, r), &self.brush, stroke, None::<&ID2D1StrokeStyle>),
                        Sweep::Arc { start, end, large } => {
                            let path = self.factory.CreatePathGeometry()?;
                            let sink = path.Open()?;
                            sink.BeginFigure(Vector2 { X: start.0, Y: start.1 }, D2D1_FIGURE_BEGIN_HOLLOW);
                            sink.AddArc(&D2D1_ARC_SEGMENT {
                                point: Vector2 { X: end.0, Y: end.1 },
                                size: D2D_SIZE_F { width: r, height: r },
                                rotationAngle: 0.0,
                                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                                arcSize: if large { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                            });
                            sink.EndFigure(D2D1_FIGURE_END_OPEN);
                            sink.Close()?;
                            t.DrawGeometry(&path, &self.brush, stroke, &self.round);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Shows the last drawn `w`×`h` at (`x`, `y`) with window opacity `alpha`.
    pub fn present(&self, hwnd: HWND, x: i32, y: i32, w: i32, h: i32, alpha: f32) -> Result<()> {
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: (alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                None,
                Some(&POINT { x, y }),
                Some(&SIZE { cx: w, cy: h }),
                Some(self.dc),
                Some(&POINT { x: 0, y: 0 }),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        }
    }

    /// The premultiplied BGRA rows of the last `w`×`h` draw (stride = capacity width).
    pub fn pixels(&self, w: i32, h: i32) -> Vec<[u8; 4]> {
        let Some((_, _, bits)) = self.dib else { return Vec::new() };
        let stride = self.capacity.0 as usize;
        let all = unsafe { std::slice::from_raw_parts(bits.cast::<[u8; 4]>(), stride * self.capacity.1 as usize) };
        (0..h as usize).flat_map(|row| all[row * stride..row * stride + w as usize].iter().copied()).collect()
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            if let Some((bitmap, original, _)) = self.dib.take() {
                SelectObject(self.dc, original);
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
            }
            let _ = DeleteDC(self.dc);
        }
    }
}

/// How a `w`x`h` unturned window maps into the shown one for a side spot
/// (row vectors: a point is `p * M`). The pill's length runs down the
/// screen on both edges, its start at the top; across the pill, `Right`
/// reaches leftwards (inwards from the right edge), `Left` rightwards.
/// Only geometry (the pill, clips, the sweep) goes through this; glyphs
/// and marks are drawn upright at their mapped centres.
pub(crate) fn turn(turn: Turn, _w: f32, h: f32) -> Matrix3x2 {
    match turn {
        Turn::None => Matrix3x2::identity(),
        Turn::Right => Matrix3x2 { M11: 0.0, M12: 1.0, M21: -1.0, M22: 0.0, M31: h, M32: 0.0 },
        Turn::Left => Matrix3x2 { M11: 0.0, M12: 1.0, M21: 1.0, M22: 0.0, M31: 0.0, M32: 0.0 },
    }
}

/// `p * m` (row vector).
pub(crate) fn apply(m: &Matrix3x2, (x, y): (f32, f32)) -> (f32, f32) {
    (x * m.M11 + y * m.M21 + m.M31, x * m.M12 + y * m.M22 + m.M32)
}

/// Where `scene` sits in the pill now, and the scale it draws at.
fn placed(look: &Look, scene: &Scene) -> (f32, f32) {
    let k = content_scale(look, scene.h);
    (content_dx(look, scene.w * k, scene.lead), k)
}

fn color(rgba: Rgba, alpha: f32) -> D2D1_COLOR_F {
    let channel = |shift: u32| ((rgba.rgb >> shift) & 0xff) as f32 / 255.0;
    D2D1_COLOR_F { r: channel(16), g: channel(8), b: channel(0), a: rgba.a * alpha }
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
