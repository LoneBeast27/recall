#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod diagnostics;
mod draw;
mod reducer;
mod window;

use diagnostics::write_diagnostics;
use draw::{draw_overlay, load_fonts, render_smoke_bmp};
use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{GenericNamespaced, Stream, ToNsName};
use reducer::Reducer;
use serde_json::Value;
use std::cell::RefCell;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};
use window::{create_window, window_bounds};
use windows::Win32::Foundation::{
    CloseHandle, HANDLE, HWND, LPARAM, LRESULT, RECT, WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateRoundRectRgn, EndPaint, InvalidateRect, SetWindowRgn, FW_NORMAL,
    PAINTSTRUCT,
};
use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, GetWindowRect,
    PostQuitMessage, IsWindowVisible, SetTimer, SetWindowDisplayAffinity, SetWindowPos,
    ShowWindow, TranslateMessage, CREATESTRUCTW, HTTRANSPARENT, HWND_TOPMOST, MSG,
    SWP_NOACTIVATE, SW_HIDE, SW_SHOWNOACTIVATE, WDA_EXCLUDEFROMCAPTURE, WM_CLOSE, WM_DESTROY,
    WM_ERASEBKGND, WM_NCCREATE, WM_NCHITTEST, WM_PAINT, WM_TIMER,
};

const PIPE_NAME: &str = "recall-hdr-control";
const TIMER_ID: usize = 1;
const TICK_MS: u32 = 250;
const WIDTH_DIP: i32 = 260;
const HEIGHT_DIP: i32 = 68;
const MARGIN_DIP: i32 = 16;

#[derive(Clone, Copy, Debug)]
enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "top-left" => Ok(Self::TopLeft),
            "top-right" => Ok(Self::TopRight),
            "bottom-left" => Ok(Self::BottomLeft),
            "bottom-right" => Ok(Self::BottomRight),
            _ => Err(format!("invalid corner: {value}")),
        }
    }
}

#[derive(Debug)]
struct Options {
    corner: Corner,
    parent: Option<u32>,
    diagnostics: Option<PathBuf>,
    smoke_state: Option<PathBuf>,
    smoke_allow_capture: bool,
    smoke_render: Option<PathBuf>,
}

#[derive(Debug)]
enum Feed {
    Connected,
    Disconnected,
    Json(Value),
}

struct AppState {
    reducer: Reducer,
    rx: Receiver<Feed>,
    parent_handle: Option<HANDLE>,
    diagnostics_path: Option<PathBuf>,
    capture_exclusion_result: String,
    capture_exclusion_requested: bool,
    parent_pid: Option<u32>,
    smoke_mode: bool,
    selected_title_font: String,
    selected_body_font: String,
}

thread_local! {
    static APP: RefCell<Option<AppState>> = const { RefCell::new(None) };
}

fn main() {
    if let Err(error) = run() {
        eprintln!("recall-overlay: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    load_fonts();
    let options = parse_args()?;
    if options.parent.is_none() && options.smoke_state.is_none() {
        return Err("--parent PID is required outside --smoke-state test mode".into());
    }
    if options.smoke_allow_capture && options.smoke_state.is_none() {
        return Err("--smoke-allow-capture requires --smoke-state".into());
    }
    if options.smoke_render.is_some() && options.smoke_state.is_none() {
        return Err("--smoke-render requires --smoke-state".into());
    }

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let parent_handle = match options.parent {
        Some(pid) => Some(
            unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }
                .map_err(|e| format!("open parent process {pid}: {e}"))?,
        ),
        None => None,
    };

    let (tx, rx) = mpsc::channel();
    let (x, y, width, height) = window_bounds(options.corner)?;
    let hwnd = create_window(x, y, width, height)?;
    let capture_exclusion_requested = !options.smoke_allow_capture;
    let capture_exclusion_result = if options.smoke_allow_capture {
        "disabled_for_smoke_capture".to_string()
    } else {
        match unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) } {
            Ok(()) => "enabled".to_string(),
            Err(error) => format!("unsupported_or_failed: {error}"),
        }
    };

    let mut reducer = Reducer::new();
    if let Some(path) = options.smoke_state.as_ref() {
        apply_smoke_fixture(&mut reducer, path)?;
    } else {
        spawn_pipe_client(tx.clone());
    }
    if let Some(path) = options.smoke_render.as_ref() {
        render_smoke_bmp(path, reducer.view())?;
    }

    APP.with(|slot| {
        *slot.borrow_mut() = Some(AppState {
            reducer,
            rx,
            parent_handle,
            diagnostics_path: options.diagnostics,
            capture_exclusion_result,
            capture_exclusion_requested,
            parent_pid: options.parent,
            smoke_mode: options.smoke_state.is_some(),
            selected_title_font: "unmeasured".into(),
            selected_body_font: "unmeasured".into(),
        });
    });

    unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE,
        )
        .map_err(|e| format!("position overlay: {e}"))?;
        // Rounded card to match the app's 10 px panels.
        let radius = 10 * GetDpiForWindow(hwnd).max(96) as i32 / 96;
        let region = CreateRoundRectRgn(0, 0, width + 1, height + 1, radius * 2, radius * 2);
        let _ = SetWindowRgn(hwnd, Some(region), false);
        SetTimer(Some(hwnd), TIMER_ID, TICK_MS, None);
    }
    sync_visibility(hwnd);
    write_diagnostics(hwnd);

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
    let mut options = Options {
        corner: Corner::TopRight,
        parent: None,
        diagnostics: None,
        smoke_state: None,
        smoke_allow_capture: false,
        smoke_render: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--corner" => {
                let value = args.next().ok_or("--corner requires a value")?;
                options.corner = Corner::parse(&value)?;
            }
            "--parent" => {
                let value = args.next().ok_or("--parent requires a PID")?;
                let pid = value
                    .parse::<u32>()
                    .map_err(|_| format!("invalid parent PID: {value}"))?;
                if pid == 0 {
                    return Err("parent PID must be positive".into());
                }
                options.parent = Some(pid);
            }
            "--diagnostics" => {
                options.diagnostics = Some(PathBuf::from(
                    args.next().ok_or("--diagnostics requires a JSON path")?,
                ));
            }
            "--smoke-state" => {
                options.smoke_state = Some(PathBuf::from(
                    args.next()
                        .ok_or("--smoke-state requires a JSON fixture path")?,
                ));
            }
            "--smoke-allow-capture" => options.smoke_allow_capture = true,
            "--smoke-render" => {
                options.smoke_render = Some(PathBuf::from(
                    args.next().ok_or("--smoke-render requires a BMP path")?,
                ));
            }
            "--help" | "-h" => {
                return Err("usage: recall-overlay [--corner top-left|top-right|bottom-left|bottom-right] --parent PID [--diagnostics PATH] [--smoke-state PATH --smoke-allow-capture --smoke-render PATH.bmp]".into());
            }
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }
    Ok(options)
}

