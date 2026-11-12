//! Starting a capture: monitor (Desktop Duplication when opted in, else WGC) or a game
//! window (monitor Desktop Duplication when it covers its monitor and DDA is opted in, else
//! WGC window capture), with the capture-rate cap.

use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

use windows_capture::capture::{GraphicsCaptureApiError, GraphicsCaptureApiHandler};
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings, MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

use super::{dda, dda_window, CaptureControlHandle, CaptureError, CaptureShared, Handler, WgcControl};

/// Start capturing the primary monitor on its own thread. The returned
/// `CaptureControl` keeps the capture alive — drop or `.stop()` it to end.
pub fn start_monitor(
    index: Option<usize>,
    shared: Arc<CaptureShared>,
) -> Result<CaptureControlHandle, CaptureError> {
    let monitor = match index {
        Some(i) => Monitor::from_index(i)?,
        None => Monitor::primary()?,
    };
    let format = shared.start_format();
    // Desktop Duplication (opt-in, `RECALL_CAPTURE=dda`): no yellow border and no Borderless
    // permission needed. Any setup failure falls back to WGC.
    match shared.dda_wanted() {
        Ok(()) => {
            let hmonitor = windows::Win32::Graphics::Gdi::HMONITOR(monitor.as_raw_hmonitor());
            match dda::start(hmonitor, shared.clone(), capture_interval(recording_fps()), None) {
                Ok(control) => return Ok(CaptureControlHandle::Dda(control)),
                Err(reason) => eprintln!("[recall] capture: wgc (desktop duplication unavailable: {reason})"),
            }
        }
        Err(reason) => eprintln!("[recall] capture: wgc ({reason})"),
    }
    // Ask for borderless access first so the session's IsBorderRequired(false)
    // (set by windows-capture for our WithoutBorder setting) actually suppresses
    // the Windows capture border.
    crate::border::request_borderless();
    let start = |interval| {
        Handler::start_free_threaded(Settings::new(
            monitor,
            CursorCaptureSettings::WithoutCursor,
            DrawBorderSettings::WithoutBorder,
            SecondaryWindowSettings::Default,
            interval,
            DirtyRegionSettings::Default,
            format,
            shared.clone(),
        ))
    };
    with_rate_cap(&shared, start).map(CaptureControlHandle::Wgc)
}

/// Frames per second the capture is capped at: the recording rate plus 10% headroom. Every
/// frame let in is encoded with its own time (variable rate), so a clip's average rate can
/// sit up to 10% over the recording rate (`clip_rate`). Uncapped, Windows hands over every frame the game
/// presents (500+ a second in VALORANT) and each is read back to the CPU (29.5 MB at 1440p
/// HDR), which costs the game frames.
pub(crate) fn capture_interval(recording_fps: u32) -> std::time::Duration {
    std::time::Duration::from_secs_f64(1.0 / (f64::from(recording_fps.clamp(1, 1000)) * 1.1))
}

/// `RECALL_FPS` as the encoder reads it (1..=1000, default 60).
fn recording_fps() -> u32 {
    std::env::var("RECALL_FPS").ok().and_then(|v| v.trim().parse().ok()).filter(|f| (1..=1000).contains(f)).unwrap_or(60)
}

/// Starts with Windows' rate cap where Windows has it (`MinUpdateInterval`, 11 24H2 and
/// later). Older builds hand over every frame the game presents: there the handler drops the
/// frames over the rate itself (`soft_cap_ns`) before any GPU work. A start error is returned
/// as it is; it never turns the cap off.
fn with_rate_cap<S>(shared: &CaptureShared, start: S) -> Result<WgcControl, CaptureError>
where
    S: Fn(MinimumUpdateIntervalSettings) -> Result<WgcControl, GraphicsCaptureApiError<CaptureError>>,
{
    let interval = capture_interval(recording_fps());
    let fps = 1.0 / interval.as_secs_f64();
    // Checks only: `RECALL_CAPTURE_WINDOWS_CAP=0` acts like Windows before 11 24H2,
    // `RECALL_CAPTURE_SOFT_CAP=0` like Recall before the handler kept the rate itself.
    let forced_old = std::env::var("RECALL_CAPTURE_WINDOWS_CAP").is_ok_and(|v| v.trim() == "0");
    let soft_cap = std::env::var("RECALL_CAPTURE_SOFT_CAP").map_or(true, |v| v.trim() != "0");
    if !forced_old && windows_has_rate_cap() {
        shared.soft_cap_ns.store(0, Relaxed);
        let control = start(MinimumUpdateIntervalSettings::Custom(interval))
            .map_err(|e| box_start_err_with(e, "capped start (Windows reports MinUpdateInterval)"))?;
        eprintln!("[recall] capture capped at {fps:.0} fps");
        return Ok(control);
    }
    let nanos = if soft_cap { u64::try_from(interval.as_nanos()).unwrap_or(u64::MAX) } else { 0 };
    shared.soft_cap_ns.store(nanos, Relaxed);
    let control = start(MinimumUpdateIntervalSettings::Default).map_err(box_start_err)?;
    if soft_cap {
        eprintln!("[recall] capture: this Windows has no rate cap; Recall keeps {fps:.0} fps itself");
    } else {
        eprintln!("[recall] capture: this Windows has no rate cap; capturing every frame");
    }
    Ok(control)
}

