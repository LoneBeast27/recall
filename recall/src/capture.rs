//! Windows.Graphics.Capture pipeline for Recall.
//!
//! Captures the primary monitor in `Rgba16F` (scRGB-linear; carries HDR) on a
//! dedicated thread, reads each frame back to CPU, and parks the latest frame
//! in a shared slot for the render thread to upload + tonemap. CPU readback is
//! the simple/debuggable Phase-0 path; zero-copy D3D11->D3D12 interop is the
//! deferred optimization.

use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex};

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
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

/// Hand-off point between the capture thread and the render thread.
pub struct CaptureShared {
    /// Most recent frame; render thread `take()`s it. None until first frame.
    pub latest: Mutex<Option<FrameData>>,
    /// Current tonemap mode (set by HDR/SDR auto-detect later; tonemap for now).
    pub mode: AtomicU32,
}

impl CaptureShared {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            latest: Mutex::new(None),
            // Default to passthrough: correct/readable for SDR sources. HDR
            // auto-detect flips this to tonemap once the display is known.
            mode: AtomicU32::new(MODE_PASSTHROUGH),
        })
    }
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

        // CPU readback: maps a staging copy; `as_nopadding_buffer` strips the
        // D3D row-pitch padding into `scratch` (handles Rgba16F = 8 bytes/px).
        let fb = frame.buffer()?;
        let packed = fb.as_nopadding_buffer(&mut self.scratch);

        let data = FrameData {
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
pub fn start_primary_monitor(
    shared: Arc<CaptureShared>,
) -> Result<CaptureControlHandle, CaptureError> {
    // Ask for borderless access first so the session's IsBorderRequired(false)
    // (set by windows-capture for our WithoutBorder setting) actually suppresses
    // the Windows capture border.
    crate::border::request_borderless();

    let monitor = Monitor::primary()?;
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
