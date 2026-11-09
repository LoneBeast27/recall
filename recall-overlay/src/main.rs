#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! recall-overlay: the Recall Island. A click-through, no-activate,
//! capture-excluded topmost pill at each game's free top spot that flashes
//! notices and, while the stats toggle is on, shows FPS / GPU / CPU.

mod anim;
mod app;
mod badge;
mod cue;
mod diagnostics;
mod durable;
mod dynamic;
mod export;
mod export_sim;
mod filmstrip;
mod fit_width;
mod fonts;
mod guide;
mod guide_render;
mod guide_window;
mod hint;
mod island;
mod layout;
mod notice;
mod pipe_name;
mod pipe_owner;
mod placement;
mod png;
mod presence;
mod preview;
mod rec_notice;
mod reducer;
mod render;
mod session;
#[path = "../../recall/src/single_instance.rs"]
mod single_instance;
mod smoke;
mod spot_edit;
mod stack;
mod stack_lines;
mod stats;
mod stats_gate;
mod trace;
mod window;
mod zorder;

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
    SetWindowDisplayAffinity, TranslateMessage, HTCLIENT, HTTRANSPARENT, MSG, WDA_EXCLUDEFROMCAPTURE, WM_APP, WM_CLOSE,
    WM_CAPTURECHANGED, WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCHITTEST, WM_PAINT,
    WM_RBUTTONUP, WM_TIMER,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};

use app::{App, Feed, ANIM_TIMER, BASE_MS, BASE_TIMER};
use diagnostics::write_diagnostics;
use fonts::Fonts;
use reducer::Reducer;
use render::Renderer;
use smoke::Smoke;
use window::create_window;

/// How long a new island waits for one that is ending (the controller restarts it).
const INSTANCE_WAIT: Duration = Duration::from_secs(3);
/// Posted by the pipe thread so a notice lands at once, not on the next tick.
const WM_FEED: u32 = WM_APP + 1;
const CORNERS: [&str; 5] = ["dynamic", "top-left", "top-right", "bottom-left", "bottom-right"];
const USAGE: &str = "usage: recall-overlay --parent PID [--corner dynamic|top-left|top-right|bottom-left|bottom-right] \
[--diagnostics PATH] | --smoke-state FIXTURE.json [--smoke-allow-capture] [--smoke-render PATH.bmp] [--diagnostics PATH] \
| --preview-sheet PATH.bmp [--preview-scale S] | --preview-notice PATH.bmp [--preview-scale S] \
| --export-frames DIR | --frame-timing-probe PID [--probe-seconds N]";

