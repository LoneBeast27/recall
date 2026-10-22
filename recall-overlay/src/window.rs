//! The overlay window itself: corner placement on the primary monitor's work
//! area and the layered, click-through, no-activate topmost popup.

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, POINT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, LoadCursorW, RegisterClassW, SetLayeredWindowAttributes, CS_HREDRAW,
    CS_VREDRAW, IDC_ARROW, LWA_ALPHA, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::{window_proc, Corner, HEIGHT_DIP, MARGIN_DIP, WIDTH_DIP};

pub(super) fn window_bounds(corner: Corner) -> Result<(i32, i32, i32, i32), String> {
    unsafe {
        let dpi = GetDpiForSystem().max(96) as i32;
        let scale = |dip: i32| dip * dpi / 96;
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return Err("GetMonitorInfoW failed".into());
        }
        let width = scale(WIDTH_DIP);
        let height = scale(HEIGHT_DIP);
        let margin = scale(MARGIN_DIP);
        let left = info.rcWork.left + margin;
        let right = info.rcWork.right - margin - width;
        let top = info.rcWork.top + margin;
        let bottom = info.rcWork.bottom - margin - height;
        Ok(match corner {
            Corner::TopLeft => (left, top, width, height),
            Corner::TopRight => (right, top, width, height),
            Corner::BottomLeft => (left, bottom, width, height),
            Corner::BottomRight => (right, bottom, width, height),
        })
    }
}

pub(super) fn create_window(x: i32, y: i32, width: i32, height: i32) -> Result<HWND, String> {
    unsafe {
        let module = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
            .map_err(|e| format!("GetModuleHandleW: {e}"))?;
        let instance = HINSTANCE(module.0);
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("RecallNativeStatusOverlay"),
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err("RegisterClassW failed".into());
        }
        let extended =
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST;
        let hwnd = CreateWindowExW(
            extended,
            w!("RecallNativeStatusOverlay"),
            w!("Recall status"),
            WS_POPUP,
            x,
            y,
            width,
            height,
            None,
            None,
            Some(instance),
            None,
        )
        .map_err(|e| format!("CreateWindowExW: {e}"))?;
        SetLayeredWindowAttributes(hwnd, COLORREF(0), 244, LWA_ALPHA)
            .map_err(|e| format!("SetLayeredWindowAttributes: {e}"))?;
        Ok(hwnd)
    }
}
