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
use crate::presence::Look;
use crate::layout::{sweep, Rgba, Scene, Shape, Sweep, EDGE_LINE, PILL, TRACK};

#[path = "render_icon.rs"]
mod icon;

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
            Ok(Self { factory, target, brush, round, dc, dib: None, capacity: (0, 0), sheen: None })
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

    /// Draws `look`: the pill (or the circle it grows from) with `scene`
    /// centred in the open pill, the action notice's sweep (`sheen`) over the
    /// pill under the content, and the mark (or the save `badge`) on top.
    /// Returns the window size.
    pub fn draw(
        &mut self,
        fonts: &Fonts,
        scene: &Scene,
        look: Look,
        scale: f32,
        sheen: Option<Sheen>,
        badge: Option<Badge>,
    ) -> Result<(i32, i32)> {
        let (w, h) = look.window();
        self.ensure(w, h)?;
        let sheen = match sheen {
            Some(sheen) => Some((self.sheen_brush(sheen.color)?, sheen.progress)),
            None => None,
        };
        let t = &self.target;
        unsafe {
            t.BindDC(self.dc, &RECT { left: 0, top: 0, right: w, bottom: h })?;
            t.BeginDraw();
            t.SetTransform(&Matrix3x2::identity());
            t.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            let pill = self.pill(&look, sheen);
            t.PushAxisAlignedClip(&pill.rect, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            if look.content_alpha > 0.01 {
                let [x, y, _, ph] = look.pill;
                let (dx, dy) = (look.margin + x + (look.full_w - scene.w) / 2.0, look.margin + y + (ph - scene.h) / 2.0);
                t.SetTransform(&Matrix3x2::translation(dx, dy));
                for shape in &scene.shapes {
                    self.shape(shape, look.content_alpha)?;
                }
                for text in &scene.texts {
                    let layout = fonts.layout(text.role, scale, &text.text)?;
                    self.paint(text.color, look.content_alpha);
                    t.DrawTextLayout(Vector2 { X: text.x, Y: text.y }, &layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
                }
            }
            if let Some(icon) = scene.icon.filter(|_| look.icon_alpha > 0.01) {
                self.icon(&icon, &look, scene.w, badge)?;
            }
            t.SetTransform(&Matrix3x2::identity());
            t.PopAxisAlignedClip();
            t.EndDraw(None, None)?;
        }
        Ok((w, h))
    }

    /// The pill body (fill, the sweep's band, hairline edge) at `look.pill`;
    /// returns its shape (window coordinates).
    unsafe fn pill(&self, look: &Look, sheen: Option<(ID2D1LinearGradientBrush, f32)>) -> D2D1_ROUNDED_RECT {
        let t = &self.target;
        let [x, y, w, h] = look.pill;
        let (left, top) = (look.margin + x, look.margin + y);
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

fn color(rgba: Rgba, alpha: f32) -> D2D1_COLOR_F {
    let channel = |shift: u32| ((rgba.rgb >> shift) & 0xff) as f32 / 255.0;
    D2D1_COLOR_F { r: channel(16), g: channel(8), b: channel(0), a: rgba.a * alpha }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::Frame;

    fn alpha_at(renderer: &Renderer, w: i32, h: i32, x: i32, y: i32) -> u8 {
        renderer.pixels(w, h)[(y * w + x) as usize][3]
    }

    #[test]
    fn pill_is_antialiased_and_survives_the_buffer_growing() {
        let (fonts, mut renderer) = (Fonts::new().unwrap(), Renderer::new().unwrap());
        let scene = Scene { w: 60.0, h: 44.0, ..Scene::default() };
        let (w, h) = renderer.draw(&fonts, &scene, Look::open(Frame { w: 60.0, h: 44.0, alpha: 1.0 }), 1.0, None, None).unwrap();
        assert_eq!(alpha_at(&renderer, w, h, 30, 22), (PILL.a * 255.0).round() as u8, "pill body is 94% opaque");
        assert_eq!(alpha_at(&renderer, w, h, 0, 0), 0, "rounded corner is transparent");
        let edge = alpha_at(&renderer, w, h, 4, 8);
        assert!(edge > 0 && edge < 240, "the curve is anti-aliased: {edge}");
        // A wider frame re-creates the DIB; drawing must land in the new one.
        let (w, h) = renderer.draw(&fonts, &scene, Look::open(Frame { w: 300.0, h: 44.0, alpha: 1.0 }), 1.0, None, None).unwrap();
        assert_eq!(w, 300);
        assert_eq!(alpha_at(&renderer, w, h, 280, 22), (PILL.a * 255.0).round() as u8);
    }

    /// Software-render cost of one island frame (an instrument, not a gate):
    /// `cargo test --release -p recall-overlay -- --ignored draw_cost --nocapture`.
    #[test]
    #[ignore]
    fn draw_cost_per_frame() {
        use crate::badge::SaveMark;
        use crate::layout::{notice_scene, stats_scene, StatsContent, ACTIVE};
        use crate::notice::{NoticeView, SavePhase, Tone};
        let (fonts, mut renderer) = (Fonts::new().unwrap(), Renderer::new().unwrap());
        let stats = StatsContent { recording: true, replay_secs: 120, fps: Some(144), low1: Some(98), gpu: Some(71), gpu_temp: Some(64), cpu: Some(23), ..Default::default() };
        let stats = stats_scene(&stats, 1.0, &fonts);
        let grow = 58.0 / 44.0;
        let notice = notice_scene(&NoticeView::new(Tone::Success, "Clip saved", "VALORANT · 0:30", true), grow, &fonts);
        let saving = NoticeView::new(Tone::Recording, "Saving", "0:30…", true).saving(SavePhase::Saving);
        let saving = notice_scene(&saving, grow, &fonts);
        let sweep = Some(Sheen { progress: 0.5, color: ACTIVE });
        let now = std::time::Instant::now();
        let ring = Some(SaveMark::new(SavePhase::Saving, now).badge(now + std::time::Duration::from_millis(450)));
        let grown = Frame { w: stats.w + 40.0, h: 58.0, alpha: 1.0 };
        let cases = [
            ("stats pill", &stats, Frame { w: stats.w, h: stats.h, alpha: 1.0 }, 1.0, None, None),
            ("grown notice", &notice, grown, grow, None, None),
            ("grown notice + sweep", &notice, grown, grow, sweep, None),
            ("saving pill + spinning ring", &saving, grown, grow, None, ring),
        ];
        for (label, scene, frame, scale, sheen, badge) in cases {
            renderer.draw(&fonts, scene, Look::open(frame), scale, sheen, badge).unwrap();
            let start = std::time::Instant::now();
            for _ in 0..500 {
                renderer.draw(&fonts, scene, Look::open(frame), scale, sheen, badge).unwrap();
            }
            println!("draw_cost {label}: {:.0} us/frame", start.elapsed().as_secs_f64() * 1e6 / 500.0);
        }
    }

    #[test]
    fn the_sweep_tints_the_pill_in_the_tone_colour_where_the_band_is_and_nowhere_else() {
        use crate::layout::{ACTIVE, ERROR};
        let (fonts, mut renderer) = (Fonts::new().unwrap(), Renderer::new().unwrap());
        let scene = Scene { w: 340.0, h: 58.0, ..Scene::default() };
        let frame = Frame { w: 340.0, h: 58.0, alpha: 1.0 };
        let pixel = |r: &Renderer, x: i32, y: i32| r.pixels(340, 58)[(y * 340 + x) as usize];
        renderer.draw(&fonts, &scene, Look::open(frame), 1.0, None, None).unwrap();
        let plain = pixel(&renderer, 170, 29);
        let mid = Some(Sheen { progress: 0.5, color: ACTIVE });
        renderer.draw(&fonts, &scene, Look::open(frame), 1.0, mid, None).unwrap();
        let [b, g, r, _] = pixel(&renderer, 170, 29);
        assert!(g > plain[1] + 50 && g > r && g > b, "green band at the centre: {:?} vs {plain:?}", [b, g, r]);
        assert_eq!(pixel(&renderer, 12, 29), plain, "the pill's far end is untouched mid-sweep");
        assert_eq!(pixel(&renderer, 0, 0)[3], 0, "never outside the pill");
        let failure = Some(Sheen { progress: 0.5, color: ERROR });
        renderer.draw(&fonts, &scene, Look::open(frame), 1.0, failure, None).unwrap();
        let [b, g, r, _] = pixel(&renderer, 170, 29);
        assert!(r > g && r > b, "a failure sweeps in the error colour: {:?}", [b, g, r]);
        for progress in [0.0, 1.0] {
            renderer.draw(&fonts, &scene, Look::open(frame), 1.0, Some(Sheen { progress, color: ACTIVE }), None).unwrap();
            assert_eq!(pixel(&renderer, 170, 29), plain, "off the pill at {progress}");
            assert_eq!(pixel(&renderer, 6, 29), plain, "left end clear at {progress}");
            assert_eq!(pixel(&renderer, 333, 29), plain, "right end clear at {progress}");
        }
    }
}