#[derive(Debug, Default)]
struct Options {
    /// `dynamic` (a free spot from the game's HUD) or a fixed corner.
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
    // One island per user. Smoke runs are tests and never take the guard. Held until exit.
    let _instance = match options.smoke_state {
        Some(_) => None,
        None => match single_instance::claim_within(&single_instance::mutex_name(single_instance::OVERLAY), INSTANCE_WAIT) {
            single_instance::Claim::First(guard) => Some(guard),
            single_instance::Claim::Taken => {
                eprintln!("recall-overlay: another island is already running; exiting");
                return Ok(());
            }
            single_instance::Claim::Unavailable(code) => {
                eprintln!("recall-overlay: single-instance guard unavailable (error {code}); continuing");
                None
            }
        },
    };
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
    if options.smoke_state.is_none() {
        app.load_hud_cache(dynamic::HudCache::default_path());
        app.trace = trace::Trace::open(trace::Trace::default_path(), trace::LIMIT);
        let line = format!(
            "start: pid {} parent {:?} corner {} capture exclusion {}",
            std::process::id(),
            options.parent,
            options.corner,
            capture_exclusion_result
        );
        app.trace.event(&line);
    }
    app.diagnostics_path = options.diagnostics.clone();
    app.capture_exclusion_requested = !options.smoke_allow_capture;
    app.capture_exclusion_result = capture_exclusion_result;
    match options.smoke_state.as_ref() {
        Some(path) => {
            let mut smoke = Smoke::load(path)?;
            smoke.apply_due(&mut app.reducer, Duration::ZERO, Instant::now());
            app.smoke = Some(smoke);
        }
        None => spawn_pipe_client(tx, hwnd.0 as isize, app.out.clone()),
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
    let mut options = Options { corner: "dynamic".into(), ..Options::default() };
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

fn spawn_pipe_client(tx: Sender<Feed>, hwnd: isize, out: app::PipeOut) {
    let wake = move || unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_FEED, WPARAM(0), LPARAM(0));
    };
    std::thread::Builder::new()
        .name("recall-overlay-pipe".into())
        .spawn(move || {
            // Per Windows session: this user's recorder, never another signed-in account's.
            let pipe = pipe_name::current_pipe_name();
            let mut refused: Option<String> = None;
            loop {
                let Ok(name) = pipe.as_str().to_ns_name::<GenericNamespaced>() else { return };
                // Only a server of this account (`pipe_owner`): a refused one gets nothing.
                let stream = Stream::connect(name).ok().filter(|stream| match pipe_owner::check(stream) {
                    Ok(()) => true,
                    Err(reason) => {
                        if refused.as_deref() != Some(reason.as_str()) {
                            eprintln!("recall-overlay: {reason}; not using the control pipe");
                        }
                        refused = Some(reason);
                        false
                    }
                });
                if let Some(stream) = stream {
                    refused = None;
                    let (reader, mut writer) = stream.split();
                    let _ = tx.send(Feed::Connected);
                    let _ = writer.write_all(b"{\"cmd\":\"status\"}\n");
                    if let Ok(mut slot) = out.lock() {
                        *slot = Some(Box::new(writer));
                    }
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
                if let Ok(mut slot) = out.lock() {
                    *slot = None;
                }
                if tx.send(Feed::Disconnected).is_err() {
                    return;
                }
                wake();
                std::thread::sleep(Duration::from_secs(1));
            }
        })
        .expect("spawn overlay pipe client");
}

/// Runs `f` on the app unless it is already borrowed (a re-entrant message).
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|slot| slot.try_borrow_mut().ok().and_then(|mut slot| slot.as_mut().map(f)))
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

/// `WM_MOUSEMOVE`'s wParam bit: the left button is down (WinUser.h).
const MK_LBUTTON: usize = 0x0001;

extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        // Click-through, except while the island editor runs (the pill takes the mouse).
        WM_NCHITTEST => LRESULT(if with_app(|app| app.editing()) == Some(true) { HTCLIENT as isize } else { HTTRANSPARENT as isize }),
        // The island editor is mouse only (no keys held): drag, release saves,
        // right-click cancels, the pill's ⟲ end resets.
        WM_LBUTTONDOWN => {
            match with_app(|app| app.editor_press(hwnd, Instant::now())) {
                Some(true) => unsafe {
                    SetCapture(hwnd);
                },
                Some(false) => on_tick(hwnd, false),
                None => {}
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            // No MK_LBUTTON: the release never reached the window (a stuck drag).
            let held = wparam.0 & MK_LBUTTON != 0;
            with_app(|app| app.editor_drag(hwnd, held, Instant::now()));
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            with_app(|app| app.editor_release(Instant::now()));
            unsafe {
                let _ = ReleaseCapture();
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            if with_app(|app| app.editor_cancel(hwnd)) == Some(true) {
                on_tick(hwnd, false);
            }
            LRESULT(0)
        }
        // Another window took the mouse mid-drag: the editor ends, nothing more saved.
        WM_CAPTURECHANGED => {
            if HWND(lparam.0 as *mut _) != hwnd && with_app(|app| app.editor_capture_lost(hwnd)) == Some(true) {
                on_tick(hwnd, false);
            }
            LRESULT(0)
        }
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
