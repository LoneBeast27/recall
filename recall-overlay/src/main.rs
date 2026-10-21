#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod reducer;

use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{GenericNamespaced, Stream, ToNsName};
use reducer::{OverlayView, RecorderState, Reducer};
use serde::Serialize;
use serde_json::Value;
use std::cell::RefCell;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};
use windows::core::w;
use windows::Win32::Foundation::{
    CloseHandle, COLORREF, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WAIT_OBJECT_0,
    WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    AddFontMemResourceEx, CreateRoundRectRgn, FrameRgn, SetWindowRgn, FW_MEDIUM,
    BeginPaint, CreateCompatibleDC, CreateDIBSection, CreateFontW, CreateSolidBrush, DeleteDC,
    DeleteObject, DrawTextW, Ellipse, EndPaint, FillRect, GetMonitorInfoW, GetTextFaceW,
    InvalidateRect, MonitorFromPoint, SelectObject, SetBkMode, SetTextColor, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET,
    DEFAULT_PITCH, DIB_RGB_COLORS, DT_END_ELLIPSIS, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER,
    FW_NORMAL, FW_SEMIBOLD, HDC, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
    OUT_DEFAULT_PRECIS, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
use windows::Win32::UI::HiDpi::{
    GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, GetWindowRect, LoadCursorW, PostQuitMessage, RegisterClassW,
    IsWindowVisible, SetLayeredWindowAttributes, SetTimer, SetWindowDisplayAffinity, SetWindowPos, ShowWindow,
    TranslateMessage, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, GWL_EXSTYLE, HTTRANSPARENT,
    HWND_TOPMOST, IDC_ARROW, LWA_ALPHA, MSG, SWP_NOACTIVATE, SW_HIDE, SW_SHOWNOACTIVATE,
    WDA_EXCLUDEFROMCAPTURE, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_NCCREATE, WM_NCHITTEST,
    WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
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

fn window_bounds(corner: Corner) -> Result<(i32, i32, i32, i32), String> {
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

fn create_window(x: i32, y: i32, width: i32, height: i32) -> Result<HWND, String> {
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

unsafe fn draw_overlay(
    hdc: HDC,
    width: i32,
    height: i32,
    dpi: i32,
    view: &OverlayView,
) -> (String, String) {
    // Recall design system: raised card #121212, #282828 hairline, Geist type,
    // safety-colour dot, tertiary detail line. Two lines: what happened + detail.
    let scale = |dip: i32| dip * dpi / 96;
    let card = CreateSolidBrush(rgb(0x12, 0x12, 0x12));
    let edge = CreateSolidBrush(rgb(0x28, 0x28, 0x28));
    FillRect(hdc, &RECT { left: 0, top: 0, right: width, bottom: height }, card);
    let radius = scale(10);
    let outline = CreateRoundRectRgn(0, 0, width + 1, height + 1, radius * 2, radius * 2);
    let _ = FrameRgn(hdc, outline, edge, 1, 1);
    let dot_color = match view.state {
        RecorderState::Ready => rgb(0x81, 0xc9, 0x95),
        RecorderState::Starting => rgb(0xfd, 0xd6, 0x63),
        RecorderState::Failed => rgb(0xf2, 0x8b, 0x82),
        RecorderState::Stopped => rgb(0xa3, 0xa3, 0xa3),
    };
    let dot = CreateSolidBrush(dot_color);
    let old_brush = SelectObject(hdc, HGDIOBJ(dot.0));
    let mid = height / 2;
    let _ = Ellipse(hdc, scale(15), mid - scale(13), scale(22), mid - scale(6));
    SelectObject(hdc, old_brush);
    SetBkMode(hdc, TRANSPARENT);
    let font = |px: i32, weight: i32| {
        CreateFontW(-scale(px), 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY, DEFAULT_PITCH.0 as u32, w!("Geist"))
    };
    let title_font = font(14, FW_SEMIBOLD.0 as i32);
    let body_font = font(12, FW_MEDIUM.0 as i32);
    let original_font = SelectObject(hdc, HGDIOBJ(title_font.0));
    let title_face = selected_font_face(hdc);
    let left = scale(30);
    draw_text(hdc, &view.primary, RECT { left, top: mid - scale(20), right: width - scale(14), bottom: mid + scale(1) }, rgb(0xed, 0xed, 0xed));
    SelectObject(hdc, HGDIOBJ(body_font.0));
    let body_face = selected_font_face(hdc);
    draw_text(hdc, &view.secondary, RECT { left, top: mid + scale(1), right: width - scale(14), bottom: mid + scale(20) }, rgb(0x8f, 0x8f, 0x8f));
    SelectObject(hdc, original_font);
    for object in [HGDIOBJ(card.0), HGDIOBJ(edge.0), HGDIOBJ(dot.0), HGDIOBJ(title_font.0), HGDIOBJ(body_font.0), HGDIOBJ(outline.0)] {
        let _ = DeleteObject(object);
    }
    (title_face, body_face)
}

/// Loads Geist (OFL, shared with the app) as process-private fonts for GDI.
fn load_fonts() {
    for bytes in [
        include_bytes!("../../recall-native/assets/fonts/geist-500.ttf").as_slice(),
        include_bytes!("../../recall-native/assets/fonts/geist-600.ttf").as_slice(),
    ] {
        let mut installed = 0u32;
        unsafe { AddFontMemResourceEx(bytes.as_ptr().cast(), bytes.len() as u32, None, &mut installed) };
    }
}

unsafe fn selected_font_face(hdc: HDC) -> String {
    let length = GetTextFaceW(hdc, None);
    if length <= 0 {
        return "unknown".into();
    }
    let mut name = vec![0u16; length as usize];
    let copied = GetTextFaceW(hdc, Some(&mut name));
    if copied <= 0 {
        return "unknown".into();
    }
    let used = name
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(copied as usize);
    String::from_utf16_lossy(&name[..used])
}

fn render_smoke_bmp(path: &PathBuf, view: &OverlayView) -> Result<(), String> {
    unsafe {
        let dpi = GetDpiForSystem().max(96) as i32;
        let width = WIDTH_DIP * dpi / 96;
        let height = HEIGHT_DIP * dpi / 96;
        let memory_dc = CreateCompatibleDC(None);
        if memory_dc.0.is_null() {
            return Err("CreateCompatibleDC failed for smoke render".into());
        }
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: (width * height * 4) as u32,
                biXPelsPerMeter: dpi * 10_000 / 254,
                biYPelsPerMeter: dpi * 10_000 / 254,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(
            Some(memory_dc),
            &mut info,
            DIB_RGB_COLORS,
            &mut bits,
            None,
            0,
        )
        .map_err(|e| format!("CreateDIBSection for smoke render: {e}"))?;
        let original_bitmap = SelectObject(memory_dc, HGDIOBJ(bitmap.0));
        let _ = draw_overlay(memory_dc, width, height, dpi, view);

        let pixels = std::slice::from_raw_parts(bits.cast::<u8>(), (width * height * 4) as usize);
        let row_bytes = width as usize * 3;
        let row_stride = (row_bytes + 3) & !3;
        let image_bytes = row_stride * height as usize;
        let mut bmp = Vec::with_capacity(54 + image_bytes);
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&(54u32 + image_bytes as u32).to_le_bytes());
        bmp.extend_from_slice(&[0; 4]);
        bmp.extend_from_slice(&54u32.to_le_bytes());
        bmp.extend_from_slice(&40u32.to_le_bytes());
        bmp.extend_from_slice(&width.to_le_bytes());
        bmp.extend_from_slice(&height.to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&24u16.to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes());
        bmp.extend_from_slice(&(image_bytes as u32).to_le_bytes());
        bmp.extend_from_slice(&(dpi * 10_000 / 254).to_le_bytes());
        bmp.extend_from_slice(&(dpi * 10_000 / 254).to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes());
        for y in (0..height as usize).rev() {
            let source = &pixels[y * width as usize * 4..(y + 1) * width as usize * 4];
            for pixel in source.chunks_exact(4) {
                bmp.extend_from_slice(&pixel[..3]);
            }
            bmp.resize(bmp.len() + (row_stride - row_bytes), 0);
        }
        fs::write(path, bmp).map_err(|e| format!("write smoke render {}: {e}", path.display()))?;
        SelectObject(memory_dc, original_bitmap);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(memory_dc);
        Ok(())
    }
}

unsafe fn draw_text(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    text: &str,
    mut rect: RECT,
    color: COLORREF,
) {
    SetTextColor(hdc, color);
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    DrawTextW(
        hdc,
        &mut wide,
        &mut rect,
        DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
    );
}

fn write_diagnostics(hwnd: HWND) {
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

const fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    COLORREF(red as u32 | ((green as u32) << 8) | ((blue as u32) << 16))
}
