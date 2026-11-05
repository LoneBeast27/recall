//! The widest library tile any connected monitor can show, in physical pixels: thumbnails are
//! cached that wide, so they stay sharp on every monitor, portrait or landscape, at any window
//! size up to maximized, and the size does not change as the window moves or resizes
//! (Rohan, 2026-11-05: "take max res of both monitors and default to it").
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::core::BOOL;

use crate::library_state::TILE_MIN_WIDTH;

/// Gap between tiles (logical px); matches the grid.
const GAP: f32 = 16.0;
/// The window's minimum width (logical px).
const MIN_WINDOW: f32 = 1000.0;

/// Tile width for a grid `content` logical px wide (same rule as the grid's column fit).
pub fn tile_width(content: f32) -> f32 {
    let columns = (((content + GAP) / (TILE_MIN_WIDTH + GAP)).floor() as usize).clamp(2, 8) as f32;
    ((content - GAP * (columns - 1.0)) / columns).floor()
}

/// One monitor's work area width in physical px and its scale factor.
fn monitors() -> Vec<(f32, f32)> {
    unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, out: LPARAM) -> BOOL {
        let list = unsafe { &mut *(out.0 as *mut Vec<(f32, f32)>) };
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
            let width = (info.rcWork.right - info.rcWork.left) as f32;
            list.push((width, dpi_x.max(96) as f32 / 96.0));
        }
        true.into()
    }
    let mut list: Vec<(f32, f32)> = Vec::new();
    let _ = unsafe { EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut list as *mut _ as isize)) };
    list
}

/// Widest tile in physical px over every monitor and every window width from the minimum to
/// that monitor's work area. `chrome` is the window width (logical px) the grid does not get
/// (sidebar, padding), measured from the live window.
pub fn widest_tile(chrome: f32) -> u32 {
    widest_over(&monitors(), chrome)
}

pub(crate) fn widest_over(monitors: &[(f32, f32)], chrome: f32) -> u32 {
    let mut widest = 0.0f32;
    for &(physical, scale) in monitors {
        let logical = physical / scale;
        let mut window = MIN_WINDOW;
        while window <= logical.max(MIN_WINDOW) {
            widest = widest.max(tile_width(window - chrome) * scale);
            window += 1.0;
        }
    }
    widest.ceil() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_widest_tile_covers_every_monitor_and_window_size() {
        // A 2560 px landscape monitor at 100% and a 1440 px portrait one at 125%, 280 px of chrome.
        let one = widest_over(&[(2560.0, 1.0)], 280.0);
        let both = widest_over(&[(2560.0, 1.0), (1440.0, 1.25)], 280.0);
        assert!(both >= one, "adding a monitor never shrinks it");
        // Every window width on the landscape monitor fits under it.
        for window in 1000..=2560 {
            assert!(tile_width(window as f32 - 280.0) <= one as f32);
        }
        // The 125% monitor at its widest window counts in physical pixels.
        assert!(both as f32 >= tile_width(1440.0 / 1.25 - 280.0) * 1.25);
        assert_eq!(widest_over(&[], 280.0), 0, "no monitor known: the caller keeps its default");
    }
}
