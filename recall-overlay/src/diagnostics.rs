//! `--diagnostics PATH`: the JSON snapshot of the overlay window (styles,
//! bounds, capture exclusion, reducer view, fonts) rewritten on every change.

use std::fs;
use std::time::Instant;

use serde::Serialize;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, GetWindowRect, IsWindowVisible, GWL_EXSTYLE, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
};

use crate::reducer::OverlayView;
use crate::APP;

#[derive(Serialize)]
struct Diagnostics<'a> {
    visible: bool,
    process_id: u32,
    parent_process_id: Option<u32>,
    hwnd: isize,
    bounds_px: [i32; 4],
    extended_style: String,
    required_extended_style: String,
    click_through: bool,
    no_activate: bool,
    tool_window: bool,
    topmost: bool,
    capture_exclusion_requested: bool,
    capture_exclusion_result: &'a str,
    state: &'a OverlayView,
    last_status_age_ms: Option<u128>,
    selected_title_font: &'a str,
    selected_body_font: &'a str,
}

pub(super) fn write_diagnostics(hwnd: HWND) {
    APP.with(|slot| {
        let app = slot.borrow();
        let Some(app) = app.as_ref() else { return };
        let Some(path) = app.diagnostics_path.as_ref() else {
            return;
        };
        let now = Instant::now();
        let mut rect = RECT::default();
        unsafe {
            let _ = GetWindowRect(hwnd, &mut rect);
        }
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
        let required = (WS_EX_LAYERED
            | WS_EX_TRANSPARENT
            | WS_EX_NOACTIVATE
            | WS_EX_TOOLWINDOW
            | WS_EX_TOPMOST)
            .0;
        let diagnostics = Diagnostics {
            visible: unsafe { IsWindowVisible(hwnd).as_bool() },
            process_id: std::process::id(),
            parent_process_id: app.parent_pid,
            hwnd: hwnd.0 as isize,
            bounds_px: [rect.left, rect.top, rect.right, rect.bottom],
            extended_style: format!("0x{style:08x}"),
            required_extended_style: format!("0x{required:08x}"),
            click_through: style & (WS_EX_TRANSPARENT.0) != 0,
            no_activate: style & (WS_EX_NOACTIVATE.0) != 0,
            tool_window: style & (WS_EX_TOOLWINDOW.0) != 0,
            topmost: style & (WS_EX_TOPMOST.0) != 0,
            capture_exclusion_requested: app.capture_exclusion_requested,
            capture_exclusion_result: &app.capture_exclusion_result,
            state: app.reducer.view(),
            last_status_age_ms: app.reducer.last_status_age(now).map(|age| age.as_millis()),
            selected_title_font: &app.selected_title_font,
            selected_body_font: &app.selected_body_font,
        };
        if let Ok(json) = serde_json::to_vec_pretty(&diagnostics) {
            let _ = fs::write(path, json);
        }
    });
}
