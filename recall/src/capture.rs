//! Windows.Graphics.Capture pipeline for Recall.
//!
//! Captures the primary monitor in `Rgba16F` (scRGB-linear; carries HDR) on a
//! dedicated thread, reads each frame back to CPU, and parks the latest frame
//! in a shared slot for the render thread to upload + tonemap. CPU readback is
//! the simple/debuggable Phase-0 path; zero-copy D3D11->D3D12 interop is the
//! deferred optimization.

use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::{Arc, Mutex};

use crate::protocol::MonitorInfo;

use windows_capture::capture::{
    CaptureControl, Context, GraphicsCaptureApiError, GraphicsCaptureApiHandler,
};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

/// Tonemap mode shared with the shader's `Params.mode`.
pub const MODE_PASSTHROUGH: u32 = 0;
pub const MODE_TONEMAP: u32 = 1;

/// A captured frame, tightly packed f16 RGBA (8 bytes/pixel), ready to upload.
pub struct FrameData {
    /// Native WGC timestamp, rebased to the first capture, in microseconds.
    pub pts_us: u64,
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

/// Hand-off point between the capture thread and the render thread.
pub struct CaptureShared {
    /// Most recent frame; render thread `take()`s it. None until first frame.
    pub latest: Mutex<Option<FrameData>>,
    timeline: Mutex<Option<(u64, std::time::Instant)>>,
    /// Effective tonemap mode (MODE_PASSTHROUGH / MODE_TONEMAP).
    pub mode: AtomicU32,
    /// When true, HDR auto-detect drives `mode`. When false, `mode` is forced.
    pub auto: AtomicBool,
    /// Set by a `quit` command to ask the event loop to exit.
    pub quit: AtomicBool,
}

impl CaptureShared {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            latest: Mutex::new(None),
            timeline: Mutex::new(None),
            mode: AtomicU32::new(MODE_PASSTHROUGH),
            auto: AtomicBool::new(true),
            quit: AtomicBool::new(false),
        })
    }

    /// Current capture timeline, including time since the latest delivered frame.
    pub fn recording_now_us(&self) -> Option<u64> {
        self.timeline
            .lock()
            .ok()
            .and_then(|t| t.map(|(pts, at)| pts.saturating_add(at.elapsed().as_micros() as u64)))
    }

    /// Mode selector label for status: "auto" | "sdr" | "hdr".
    pub fn select_label(&self) -> &'static str {
        use std::sync::atomic::Ordering::Relaxed;
        if self.auto.load(Relaxed) {
            "auto"
        } else if self.mode.load(Relaxed) == MODE_TONEMAP {
            "hdr"
        } else {
            "sdr"
        }
    }

    /// Apply a `set_mode` command ("auto" | "sdr" | "hdr").
    pub fn set_select(&self, value: &str) {
        use std::sync::atomic::Ordering::Relaxed;
        match value {
            "sdr" => {
                self.auto.store(false, Relaxed);
                self.mode.store(MODE_PASSTHROUGH, Relaxed);
            }
            "hdr" => {
                self.auto.store(false, Relaxed);
                self.mode.store(MODE_TONEMAP, Relaxed);
            }
            _ => self.auto.store(true, Relaxed),
        }
    }
}

/// Enumerate monitors for the Hello message.
pub fn list_monitors() -> Vec<MonitorInfo> {
    let mut out = Vec::new();
    let primary_idx = Monitor::primary().ok().and_then(|m| m.index().ok());
    if let Ok(monitors) = Monitor::enumerate() {
        for m in monitors {
            let index = m.index().unwrap_or(0);
            out.push(MonitorInfo {
                index,
                name: m.name().unwrap_or_else(|_| format!("Display {index}")),
                width: m.width().unwrap_or(0),
                height: m.height().unwrap_or(0),
                primary: Some(index) == primary_idx,
            });
        }
    }
    out
}

type CaptureError = Box<dyn std::error::Error + Send + Sync>;

/// Opaque handle that keeps the capture thread alive; held by the app.
pub type CaptureControlHandle = CaptureControl<Handler, CaptureError>;

/// Dump the Nth frame so capture has warmed up (avoids the first blank frames).
const DUMP_AT_FRAME: u64 = 30;

/// The windows-capture handler. Owns the shared slot + a reusable readback
/// scratch buffer so we allocate at most one frame Vec per arrival.
pub struct Handler {
    shared: Arc<CaptureShared>,
    scratch: Vec<u8>,
    frame_count: u64,
    timestamp_origin: Option<i64>,
    /// When set (via `RECALL_DUMP`), write one verification PNG then clear it.
    dump_path: Option<String>,
    /// When set (via `RECALL_DUMP_EVERY=<secs>`), write a sequenced PNG every
    /// interval — the stress-test harness for cycling display configs.
    dump_every: Option<std::time::Duration>,
    last_dump: std::time::Instant,
    dump_seq: u32,
}

