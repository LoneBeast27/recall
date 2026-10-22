//! GDI rendering of the status card (Recall design system: Geist type, safety
//! dot, hairline edge), the process-private font load, and the `--smoke-render`
//! BMP dump.

use std::fs;
use std::path::PathBuf;

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    AddFontMemResourceEx, CreateCompatibleDC, CreateDIBSection, CreateFontW, CreateRoundRectRgn,
    CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, Ellipse, FillRect, FrameRgn, GetTextFaceW,
    SelectObject, SetBkMode, SetTextColor, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CLEARTYPE_QUALITY,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, DT_END_ELLIPSIS,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, FW_MEDIUM, FW_SEMIBOLD, HDC, HGDIOBJ,
    OUT_DEFAULT_PRECIS, TRANSPARENT,
};
use windows::Win32::UI::HiDpi::GetDpiForSystem;

use crate::reducer::{OverlayView, RecorderState};
use crate::{HEIGHT_DIP, WIDTH_DIP};

pub(super) unsafe fn draw_overlay(
    hdc: HDC,
    width: i32,
    height: i32,
    dpi: i32,
    view: &OverlayView,
) -> (String, String) {
    // Recall design system: raised card #121212, #282828 hairline, Geist type,
    // safety-colour dot, tertiary detail line. Two lines: what happened + detail.
    let scale = |dip: i32| dip * dpi / 96;
    let card = CreateSolidBrush(rgb(0x12, 0x12, 0x12));
    let edge = CreateSolidBrush(rgb(0x28, 0x28, 0x28));
    FillRect(hdc, &RECT { left: 0, top: 0, right: width, bottom: height }, card);
    let radius = scale(10);
    let outline = CreateRoundRectRgn(0, 0, width + 1, height + 1, radius * 2, radius * 2);
    let _ = FrameRgn(hdc, outline, edge, 1, 1);
    let dot_color = match view.state {
        RecorderState::Ready => rgb(0x81, 0xc9, 0x95),
        RecorderState::Starting => rgb(0xfd, 0xd6, 0x63),
        RecorderState::Failed => rgb(0xf2, 0x8b, 0x82),
        RecorderState::Stopped => rgb(0xa3, 0xa3, 0xa3),
    };
    let dot = CreateSolidBrush(dot_color);
    let old_brush = SelectObject(hdc, HGDIOBJ(dot.0));
    let mid = height / 2;
    let _ = Ellipse(hdc, scale(15), mid - scale(13), scale(22), mid - scale(6));
    SelectObject(hdc, old_brush);
    SetBkMode(hdc, TRANSPARENT);
    let font = |px: i32, weight: i32| {
        CreateFontW(-scale(px), 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY, DEFAULT_PITCH.0 as u32, w!("Geist"))
    };
    let title_font = font(14, FW_SEMIBOLD.0 as i32);
    let body_font = font(12, FW_MEDIUM.0 as i32);
    let original_font = SelectObject(hdc, HGDIOBJ(title_font.0));
    let title_face = selected_font_face(hdc);
    let left = scale(30);
    draw_text(hdc, &view.primary, RECT { left, top: mid - scale(20), right: width - scale(14), bottom: mid + scale(1) }, rgb(0xed, 0xed, 0xed));
    SelectObject(hdc, HGDIOBJ(body_font.0));
    let body_face = selected_font_face(hdc);
    draw_text(hdc, &view.secondary, RECT { left, top: mid + scale(1), right: width - scale(14), bottom: mid + scale(20) }, rgb(0x8f, 0x8f, 0x8f));
    SelectObject(hdc, original_font);
    for object in [HGDIOBJ(card.0), HGDIOBJ(edge.0), HGDIOBJ(dot.0), HGDIOBJ(title_font.0), HGDIOBJ(body_font.0), HGDIOBJ(outline.0)] {
        let _ = DeleteObject(object);
    }
    (title_face, body_face)
}

