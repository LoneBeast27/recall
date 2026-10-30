#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! recall-overlay: the Recall Island. A click-through, no-activate,
//! capture-excluded topmost pill at each game's free top spot that flashes
//! notices and, while the stats toggle is on, shows FPS / GPU / CPU.

mod anim;
mod app;
mod cue;
mod diagnostics;
mod export;
mod filmstrip;
mod fonts;
mod island;
mod layout;
mod notice;
mod placement;
mod png;
mod preview;
mod reducer;
mod render;
mod session;
mod smoke;
mod stats;
mod window;

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{GenericNamespaced, Stream, ToNsName};
use serde_json::Value;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, PostMessageW, PostQuitMessage, SetTimer,
    SetWindowDisplayAffinity, TranslateMessage, HTTRANSPARENT, MSG, WDA_EXCLUDEFROMCAPTURE, WM_APP, WM_CLOSE,
    WM_DESTROY, WM_ERASEBKGND, WM_NCHITTEST, WM_PAINT, WM_TIMER,
};

use app::{App, Feed, ANIM_TIMER, BASE_MS, BASE_TIMER};
use diagnostics::write_diagnostics;
use fonts::Fonts;
use reducer::Reducer;
use render::Renderer;
use smoke::Smoke;
use window::create_window;

const PIPE_NAME: &str = "recall-hdr-control";
/// Posted by the pipe thread so a notice lands at once, not on the next tick.
const WM_FEED: u32 = WM_APP + 1;
const CORNERS: [&str; 4] = ["top-left", "top-right", "bottom-left", "bottom-right"];
const USAGE: &str = "usage: recall-overlay --parent PID [--corner top-left|top-right|bottom-left|bottom-right] \
[--diagnostics PATH] | --smoke-state FIXTURE.json [--smoke-allow-capture] [--smoke-render PATH.bmp] [--diagnostics PATH] \
| --preview-sheet PATH.bmp [--preview-scale S] | --preview-notice PATH.bmp [--preview-scale S] \
| --export-frames DIR | --frame-timing-probe PID [--probe-seconds N]";

#[derive(Debug, Default)]
struct Options {
    /// Accepted for the controller's launch line; the island's placement is per game.
    corner: String,
    parent: Option<u32>,
    diagnostics: Option<PathBuf>,
    smoke_state: Option<PathBuf>,
    smoke_allow_capture: bool,
    smoke_render: Option<PathBuf>,
    preview_sheet: Option<PathBuf>,
    /// `--preview-notice PATH.bmp`: the action notice's transformation filmstrips.
    preview_notice: Option<PathBuf>,
    /// `--export-frames DIR`: the web preview's frames (atlas + manifest).
    export_frames: Option<PathBuf>,
    preview_scale: Option<f32>,
    /// `--frame-timing-probe PID`: measure one process's FPS, print, exit.
    probe_pid: Option<u32>,
    probe_seconds: Option<u64>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn main() {
    if let Err(error) = run() {
        eprintln!("recall-overlay: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let options = parse_args()?;
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    stats::install_cleanup();
    if let Some(path) = options.preview_sheet.as_ref() {
        return preview::sheet(path, options.preview_scale.unwrap_or(1.0));
    }
    if let Some(dir) = options.export_frames.as_ref() {
        return export::export(dir);
    }
    if let Some(path) = options.preview_notice.as_ref() {
        return filmstrip::notice_filmstrip(path, options.preview_scale.unwrap_or(1.0));
    }
    if let Some(pid) = options.probe_pid {
        return stats::probe::run(pid, options.probe_seconds.unwrap_or(5));
    }
    if options.parent.is_none() && options.smoke_state.is_none() {
        return Err("--parent PID is required outside --smoke-state test mode".into());
    }
    if (options.smoke_allow_capture || options.smoke_render.is_some()) && options.smoke_state.is_none() {
        return Err("--smoke-allow-capture / --smoke-render require --smoke-state".into());
    }
    let parent_handle = match options.parent {
        Some(pid) => Some(
            unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }.map_err(|e| format!("open parent process {pid}: {e}"))?,
        ),
        None => None,
    };
    let fonts = Fonts::new().map_err(|e| format!("DirectWrite fonts: {e}"))?;
    let renderer = Renderer::new().map_err(|e| format!("Direct2D renderer: {e}"))?;
    let (tx, rx) = mpsc::channel();
    let hwnd = create_window()?;
    let capture_exclusion_result = if options.smoke_allow_capture {
        "disabled_for_smoke_capture".to_string()
    } else {
        match unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) } {
            Ok(()) => "enabled".to_string(),
            Err(error) => format!("unsupported_or_failed: {error}"),
        }
    };

    let mut app = App::new(Reducer::new(), rx, renderer, fonts);
    app.parent_handle = parent_handle;
    app.parent_pid = options.parent;
    app.corner = options.corner.clone();
    app.diagnostics_path = options.diagnostics.clone();
    app.capture_exclusion_requested = !options.smoke_allow_capture;
    app.capture_exclusion_result = capture_exclusion_result;
    match options.smoke_state.as_ref() {
        Some(path) => {
            let mut smoke = Smoke::load(path)?;
            smoke.apply_due(&mut app.reducer, Duration::ZERO, Instant::now());
            app.smoke = Some(smoke);
        }
        None => spawn_pipe_client(tx, hwnd.0 as isize),
    }
    if let Some(path) = options.smoke_render.as_ref() {
        app.snapshot(path)?;
    }
    write_diagnostics(&app, hwnd);
    APP.with(|slot| *slot.borrow_mut() = Some(app));
    unsafe {
        SetTimer(Some(hwnd), BASE_TIMER, BASE_MS, None);
        let _ = PostMessageW(Some(hwnd), WM_FEED, WPARAM(0), LPARAM(0));
    }

