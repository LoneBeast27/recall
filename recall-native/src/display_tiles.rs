//! The widest library tile any connected monitor can show, in physical pixels: thumbnails are
//! cached that wide, so they stay sharp on every monitor, portrait or landscape, at any window
//! size up to maximized, and the size does not change as the window moves or resizes
//! (the maintainer, 2026-11-05: "take max res of both monitors and default to it").
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::core::BOOL;

use crate::library_state::columns_for;

/// Gap between cards (logical px): the grid lays its rows out with this one.
pub const GAP: f32 = 16.0;
/// A card's picture height per width: the grid draws it at this shape.
pub const CARD_ASPECT: f32 = 9.0 / 16.0;
/// The window's minimum width (logical px).
const MIN_WINDOW: f32 = crate::app::MIN_WINDOW_WIDTH;

/// Tile width for a grid `content` logical px wide (same rule as the grid's column fit).
pub fn tile_width(content: f32, fixed: Option<usize>) -> f32 {
    let columns = columns_for(content, fixed) as f32;
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
pub fn widest_tile(chrome: f32, fixed: Option<usize>) -> u32 {
    widest_over(&monitors(), chrome, fixed)
}

pub(crate) fn widest_over(monitors: &[(f32, f32)], chrome: f32, fixed: Option<usize>) -> u32 {
    let mut widest = 0.0f32;
    for &(physical, scale) in monitors {
        let logical = physical / scale;
        let mut window = MIN_WINDOW;
        while window <= logical.max(MIN_WINDOW) {
            widest = widest.max(tile_width(window - chrome, fixed) * scale);
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
        let one = widest_over(&[(2560.0, 1.0)], 280.0, None);
        let both = widest_over(&[(2560.0, 1.0), (1440.0, 1.25)], 280.0, None);
        assert!(both >= one, "adding a monitor never shrinks it");
        // Every window width on the landscape monitor fits under it.
        for window in 1000..=2560 {
            assert!(tile_width(window as f32 - 280.0, None) <= one as f32);
        }
        // The 125% monitor at its widest window counts in physical pixels.
        assert!(both as f32 >= tile_width(1440.0 / 1.25 - 280.0, None) * 1.25);
        assert_eq!(widest_over(&[], 280.0, None), 0, "no monitor known: the caller keeps its default");
        // Three across makes wider tiles than the automatic fit: thumbnails are made that wide.
        let three = widest_over(&[(2560.0, 1.0)], 280.0, Some(3));
        assert!(three > one);
        assert_eq!(three as f32, tile_width(2560.0 - 280.0, Some(3)));
    }

    #[test]
    fn a_fixed_grid_never_makes_tiles_narrower_than_140() {
        assert_eq!(columns_for(1600.0, None), 6, "automatic: 220 px tiles");
        assert_eq!(columns_for(1600.0, Some(4)), 4);
        assert_eq!(columns_for(1600.0, Some(8)), 8);
        assert_eq!(columns_for(700.0, Some(8)), 4, "a narrow window shows fewer");
        assert_eq!(columns_for(700.0, None), 3);
        assert_eq!(columns_for(100.0, Some(5)), 1);
        assert_eq!(columns_for(1600.0, Some(40)), 8, "a hand-edited count stays in range");
        for width in [500.0, 720.0, 1000.0, 1900.0, 3000.0] {
            for n in 3..=8 {
                let columns = columns_for(width, Some(n)) as f32;
                assert!(tile_width(width, Some(n)) >= 140.0 || columns == 1.0, "{width} px, {n} across");
            }
        }
    }

    #[test]
    fn thumbnails_cover_every_grid_size_window_width_and_display_scale() {
        // (work area width in physical px, display scale): laptop to 8K, 100% to 300%, portrait.
        // Portrait ones are listed by their (short) width: 1080x1920 and 1440x2560 at 100% to 150%.
        let monitors = [
            (1366.0, 1.0), (1920.0, 1.0), (1920.0, 1.5), (2560.0, 1.0), (2560.0, 1.25), (3840.0, 1.5), (3840.0, 2.0),
            (3840.0, 3.0), (7680.0, 1.0), (1080.0, 1.0), (1080.0, 1.25), (1080.0, 1.5), (1440.0, 1.0), (1440.0, 1.25),
            (1440.0, 1.5), (2160.0, 2.0),
        ];
        let grids = [None, Some(1), Some(2), Some(3), Some(4), Some(5), Some(6), Some(7), Some(8)];
        // Chrome is measured from the live window: the sidebar shown (280) or a slimmer layout.
        for (&(physical, scale), chrome) in monitors.iter().flat_map(|m| [(m, 280.0), (m, 48.0)]) {
            for &grid in &grids {
                let widest = widest_over(&[(physical, scale)], chrome, grid);
                let mut largest = 0.0f32;
                let mut window = MIN_WINDOW;
                while window <= (physical / scale).max(MIN_WINDOW) {
                    let tile = tile_width(window - chrome, grid) * scale;
                    assert!(tile <= widest as f32, "{physical} px at {scale}x, {grid:?}: a {window} px window needs {tile}");
                    largest = largest.max(tile);
                    window += 1.0;
                }
                assert_eq!(widest, largest.ceil() as u32, "{physical} px at {scale}x, {grid:?}: no wider than the largest tile");
            }
        }
        // A portrait 1080 px monitor at 150% is 720 units wide, narrower than the 1000-unit
        // window minimum: the window overhangs it, so the cards are those of a 1000-unit window.
        assert_eq!(widest_over(&[(1080.0, 1.5)], 280.0, Some(3)), (tile_width(720.0, Some(3)) * 1.5).ceil() as u32);
        assert!(widest_over(&[(1080.0, 1.5)], 280.0, Some(8)) >= (140.0 * 1.5) as u32, "8 across gives way to fewer, wider cards");
        // A second monitor only ever raises the width.
        let one = widest_over(&monitors[..1], 280.0, Some(3));
        assert!(widest_over(&monitors, 280.0, Some(3)) >= one);
        // Fewer tiles across never makes them narrower.
        for &(physical, scale) in &monitors {
            let widths: Vec<u32> = (3..=8).map(|n| widest_over(&[(physical, scale)], 280.0, Some(n))).collect();
            assert!(widths.windows(2).all(|w| w[0] >= w[1]), "{physical} px at {scale}x: {widths:?}");
        }
    }
}
