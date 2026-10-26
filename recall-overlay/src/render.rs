//! Direct2D drawing of the island: a software DC render target into a 32-bit
//! premultiplied top-down DIB, pushed with `UpdateLayeredWindow(ULW_ALPHA)`.
//! Software rendering keeps the overlay off the GPU and its memory small.

use windows::core::Result;
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN, D2D1_PIXEL_FORMAT,
    D2D_RECT_F, D2D_SIZE_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush, ID2D1StrokeStyle,
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_CAP_STYLE_ROUND,
    D2D1_DASH_STYLE_SOLID, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_LINE_JOIN_ROUND, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE,
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

use crate::anim::Frame;
use crate::fonts::Fonts;
use crate::layout::{sweep, Rgba, Scene, Shape, Sweep, EDGE_LINE, PILL, TRACK};

pub struct Renderer {
    factory: ID2D1Factory,
    target: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    round: ID2D1StrokeStyle,
    dc: HDC,
    dib: Option<(HBITMAP, HGDIOBJ, *mut u8)>,
    capacity: (i32, i32),
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
            Ok(Self { factory, target, brush, round, dc, dib: None, capacity: (0, 0) })
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

    /// Draws the pill at `frame` size with `scene` centred in it.
    pub fn draw(&mut self, fonts: &Fonts, scene: &Scene, frame: Frame, scale: f32, content_alpha: f32) -> Result<(i32, i32)> {
        let (w, h) = (frame.w.ceil().max(1.0) as i32, frame.h.ceil().max(1.0) as i32);
        self.ensure(w, h)?;
        let t = &self.target;
        unsafe {
            t.BindDC(self.dc, &RECT { left: 0, top: 0, right: w, bottom: h })?;
            t.BeginDraw();
            t.SetTransform(&Matrix3x2::identity());
            t.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            let pill = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F { left: 0.0, top: 0.0, right: frame.w, bottom: frame.h },
                radiusX: frame.h / 2.0,
                radiusY: frame.h / 2.0,
            };
            self.paint(PILL, 1.0);
            t.FillRoundedRectangle(&pill, &self.brush);
            let edge = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F { left: 0.5, top: 0.5, right: frame.w - 0.5, bottom: frame.h - 0.5 },
                radiusX: frame.h / 2.0 - 0.5,
                radiusY: frame.h / 2.0 - 0.5,
            };
            self.paint(EDGE_LINE, 1.0);
            t.DrawRoundedRectangle(&edge, &self.brush, 1.0, None::<&ID2D1StrokeStyle>);
            if content_alpha > 0.01 {
                t.PushAxisAlignedClip(&pill.rect, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                let (dx, dy) = ((frame.w - scene.w) / 2.0, (frame.h - scene.h) / 2.0);
                t.SetTransform(&Matrix3x2::translation(dx, dy));
                for shape in &scene.shapes {
                    self.shape(shape, content_alpha)?;
                }
                for text in &scene.texts {
                    let layout = fonts.layout(text.role, scale, &text.text)?;
                    self.paint(text.color, content_alpha);
                    t.DrawTextLayout(Vector2 { X: text.x, Y: text.y }, &layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
                }
                t.SetTransform(&Matrix3x2::identity());
                t.PopAxisAlignedClip();
            }
            t.EndDraw(None, None)?;
        }
        Ok((w, h))
    }

    fn paint(&self, rgba: Rgba, alpha: f32) {
        unsafe { self.brush.SetColor(&color(rgba, alpha)) };
    }

    unsafe fn shape(&self, shape: &Shape, alpha: f32) -> Result<()> {
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

fn color(rgba: Rgba, alpha: f32) -> D2D1_COLOR_F {
    let channel = |shift: u32| ((rgba.rgb >> shift) & 0xff) as f32 / 255.0;
    D2D1_COLOR_F { r: channel(16), g: channel(8), b: channel(0), a: rgba.a * alpha }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha_at(renderer: &Renderer, w: i32, h: i32, x: i32, y: i32) -> u8 {
        renderer.pixels(w, h)[(y * w + x) as usize][3]
    }

    #[test]
    fn pill_is_antialiased_and_survives_the_buffer_growing() {
        let (fonts, mut renderer) = (Fonts::new().unwrap(), Renderer::new().unwrap());
        let scene = Scene { w: 60.0, h: 44.0, ..Scene::default() };
        let (w, h) = renderer.draw(&fonts, &scene, Frame { w: 60.0, h: 44.0, alpha: 1.0 }, 1.0, 1.0).unwrap();
        assert_eq!(alpha_at(&renderer, w, h, 30, 22), (PILL.a * 255.0).round() as u8, "pill body is 94% opaque");
        assert_eq!(alpha_at(&renderer, w, h, 0, 0), 0, "rounded corner is transparent");
        let edge = alpha_at(&renderer, w, h, 4, 8);
        assert!(edge > 0 && edge < 240, "the curve is anti-aliased: {edge}");
        // A wider frame re-creates the DIB; drawing must land in the new one.
        let (w, h) = renderer.draw(&fonts, &scene, Frame { w: 300.0, h: 44.0, alpha: 1.0 }, 1.0, 1.0).unwrap();
        assert_eq!(w, 300);
        assert_eq!(alpha_at(&renderer, w, h, 280, 22), (PILL.a * 255.0).round() as u8);
    }
}
