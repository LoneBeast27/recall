//! The overlay window: a layered, click-through, no-activate, topmost tool
//! popup whose pixels come from `UpdateLayeredWindow` (so it must never call
//! `SetLayeredWindowAttributes`), and the monitor the island sits on.

use windows::core::{w, BOOL};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, POINT};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HMONITOR, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, EnumWindows, GetClientRect, GetWindowRect, GetWindowThreadProcessId, IsWindow, IsWindowVisible, LoadCursorW,
    RegisterClassW, IDC_ARROW, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::placement::Rect;
use crate::window_proc;

pub(super) fn create_window() -> Result<HWND, String> {
    unsafe {
        let module = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
            .map_err(|e| format!("GetModuleHandleW: {e}"))?;
        let instance = HINSTANCE(module.0);
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("RecallNativeStatusOverlay"),
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err("RegisterClassW failed".into());
        }
        let extended = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST;
        CreateWindowExW(
            extended,
            w!("RecallNativeStatusOverlay"),
            w!("Recall status"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance),
            None,
        )
        .map_err(|e| format!("CreateWindowExW: {e}"))
    }
}

/// Where the target is: its monitor, the area the island may use on it, and
/// the game picture (a windowed game's whole client rect, else the area): the
/// shape the engine's HUD scans measure, so scans are kept and looked up by it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Where {
    pub monitor: Rect,
    pub area: Rect,
    pub picture: Rect,
}

/// The monitor of the game window (`hwnd` while it exists and shows, else
/// the pid's largest visible top-level window), else the primary monitor,
/// and the area the island may use on it ([`island_area`]). `on_window`:
/// Dynamic, which places a windowed game's island on the game picture.
pub(super) fn target_monitor(hwnd: Option<isize>, pid: Option<u32>, on_window: bool) -> Where {
    let window = hwnd
        .map(|h| HWND(h as *mut _))
        .filter(|&h| unsafe { IsWindow(Some(h)).as_bool() && IsWindowVisible(h).as_bool() })
        .or_else(|| pid.and_then(main_window_of));
    let monitor = match window {
        Some(window) => unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTOPRIMARY) },
        None => unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) },
    };
    let (full, work) = monitor_rects(monitor);
    let frame = window.and_then(|w| {
        let mut r = Default::default();
        unsafe { GetWindowRect(w, &mut r) }.ok().map(|()| rect(r))
    });
    let client = window.and_then(client_rect);
    let area = island_area(full, work, frame, client, on_window);
    Where { monitor: full, area, picture: picture(full, frame, client, on_window, area) }
}

/// The game picture: a windowed game's whole client rect (unclipped: the
/// engine measures all of it), else the island's area.
pub fn picture(full: Rect, window: Option<Rect>, client: Option<Rect>, on_window: bool, area: Rect) -> Rect {
    let windowed = on_window && !window.is_some_and(|r| covers(r, full));
    match client {
        Some(c) if windowed && c.right > c.left && c.height() > 0 => c,
        _ => area,
    }
}

/// The window's HUD scans place the island only when the island's area lies
/// on the game picture they measured (else the area fell back to the monitor
/// corners: a small window).
pub fn scans_apply(area: Rect, picture: Rect) -> bool {
    area.left >= picture.left && area.top >= picture.top && area.right <= picture.right && area.bottom <= picture.bottom
}

fn covers(window: Rect, full: Rect) -> bool {
    window.left <= full.left && window.top <= full.top && window.right >= full.right && window.bottom >= full.bottom
}

/// A grown island (an action notice) is about this wide at 1440p.
const ISLAND_W: f32 = 340.0;

/// Room for the island in a windowed game: at least twice its width, and
/// that width's 16:9 height, at the monitor's scale (smaller windows keep
/// the monitor corners).
fn room_for_island(r: &Rect, full: &Rect) -> bool {
    let k = (full.right - full.left).min(full.height()).max(1) as f32 / crate::placement::REF_HEIGHT;
    let width = 2.0 * ISLAND_W * k;
    (r.right - r.left) as f32 >= width && r.height() as f32 >= width * 9.0 / 16.0
}

fn rect(r: windows::Win32::Foundation::RECT) -> Rect {
    Rect { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
}

/// The window's client area in screen coordinates (the game picture). Both
/// corners go through `ClientToScreen`, so a DPI-virtualized window's size
/// is mapped the same way as its origin.
fn client_rect(window: HWND) -> Option<Rect> {
    let mut r = Default::default();
    unsafe { GetClientRect(window, &mut r) }.ok()?;
    let mut corners = [POINT { x: 0, y: 0 }, POINT { x: r.right, y: r.bottom }];
    let mapped = corners.iter_mut().all(|p| unsafe { ClientToScreen(window, p) }.as_bool());
    mapped.then(|| client_from_points(corners[0], corners[1]))
}

/// The client rect from its two corners in screen coordinates.
fn client_from_points(top_left: POINT, bottom_right: POINT) -> Rect {
    Rect { left: top_left.x, top: top_left.y, right: bottom_right.x, bottom: bottom_right.y }
}

pub(super) fn monitor_rects(monitor: HMONITOR) -> (Rect, Rect) {
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        let fallback = Rect { left: 0, top: 0, right: 1920, bottom: 1080 };
        return (fallback, fallback);
    }
    (rect(info.rcMonitor), rect(info.rcWork))
}

struct Search {
    pid: u32,
    best: Option<(HWND, i64)>,
}

fn main_window_of(pid: u32) -> Option<HWND> {
    let mut search = Search { pid, best: None };
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize));
    }
    search.best.map(|(hwnd, _)| hwnd)
}

unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let search = unsafe { &mut *(lparam.0 as *mut Search) };
    let mut owner = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner as *mut u32)) };
    if owner == search.pid && unsafe { IsWindowVisible(hwnd) }.as_bool() {
        let mut rect = Default::default();
        if unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok() {
            let area = i64::from(rect.right - rect.left) * i64::from(rect.bottom - rect.top);
            if search.best.is_none_or(|(_, best)| area > best) {
                search.best = Some((hwnd, area));
            }
        }
    }
    BOOL(1)
}

/// Where the island may sit: the whole monitor while the game window covers
/// it, else the work area; with `on_window` (Dynamic) a windowed game's client
/// rectangle within the work area, so the HUD grid maps onto the game picture
/// (fullscreen hides the taskbar; the work area keeps it clear). A client
/// rectangle under about twice the island within the work area
/// ([`room_for_island`]) falls back to the work area's corners.
pub fn island_area(full: Rect, work: Rect, window: Option<Rect>, client: Option<Rect>, on_window: bool) -> Rect {
    if window.is_some_and(|r| covers(r, full)) {
        return full;
    }
    let inside = client.filter(|_| on_window).map(|c| Rect {
        left: c.left.max(work.left),
        top: c.top.max(work.top),
        right: c.right.min(work.right),
        bottom: c.bottom.min(work.bottom),
    });
    inside.filter(|r| room_for_island(r, &full)).unwrap_or(work)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect { left, top, right, bottom }
    }

    #[test]
    fn dynamic_maps_a_windowed_game_onto_its_client_rect() {
        let (full, work) = (rect(0, 0, 2560, 1440), rect(0, 0, 2560, 1392));
        let frame = rect(92, 69, 1388, 830);
        let client = rect(100, 100, 1380, 820);
        assert_eq!(island_area(full, work, Some(frame), Some(client), true), client);
        assert_eq!(island_area(full, work, Some(frame), Some(client), false), work, "a fixed corner keeps the monitor");
        assert_eq!(island_area(full, work, Some(full), Some(full), true), full, "a covering game is the monitor");
        let hanging = rect(2000, 1000, 3280, 1720);
        assert_eq!(island_area(full, work, Some(hanging), Some(hanging), true), work, "560 x 392 left on screen: under twice the island");
        let minimised = rect(-32000, -32000, -31840, -31972);
        assert_eq!(island_area(full, work, Some(minimised), Some(minimised), true), work);
        let wide = rect(1200, 600, 2800, 1500);
        assert_eq!(island_area(full, work, Some(wide), Some(wide), true), rect(1200, 600, 2560, 1392), "clipped, still big enough");
        let tiny = rect(10, 10, 110, 60);
        assert_eq!(island_area(full, work, Some(tiny), Some(tiny), true), work, "too small for the island");
    }

    #[test]
    fn a_window_under_twice_the_island_keeps_the_monitor_corners() {
        let (full, work) = (rect(0, 0, 2560, 1440), rect(0, 0, 2560, 1392));
        let small = rect(100, 100, 740, 460);
        assert_eq!(island_area(full, work, Some(small), Some(small), true), work, "640 x 360 on a 1440p monitor");
        let enough = rect(100, 100, 900, 550);
        assert_eq!(island_area(full, work, Some(enough), Some(enough), true), enough, "800 x 450 fits twice the island");
        let (full_hd, work_hd) = (rect(0, 0, 1920, 1080), rect(0, 0, 1920, 1040));
        assert_eq!(island_area(full_hd, work_hd, Some(small), Some(small), true), small, "640 x 360 on 1080p: the island is smaller there");
    }

    #[test]
    fn the_scan_shape_is_the_whole_game_picture_not_the_clipped_area() {
        let (full, work) = (rect(0, 0, 2560, 1440), rect(0, 0, 2560, 1392));
        let client = rect(1000, 600, 2600, 1500); // 1600 x 900 (16:9), partly off the monitor
        let area = island_area(full, work, Some(client), Some(client), true);
        assert_eq!(area, rect(1000, 600, 2560, 1392));
        let seen = picture(full, Some(client), Some(client), true, area);
        let scan = serde_json::json!({"aspect": 1600.0 / 900.0});
        let stored = crate::dynamic::scan_key("game@10", &scan);
        assert_eq!(crate::dynamic::shape_key("game", seen.aspect()), stored, "looked up where it was kept");
        assert_eq!(picture(full, Some(full), Some(full), true, full), full, "a covering game: the monitor");
        assert_eq!(picture(full, Some(client), Some(client), false, work), work, "a fixed corner: the area");
    }

    #[test]
    fn the_client_rect_is_both_mapped_corners() {
        let r = client_from_points(POINT { x: 150, y: 150 }, POINT { x: 2070, y: 1230 });
        assert_eq!(r, rect(150, 150, 2070, 1230));
    }

    #[test]
    fn a_small_windows_scans_never_place_the_island_on_the_monitor() {
        let (full, work) = (rect(0, 0, 2560, 1440), rect(0, 0, 2560, 1392));
        let shape = |client: Rect| {
            let area = island_area(full, work, Some(client), Some(client), true);
            scans_apply(area, picture(full, Some(client), Some(client), true, area))
        };
        assert!(!shape(rect(100, 100, 740, 460)), "640 x 360 on 1440p: the monitor corners, not the window's grid");
        assert!(shape(rect(100, 100, 900, 550)), "800 x 450: on the game picture");
        assert!(shape(rect(1000, 600, 2600, 1500)), "clipped by the screen edge, still the picture");
        assert!(!shape(rect(2000, 1000, 3280, 1720)), "hanging off the screen: the work area");
        assert!(shape(full), "a covering game");
        assert!(scans_apply(work, work), "a fixed corner or the desktop");
    }
}
