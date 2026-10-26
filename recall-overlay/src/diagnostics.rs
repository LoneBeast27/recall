//! `--diagnostics PATH`: the JSON snapshot of the overlay window (styles,
//! bounds, capture exclusion, reducer view, island state, stats sources,
//! fonts), rewritten on every visible change.

use std::fs;
use std::time::Instant;

use serde::Serialize;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, GetWindowRect, IsWindowVisible, GWL_EXSTYLE, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
};

use crate::app::App;
use crate::placement::{Rect, Spot};
use crate::reducer::OverlayView;
use crate::session::Session;
use crate::stats::StatsSnapshot;

#[derive(Serialize)]
struct IslandDiagnostics<'a> {
    content: &'static str,
    shown: bool,
    frame: [f32; 3],
    bounds_px: [i32; 4],
    game_key: Option<&'a str>,
    spot: Option<Spot>,
    monitor: Option<Rect>,
    scale: Option<f32>,
    stats_poller: bool,
    nvml_loaded: bool,
    stats: &'a StatsSnapshot,
    render_error: Option<&'a str>,
}

#[derive(Serialize)]
struct Diagnostics<'a> {
    visible: bool,
    process_id: u32,
    parent_process_id: Option<u32>,
    corner_arg: &'a str,
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
    session: &'a Session,
    island: IslandDiagnostics<'a>,
    last_status_age_ms: Option<u128>,
    font_families: (String, String),
}

pub(super) fn write_diagnostics(app: &App, hwnd: HWND) {
    let Some(path) = app.diagnostics_path.as_ref() else { return };
    let now = Instant::now();
    let mut rect = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut rect);
    }
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    let required = (WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST).0;
    let frame = app.island.frame(now);
    let placed = app.placed.as_ref();
    let diagnostics = Diagnostics {
        visible: unsafe { IsWindowVisible(hwnd).as_bool() },
        process_id: std::process::id(),
        parent_process_id: app.parent_pid,
        corner_arg: &app.corner,
        hwnd: hwnd.0 as isize,
        bounds_px: [rect.left, rect.top, rect.right, rect.bottom],
        extended_style: format!("0x{style:08x}"),
        required_extended_style: format!("0x{required:08x}"),
        click_through: style & WS_EX_TRANSPARENT.0 != 0,
        no_activate: style & WS_EX_NOACTIVATE.0 != 0,
        tool_window: style & WS_EX_TOOLWINDOW.0 != 0,
        topmost: style & WS_EX_TOPMOST.0 != 0,
        capture_exclusion_requested: app.capture_exclusion_requested,
        capture_exclusion_result: &app.capture_exclusion_result,
        state: app.reducer.view(),
        session: app.reducer.session(),
        island: IslandDiagnostics {
            content: app.island.content().map_or("hidden", |c| c.name()),
            shown: app.island.shown(),
            frame: [frame.w, frame.h, frame.alpha],
            bounds_px: app.bounds,
            game_key: placed.map(|p| p.key.as_str()),
            spot: placed.map(|p| p.spot),
            monitor: placed.map(|p| p.monitor),
            scale: placed.map(|p| p.scale),
            stats_poller: app.poller_active(),
            nvml_loaded: app.nvml_loaded(),
            stats: &app.stats,
            render_error: app.render_error.as_deref(),
        },
        last_status_age_ms: app.reducer.last_status_age(now).map(|age| age.as_millis()),
        font_families: app.fonts.family_names(),
    };
    if let Ok(json) = serde_json::to_vec_pretty(&diagnostics) {
        let _ = fs::write(path, json);
    }
}