/// Loads Geist (OFL, shared with the app) as process-private fonts for GDI.
pub(super) fn load_fonts() {
    for bytes in [
        include_bytes!("../../recall-native/assets/fonts/geist-500.ttf").as_slice(),
        include_bytes!("../../recall-native/assets/fonts/geist-600.ttf").as_slice(),
    ] {
        let mut installed = 0u32;
        unsafe { AddFontMemResourceEx(bytes.as_ptr().cast(), bytes.len() as u32, None, &mut installed) };
    }
}

unsafe fn selected_font_face(hdc: HDC) -> String {
    let length = GetTextFaceW(hdc, None);
    if length <= 0 {
        return "unknown".into();
    }
    let mut name = vec![0u16; length as usize];
    let copied = GetTextFaceW(hdc, Some(&mut name));
    if copied <= 0 {
        return "unknown".into();
    }
    let used = name
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(copied as usize);
    String::from_utf16_lossy(&name[..used])
}

pub(super) fn render_smoke_bmp(path: &PathBuf, view: &OverlayView) -> Result<(), String> {
    unsafe {
        let dpi = GetDpiForSystem().max(96) as i32;
        let width = WIDTH_DIP * dpi / 96;
        let height = HEIGHT_DIP * dpi / 96;
        let memory_dc = CreateCompatibleDC(None);
        if memory_dc.0.is_null() {
            return Err("CreateCompatibleDC failed for smoke render".into());
        }
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: (width * height * 4) as u32,
                biXPelsPerMeter: dpi * 10_000 / 254,
                biYPelsPerMeter: dpi * 10_000 / 254,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(
            Some(memory_dc),
            &mut info,
            DIB_RGB_COLORS,
            &mut bits,
            None,
            0,
        )
        .map_err(|e| format!("CreateDIBSection for smoke render: {e}"))?;
        let original_bitmap = SelectObject(memory_dc, HGDIOBJ(bitmap.0));
        let _ = draw_overlay(memory_dc, width, height, dpi, view);

        let pixels = std::slice::from_raw_parts(bits.cast::<u8>(), (width * height * 4) as usize);
        let row_bytes = width as usize * 3;
        let row_stride = (row_bytes + 3) & !3;
        let image_bytes = row_stride * height as usize;
        let mut bmp = Vec::with_capacity(54 + image_bytes);
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&(54u32 + image_bytes as u32).to_le_bytes());
        bmp.extend_from_slice(&[0; 4]);
        bmp.extend_from_slice(&54u32.to_le_bytes());
        bmp.extend_from_slice(&40u32.to_le_bytes());
        bmp.extend_from_slice(&width.to_le_bytes());
        bmp.extend_from_slice(&height.to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&24u16.to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes());
        bmp.extend_from_slice(&(image_bytes as u32).to_le_bytes());
        bmp.extend_from_slice(&(dpi * 10_000 / 254).to_le_bytes());
        bmp.extend_from_slice(&(dpi * 10_000 / 254).to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes());
        for y in (0..height as usize).rev() {
            let source = &pixels[y * width as usize * 4..(y + 1) * width as usize * 4];
            for pixel in source.chunks_exact(4) {
                bmp.extend_from_slice(&pixel[..3]);
            }
            bmp.resize(bmp.len() + (row_stride - row_bytes), 0);
        }
        fs::write(path, bmp).map_err(|e| format!("write smoke render {}: {e}", path.display()))?;
        SelectObject(memory_dc, original_bitmap);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(memory_dc);
        Ok(())
    }
}

unsafe fn draw_text(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    text: &str,
    mut rect: RECT,
    color: COLORREF,
) {
    SetTextColor(hdc, color);
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    DrawTextW(
        hdc,
        &mut wide,
        &mut rect,
        DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
    );
}

const fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    COLORREF(red as u32 | ((green as u32) << 8) | ((blue as u32) << 16))
}