    let mut message = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
        if result.0 == -1 {
            return Err("GetMessageW failed".into());
        }
        if result.0 == 0 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

fn parse_args() -> Result<Options, String> {
    let mut options = Options { corner: "top-right".into(), ..Options::default() };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |what: &str| args.next().ok_or(format!("{arg} requires {what}"));
        match arg.as_str() {
            "--corner" => {
                let corner = value("a value")?;
                if !CORNERS.contains(&corner.as_str()) {
                    return Err(format!("invalid corner: {corner}"));
                }
                options.corner = corner;
            }
            "--parent" => {
                let raw = value("a PID")?;
                let pid = raw.parse::<u32>().map_err(|_| format!("invalid parent PID: {raw}"))?;
                if pid == 0 {
                    return Err("parent PID must be positive".into());
                }
                options.parent = Some(pid);
            }
            "--diagnostics" => options.diagnostics = Some(PathBuf::from(value("a JSON path")?)),
            "--smoke-state" => options.smoke_state = Some(PathBuf::from(value("a JSON fixture path")?)),
            "--smoke-allow-capture" => options.smoke_allow_capture = true,
            "--smoke-render" => options.smoke_render = Some(PathBuf::from(value("a BMP path")?)),
            "--preview-sheet" => options.preview_sheet = Some(PathBuf::from(value("a BMP path")?)),
            "--preview-notice" => options.preview_notice = Some(PathBuf::from(value("a BMP path")?)),
            "--export-frames" => options.export_frames = Some(PathBuf::from(value("a directory")?)),
            "--preview-scale" => {
                let raw = value("a number")?;
                options.preview_scale = Some(raw.parse().map_err(|_| format!("invalid scale: {raw}"))?);
            }
            "--frame-timing-probe" => {
                let raw = value("a PID")?;
                options.probe_pid = Some(raw.parse().map_err(|_| format!("invalid PID: {raw}"))?);
            }
            "--probe-seconds" => {
                let raw = value("a number")?;
                options.probe_seconds = Some(raw.parse().map_err(|_| format!("invalid seconds: {raw}"))?);
            }
            "--help" | "-h" => return Err(USAGE.into()),
            _ => return Err(format!("unknown argument: {arg}\n{USAGE}")),
        }
    }
    Ok(options)
}

fn spawn_pipe_client(tx: Sender<Feed>, hwnd: isize) {
    let wake = move || unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_FEED, WPARAM(0), LPARAM(0));
    };
    std::thread::Builder::new()
        .name("recall-overlay-pipe".into())
        .spawn(move || loop {
            let Ok(name) = PIPE_NAME.to_ns_name::<GenericNamespaced>() else { return };
            if let Ok(stream) = Stream::connect(name) {
                let (reader, mut writer) = stream.split();
                let _ = tx.send(Feed::Connected);
                let _ = writer.write_all(b"{\"cmd\":\"status\"}\n");
                for line in BufReader::new(reader).lines() {
                    let Ok(line) = line else { break };
                    if let Ok(value) = serde_json::from_str::<Value>(line.trim()) {
                        if tx.send(Feed::Json(value)).is_err() {
                            return;
                        }
                        wake();
                    }
                }
            }
            if tx.send(Feed::Disconnected).is_err() {
                return;
            }
            wake();
            std::thread::sleep(Duration::from_secs(1));
        })
        .expect("spawn overlay pipe client");
}

/// Runs a base tick (or an animation frame) unless the app is already
/// borrowed (a re-entrant message during ShowWindow / SetWindowPos).
fn on_tick(hwnd: HWND, animation_frame: bool) {
    let close = APP.with(|slot| {
        let Ok(mut slot) = slot.try_borrow_mut() else { return false };
        let Some(app) = slot.as_mut() else { return false };
        let (close, changed) = if animation_frame { (false, app.animate(hwnd)) } else { app.tick(hwnd) };
        if changed {
            write_diagnostics(app, hwnd);
        }
        close
    });
    if close {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
}

extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            // Pixels come from UpdateLayeredWindow; just validate.
            unsafe {
                let mut paint = PAINTSTRUCT::default();
                let _ = BeginPaint(hwnd, &mut paint);
                let _ = EndPaint(hwnd, &paint);
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == BASE_TIMER => {
            on_tick(hwnd, false);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == ANIM_TIMER => {
            on_tick(hwnd, true);
            LRESULT(0)
        }
        WM_FEED => {
            on_tick(hwnd, false);
            LRESULT(0)
        }
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let app = APP.with(|slot| slot.try_borrow_mut().ok().and_then(|mut slot| slot.take()));
            if let Some(handle) = app.as_ref().and_then(|app| app.parent_handle) {
                unsafe {
                    let _ = CloseHandle(handle);
                }
            }
            drop(app);
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