impl GraphicsCaptureApiHandler for Handler {
    type Flags = Arc<CaptureShared>;
    type Error = CaptureError;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let dump_path = std::env::var("RECALL_DUMP").ok().map(|v| {
            if v.is_empty() || v == "1" {
                "captures/recall-dump.png".to_string()
            } else {
                v
            }
        });
        let dump_every = std::env::var("RECALL_DUMP_EVERY")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|s| *s > 0.0)
            .map(std::time::Duration::from_secs_f64);
        Ok(Self {
            shared: ctx.flags,
            scratch: Vec::new(),
            frame_count: 0,
            timestamp_origin: None,
            dump_path,
            dump_every,
            last_dump: std::time::Instant::now(),
            dump_seq: 0,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let (width, height) = (frame.width(), frame.height());
        let timestamp = frame.timestamp()?.Duration;
        let origin = *self.timestamp_origin.get_or_insert(timestamp);
        let pts_us = (timestamp.saturating_sub(origin).max(0) as u64) / 10;
        if let Ok(mut timeline) = self.shared.timeline.lock() {
            *timeline = Some((pts_us, std::time::Instant::now()));
        }

        // CPU readback: maps a staging copy; `as_nopadding_buffer` strips the
        // D3D row-pitch padding into `scratch` (handles Rgba16F = 8 bytes/px).
        let fb = frame.buffer()?;
        let packed = fb.as_nopadding_buffer(&mut self.scratch);

        let data = FrameData {
            pts_us,
            width,
            height,
            bytes: packed.to_vec(),
        };

        self.frame_count += 1;
        let mode = self.shared.mode.load(std::sync::atomic::Ordering::Relaxed);

        // One-shot dump after warmup.
        if self.frame_count >= DUMP_AT_FRAME {
            if let Some(path) = self.dump_path.take() {
                match crate::debug_dump::dump(width, height, &data.bytes, mode, 1.0, 6.0, &path) {
                    Ok(stats) => eprintln!("[recall dump]\n{stats}"),
                    Err(e) => eprintln!("[recall dump] failed: {e}"),
                }
            }
        }

        // Periodic dump (stress-test harness): sequenced PNGs every interval.
        if let Some(interval) = self.dump_every {
            if self.frame_count >= DUMP_AT_FRAME && self.last_dump.elapsed() >= interval {
                self.dump_seq += 1;
                let path = format!("captures/stress/dump-{:03}.png", self.dump_seq);
                match crate::debug_dump::dump(width, height, &data.bytes, mode, 1.0, 6.0, &path) {
                    Ok(stats) => eprintln!("[recall dump #{}]\n{stats}", self.dump_seq),
                    Err(e) => eprintln!("[recall dump #{}] failed: {e}", self.dump_seq),
                }
                self.last_dump = std::time::Instant::now();
            }
        }

        // Overwrite the slot; if the render thread hasn't consumed the previous
        // frame we simply drop it — only the freshest frame matters for a live view.
        if let Ok(mut slot) = self.shared.latest.lock() {
            *slot = Some(data);
        }
        Ok(())
    }
}

/// Start capturing the primary monitor on its own thread. The returned
/// `CaptureControl` keeps the capture alive — drop or `.stop()` it to end.
pub fn start_monitor(
    index: Option<usize>,
    shared: Arc<CaptureShared>,
) -> Result<CaptureControlHandle, CaptureError> {
    // Ask for borderless access first so the session's IsBorderRequired(false)
    // (set by windows-capture for our WithoutBorder setting) actually suppresses
    // the Windows capture border.
    crate::border::request_borderless();

    let monitor = match index {
        Some(i) => Monitor::from_index(i)?,
        None => Monitor::primary()?,
    };
    let settings = Settings::new(
        monitor,
        CursorCaptureSettings::WithoutCursor,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Rgba16F,
        shared,
    );
    let control = Handler::start_free_threaded(settings).map_err(box_start_err)?;
    Ok(control)
}

/// Flatten the capture-start error into our boxed error type.
fn box_start_err(e: GraphicsCaptureApiError<CaptureError>) -> CaptureError {
    Box::new(std::io::Error::other(e.to_string()))
}

/// Capture an explicitly selected game window through the supported WGC API.
pub fn start_window(hwnd: usize, shared: Arc<CaptureShared>) -> Result<CaptureControlHandle, CaptureError> {
    if hwnd == 0 { return Err(std::io::Error::other("capture window handle is null").into()); }
    crate::border::request_borderless();
    let window = windows_capture::window::Window::from_raw_hwnd(hwnd as *mut std::ffi::c_void);
    let settings = Settings::new(
        window, CursorCaptureSettings::WithoutCursor, DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default, MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default, ColorFormat::Rgba16F, shared,
    );
    Handler::start_free_threaded(settings).map_err(box_start_err)
}
