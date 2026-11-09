//! Small icons for app sources: the icon Windows shows for an exe, read with the shell and
//! turned into a BGRA bitmap the UI draws. Read off the UI thread, together with the list of
//! playing apps, and cached by exe for the launch.
use std::path::Path;
use std::sync::Arc;

use gpui::RenderImage;
use windows::Win32::Graphics::Gdi::{BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits, GetObjectW, HGDIOBJ, ReleaseDC};
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::UI::Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_SMALLICON, SHGetFileInfoW};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};
use windows::core::HSTRING;

/// The exe's small shell icon as a BGRA image; `None` when Windows has none.
pub fn icon_for(path: &Path) -> Option<Arc<RenderImage>> {
    let (width, height, mut bgra) = small_icon_bgra(path)?;
    for pixel in bgra.chunks_exact_mut(4) {
        let luma = (54 * u16::from(pixel[2]) + 183 * u16::from(pixel[1]) + 19 * u16::from(pixel[0])) / 256;
        pixel[..3].fill((64 + luma * 191 / 255) as u8);
    }
    let buffer = image::RgbaImage::from_raw(width, height, bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

fn small_icon_bgra(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let mut info = SHFILEINFOW::default();
    // SAFETY: plain shell call writing into a sized struct; the icon it returns is destroyed below.
    let ok = unsafe { SHGetFileInfoW(&HSTRING::from(path.as_os_str()), FILE_FLAGS_AND_ATTRIBUTES(0), Some(&mut info), std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_ICON | SHGFI_SMALLICON) };
    if ok == 0 || info.hIcon.is_invalid() {
        return None;
    }
    let pixels = icon_pixels(info.hIcon);
    // SAFETY: the icon came from SHGetFileInfoW and is destroyed once.
    let _ = unsafe { DestroyIcon(info.hIcon) };
    pixels
}

/// The icon's colour bitmap as top-down BGRA; an icon with no alpha gets an opaque one.
fn icon_pixels(icon: HICON) -> Option<(u32, u32, Vec<u8>)> {
    // SAFETY: GDI objects from GetIconInfo are owned here and deleted once; the DC is released.
    unsafe {
        let mut parts = ICONINFO::default();
        GetIconInfo(icon, &mut parts).ok()?;
        let cleanup = |parts: &ICONINFO| {
            let _ = DeleteObject(HGDIOBJ(parts.hbmColor.0));
            let _ = DeleteObject(HGDIOBJ(parts.hbmMask.0));
        };
        if parts.hbmColor.is_invalid() {
            cleanup(&parts);
            return None;
        }
        let mut bitmap = BITMAP::default();
        if GetObjectW(HGDIOBJ(parts.hbmColor.0), std::mem::size_of::<BITMAP>() as i32, Some(&mut bitmap as *mut _ as *mut _)) == 0 {
            cleanup(&parts);
            return None;
        }
        let (width, height) = (bitmap.bmWidth.max(0) as u32, bitmap.bmHeight.max(0) as u32);
        let mut header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32), // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bgra = vec![0u8; (width * height * 4) as usize];
        let dc = GetDC(None);
        let lines = GetDIBits(dc, parts.hbmColor, 0, height, Some(bgra.as_mut_ptr() as *mut _), &mut header, DIB_RGB_COLORS);
        let _ = ReleaseDC(None, dc);
        cleanup(&parts);
        if lines == 0 || width == 0 || height == 0 {
            return None;
        }
        if bgra.chunks_exact(4).all(|px| px[3] == 0) {
            bgra.chunks_exact_mut(4).for_each(|px| px[3] = 255);
        }
        Some((width, height, bgra))
    }
}
