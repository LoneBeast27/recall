//! Free helpers of the pump loop, shared with the unified record mode: the
//! message drain, the wait clamp, WatchEvent -> Foreground, the primary
//! monitor, and the engine-health summary.

use std::time::Instant;

use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, HMONITOR, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE, WM_QUIT,
};

use crate::rules::Foreground;
use crate::supervisor::EngineHealth;
use crate::watcher::WatchEvent;

/// Drain the thread message queue with `PeekMessageW`. Returns `true` if a
/// `WM_QUIT` was seen. Dispatching drives the out-of-context WinEvent callback,
/// the tray popup menu, and the global hotkey.
pub(crate) fn drain_messages() -> bool {
    let mut msg = MSG::default();
    loop {
        let got = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) };
        if !got.as_bool() {
            return false;
        }
        if msg.message == WM_QUIT {
            return true;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Milliseconds until `deadline` (0 if passed), capped at `cap`.
pub(crate) fn wait_ms(deadline: Option<Instant>, cap: u32) -> u32 {
    match deadline {
        Some(d) => d.saturating_duration_since(Instant::now()).as_millis().min(cap as u128) as u32,
        None => cap,
    }
}

/// The WatchEvent -> rule-engine Foreground conversion (stem lowercased).
pub(crate) fn foreground_of(ev: &WatchEvent) -> Foreground {
    Foreground {
        exe_stem: ev.exe_stem.to_ascii_lowercase(),
        title: ev.title.clone(),
        // A resolved UWP identity earns the fullscreen default only as a known game.
        fullscreen_on_target: crate::record_hosted::hdr_fullscreen(ev),
        pid: ev.pid,
        game_evidence: crate::record_target::game_evidence(ev),
    }
}

/// `%APPDATA%\recall-hdr\recall-hdr.log` -- mirrors `logging::log_path` (which is
/// private to that module) for the tray's "Open log" helper.
pub(super) fn log_file_path() -> std::path::PathBuf {
    let base = std::env::var("APPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("recall-hdr").join("recall-hdr.log")
}

/// The primary monitor's `HMONITOR` as an `isize` (the watcher's target format).
/// Fullscreen attribution assumes the managed display is the primary (the default setting).
pub(crate) fn primary_monitor_isize() -> isize {
    let mon: HMONITOR = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    mon.0 as isize
}

/// A short, human summary of engine health for the status line.
pub(crate) fn health_summary(h: EngineHealth) -> &'static str {
    match h {
        EngineHealth::Running { .. } => "running",
        EngineHealth::Restarting { .. } => "restarting",
        EngineHealth::GivenUp => "off",
        EngineHealth::Stopped => "stopped",
    }
}