/// Whether this Windows has `GraphicsCaptureSession.MinUpdateInterval` (11 24H2 and later),
/// asked once.
pub(crate) fn windows_has_rate_cap() -> bool {
    static HAS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HAS.get_or_init(|| {
        use windows::core::h;
        windows::Foundation::Metadata::ApiInformation::IsPropertyPresent(
            h!("Windows.Graphics.Capture.GraphicsCaptureSession"),
            h!("MinUpdateInterval"),
        )
        .unwrap_or_else(|e| {
            eprintln!("[recall] capture: could not ask Windows for the rate cap ({e}); Recall keeps the rate itself");
            false
        })
    })
}

/// Flatten the capture-start error, naming which start failed.
fn box_start_err_with(e: GraphicsCaptureApiError<CaptureError>, which: &str) -> CaptureError {
    Box::new(std::io::Error::other(format!("{which}: {e}")))
}

/// Flatten the capture-start error into our boxed error type.
fn box_start_err(e: GraphicsCaptureApiError<CaptureError>) -> CaptureError {
    Box::new(std::io::Error::other(e.to_string()))
}

/// Whether `hwnd` is still a window, owned by the process that owned it at the first check:
/// a closed game's handle is not a window, and a recycled one belongs to another process.
/// (The owner is the window's own process: for a Store game that is the frame host, not the
/// hosted game process the controller names for audio.)
pub fn window_exists(hwnd: usize) -> bool {
    static FIRST_OWNER: std::sync::Mutex<Option<(usize, u32)>> = std::sync::Mutex::new(None);
    let Some(owner) = window_owner(hwnd) else { return false };
    let Ok(mut first) = FIRST_OWNER.lock() else { return true };
    owner_matches(&mut first, hwnd, owner)
}

/// Remembers `owner` for `hwnd` on its first check; later checks must match it.
pub(crate) fn owner_matches(first: &mut Option<(usize, u32)>, hwnd: usize, owner: u32) -> bool {
    match *first {
        Some((h, pid)) if h == hwnd => pid == owner,
        _ => {
            *first = Some((hwnd, owner));
            true
        }
    }
}

/// The process owning `hwnd`, when it is a window.
pub(crate) fn window_owner(hwnd: usize) -> Option<u32> {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, IsWindow};
    let handle = windows::Win32::Foundation::HWND(hwnd as *mut std::ffi::c_void);
    if hwnd == 0 || !unsafe { IsWindow(Some(handle)) }.as_bool() {
        return None;
    }
    let mut owner = 0u32;
    unsafe { GetWindowThreadProcessId(handle, Some(&mut owner)) };
    Some(owner)
}

/// Capture an explicitly selected game window through the supported WGC API.
pub fn start_window(hwnd: usize, shared: Arc<CaptureShared>) -> Result<CaptureControlHandle, CaptureError> {
    if hwnd == 0 { return Err(std::io::Error::other("capture window handle is null").into()); }
    let format = shared.start_format();
    // A game window covering its whole monitor shows what the monitor shows: record it with
    // monitor Desktop Duplication (no border) while it keeps covering it (`dda_window`).
    if shared.dda_wanted().is_ok() {
        match dda_window::covering_monitor(hwnd) {
            Some(monitor) => {
                shared.window.store(0, Relaxed); // the whole monitor is the game: no crop
                match dda::start(monitor, shared.clone(), capture_interval(recording_fps()), Some(hwnd)) {
                    Ok(control) => return Ok(CaptureControlHandle::Dda(control)),
                    Err(reason) => eprintln!("[recall] capture: wgc window (desktop duplication unavailable: {reason})"),
                }
            }
            None => eprintln!("[recall] capture: wgc window (the game window does not cover its monitor)"),
        }
    }
    shared.window.store(hwnd, Relaxed);
    crate::border::request_borderless();
    let window = windows_capture::window::Window::from_raw_hwnd(hwnd as *mut std::ffi::c_void);
    let start = |interval| {
        Handler::start_free_threaded(Settings::new(
            window, CursorCaptureSettings::WithoutCursor, DrawBorderSettings::WithoutBorder,
            SecondaryWindowSettings::Default, interval,
            DirtyRegionSettings::Default, format, shared.clone(),
        ))
    };
    with_rate_cap(&shared, start).map(CaptureControlHandle::Wgc)
}