fn apply_smoke_fixture(reducer: &mut Reducer, path: &PathBuf) -> Result<(), String> {
    let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let now = Instant::now();
    if let Some(events) = value
        .as_array()
        .or_else(|| value.get("events").and_then(Value::as_array))
    {
        for event in events {
            if event.get("mod").is_some() {
                reducer.apply_envelope(event, now);
            } else {
                reducer.apply_engine_event(event, now);
            }
        }
    } else if value.get("mod").is_some() {
        reducer.apply_envelope(&value, now);
    } else {
        reducer.apply_engine_event(&value, now);
    }
    Ok(())
}

fn spawn_pipe_client(tx: Sender<Feed>) {
    std::thread::Builder::new()
        .name("recall-overlay-pipe".into())
        .spawn(move || loop {
            let name = match PIPE_NAME.to_ns_name::<GenericNamespaced>() {
                Ok(name) => name,
                Err(_) => return,
            };
            match Stream::connect(name) {
                Ok(stream) => {
                    let (reader, mut writer) = stream.split();
                    let _ = tx.send(Feed::Connected);
                    let _ = writer.write_all(b"{\"cmd\":\"status\"}\n");
                    let lines = BufReader::new(reader).lines();
                    for line in lines {
                        let Ok(line) = line else { break };
                        if let Ok(value) = serde_json::from_str::<Value>(line.trim()) {
                            if tx.send(Feed::Json(value)).is_err() {
                                return;
                            }
                        }
                    }
                }
                Err(_) => {}
            }
            if tx.send(Feed::Disconnected).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
        })
        .expect("spawn overlay pipe client");
}

extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCCREATE => {
            let _ = lparam.0 as *const CREATESTRUCTW;
            LRESULT(1)
        }
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_ID => {
            tick(hwnd);
            LRESULT(0)
        }
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            APP.with(|slot| {
                if let Some(state) = slot.borrow_mut().take() {
                    if let Some(handle) = state.parent_handle {
                        unsafe {
                            let _ = CloseHandle(handle);
                        }
                    }
                }
            });
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn tick(hwnd: HWND) {
    let now = Instant::now();
    let mut close = false;
    let mut changed = false;
    APP.with(|slot| {
        let mut app = slot.borrow_mut();
        let Some(app) = app.as_mut() else { return };
        if let Some(handle) = app.parent_handle {
            close = unsafe { WaitForSingleObject(handle, 0) == WAIT_OBJECT_0 };
        }
        while let Ok(feed) = app.rx.try_recv() {
            changed |= match feed {
                Feed::Connected => app.reducer.connected(),
                Feed::Disconnected => app.reducer.disconnected(),
                Feed::Json(value) => app.reducer.apply_envelope(&value, now),
            };
        }
        if !app.smoke_mode {
            changed |= app.reducer.tick(now);
        }
    });
    if close {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        return;
    }
    if changed {
        sync_visibility(hwnd);
        if unsafe { IsWindowVisible(hwnd).as_bool() } {
            unsafe { let _ = InvalidateRect(Some(hwnd), None, false); }
        }
        write_diagnostics(hwnd);
    }
}

fn sync_visibility(hwnd: HWND) {
    let visible = APP.with(|slot| slot.borrow().as_ref().is_some_and(|app| app.reducer.view().visible));
    if unsafe { IsWindowVisible(hwnd).as_bool() } != visible {
        unsafe { let _ = ShowWindow(hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE }); }
    }
}

fn paint(hwnd: HWND) {
    let view = APP.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|state| state.reducer.view().clone())
    });
    let Some(view) = view else { return };
    unsafe {
        let mut paint = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut paint);
        let dpi = GetDpiForWindow(hwnd).max(96) as i32;
        let mut bounds = RECT::default();
        let _ = GetWindowRect(hwnd, &mut bounds);
        let width = bounds.right - bounds.left;
        let height = bounds.bottom - bounds.top;
        let (title_face, body_face) = draw_overlay(hdc, width, height, dpi, &view);
        let _ = EndPaint(hwnd, &paint);
        APP.with(|slot| {
            if let Some(app) = slot.borrow_mut().as_mut() {
                app.selected_title_font = title_face;
                app.selected_body_font = body_face;
            }
        });
        write_diagnostics(hwnd);
    }
}
