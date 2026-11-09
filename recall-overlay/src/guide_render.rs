//! Software Direct2D drawing of the editor's guide grid (`guide::GuideScene`)
//! into a 32-bit premultiplied top-down DIB the size of the placement area,
//! the island's way (`render.rs`). A `clip` redraws only that box.

use windows::core::Result;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush, ID2D1StrokeStyle,
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_CAP_STYLE_FLAT, D2D1_DASH_STYLE_CUSTOM,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_LINE_JOIN_MITER,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
    D2D1_STROKE_STYLE_PROPERTIES, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows_numerics::{Matrix3x2, Vector2};

use crate::fonts::Fonts;
use crate::guide::{GuideScene, PLATE};
use crate::layout::Rgba;

pub struct GuideRenderer {
    _factory: ID2D1Factory,
    target: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    dash: ID2D1StrokeStyle,
    pub(crate) dc: HDC,
    dib: Option<(HBITMAP, HGDIOBJ, *mut u8)>,
    pub size: (i32, i32),
}

fn color(rgba: Rgba) -> D2D1_COLOR_F {
    let channel = |shift: u32| ((rgba.rgb >> shift) & 0xff) as f32 / 255.0;
    D2D1_COLOR_F { r: channel(16), g: channel(8), b: channel(0), a: rgba.a }
}

impl GuideRenderer {
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
            let brush = target.CreateSolidColorBrush(&color(PLATE), None)?;
            // Dashes in stroke widths: 4 on, 3 off (8 px / 6 px at 2 px).
            let dash = factory.CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES {
                    startCap: D2D1_CAP_STYLE_FLAT,
                    endCap: D2D1_CAP_STYLE_FLAT,
                    dashCap: D2D1_CAP_STYLE_FLAT,
                    lineJoin: D2D1_LINE_JOIN_MITER,
                    miterLimit: 10.0,
                    dashStyle: D2D1_DASH_STYLE_CUSTOM,
                    dashOffset: 0.0,
                },
                Some(&[4.0, 3.0]),
            )?;
            let dc = CreateCompatibleDC(None);
            Ok(Self { _factory: factory, target, brush, dash, dc, dib: None, size: (0, 0) })
        }
    }

    /// A DIB of exactly `w`×`h` (the area: drawn once per size).
    fn ensure(&mut self, w: i32, h: i32) -> Result<()> {
        if self.size == (w, h) && self.dib.is_some() {
            return Ok(());
        }
        unsafe {
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w.max(1),
                    biHeight: -h.max(1),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(Some(self.dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let previous = SelectObject(self.dc, HGDIOBJ(bitmap.0));
            match self.dib.take() {
                Some((old, original, _)) => {
                    let _ = DeleteObject(HGDIOBJ(old.0));
                    self.dib = Some((bitmap, original, bits.cast()));
                }
                None => self.dib = Some((bitmap, previous, bits.cast())),
            }
        }
        self.size = (w, h);
        Ok(())
    }

    /// Draws `scene` (the whole window, or only `clip` `[l, t, r, b]`).
    pub fn draw(&mut self, fonts: &Fonts, scene: &GuideScene, clip: Option<[i32; 4]>) -> Result<()> {
        self.ensure(scene.w, scene.h)?;
        let t = &self.target;
        unsafe {
            t.BindDC(self.dc, &RECT { left: 0, top: 0, right: scene.w, bottom: scene.h })?;
            t.BeginDraw();
            t.SetTransform(&Matrix3x2::identity());
            if let Some([l, top, r, b]) = clip {
                let rect = D2D_RECT_F { left: l as f32, top: top as f32, right: r as f32, bottom: b as f32 };
                t.PushAxisAlignedClip(&rect, D2D1_ANTIALIAS_MODE_ALIASED);
            }
            t.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            for line in &scene.lines {
                // Axis-aligned 1 px grid lines stay crisp without anti-aliasing.
                let mode = if line.width <= 1.0 { D2D1_ANTIALIAS_MODE_ALIASED } else { D2D1_ANTIALIAS_MODE_PER_PRIMITIVE };
                t.SetAntialiasMode(mode);
                self.brush.SetColor(&color(line.color));
                let stroke = line.dashed.then_some(&self.dash);
                t.DrawLine(Vector2 { X: line.from.0, Y: line.from.1 }, Vector2 { X: line.to.0, Y: line.to.1 }, &self.brush, line.width, stroke);
            }
            t.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
            let [l, top, r, b] = scene.margin;
            self.brush.SetColor(&color(crate::guide::MARGIN));
            t.DrawRectangle(&D2D_RECT_F { left: l, top, right: r, bottom: b }, &self.brush, 1.0, None);
            t.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            for label in &scene.labels {
                if let Some([l, top, r, b]) = label.plate {
                    let radius = (b - top) / 2.0;
                    let plate = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: l, top, right: r, bottom: b }, radiusX: radius, radiusY: radius };
                    self.brush.SetColor(&color(PLATE));
                    t.FillRoundedRectangle(&plate, &self.brush);
                }
                let layout = fonts.layout(label.role, scene.scale, &label.text)?;
                self.brush.SetColor(&color(label.color));
                t.DrawTextLayout(Vector2 { X: label.x, Y: label.y }, &layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
            if clip.is_some() {
                t.PopAxisAlignedClip();
            }
            t.EndDraw(None, None)?;
        }
        Ok(())
    }

    /// The premultiplied BGRA rows of the last draw.
    #[cfg(test)]
    pub fn pixels(&self) -> Vec<[u8; 4]> {
        let Some((_, _, bits)) = self.dib else { return Vec::new() };
        let n = (self.size.0 * self.size.1).max(0) as usize;
        unsafe { std::slice::from_raw_parts(bits.cast::<[u8; 4]>(), n) }.to_vec()
    }
}

impl Drop for GuideRenderer {
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
