//! Windows.Graphics.Capture pipeline for Recall.
//!
//! Captures a window or monitor in `Rgba16F` (scRGB-linear; carries HDR) on a
//! dedicated thread. Recording takes the zero-copy path (`gpu_frames`): each frame is
//! tonemapped on the capture device into an FFmpeg D3D11 frame and handed to the encoder,
//! with no CPU readback. When that path cannot start (or `RECALL_DUMP*` asks for fp16
//! pixels) the frame is read back to the CPU and parked in a shared slot for the render
//! thread to upload + tonemap (the Phase-0 path).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering::Relaxed};
#[cfg(feature = "encode")]
use std::sync::OnceLock;
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

/// Which path frames take (`CaptureShared::path`).
pub const PATH_STARTING: u8 = 0;
/// Zero-copy: tonemapped on the GPU, encoded from VRAM; the render loop idles.
#[cfg_attr(not(feature = "encode"), allow(dead_code))]
pub const PATH_GPU: u8 = 1;
/// CPU readback into `latest` for the render thread (fallback / dump harness).
pub const PATH_CPU: u8 = 2;

/// Where GPU frames go (the encoder's frame queue) and where the capture thread logs.
#[cfg(feature = "encode")]
pub struct GpuSink {
    /// Hand one frame (capture PTS in us) to the encoder; false once the encoder is gone.
    pub send: Box<dyn Fn(u64, crate::gpu_frames::HwFrame) -> bool + Send + Sync>,
    pub log: Box<dyn Fn(String) + Send + Sync>,
}

/// Consecutive GPU-path failures after which capture falls back to the CPU path.
#[cfg(feature = "encode")]
const GPU_FAILURES_BEFORE_FALLBACK: u32 = 30;

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
    /// `PATH_STARTING` / `PATH_GPU` / `PATH_CPU`, set by the capture thread.
    pub path: AtomicU8,
    /// Encoder frame size of the newest GPU frame (status line).
    pub gpu_size: Mutex<Option<(u32, u32)>>,
    /// Newest GPU frame (screenshots read it back on request).
    #[cfg(feature = "encode")]
    pub latest_gpu: Mutex<Option<crate::gpu_frames::HwFrame>>,
    /// Set once by the engine before capture starts; without it frames take the CPU path.
    #[cfg(feature = "encode")]
    pub gpu_sink: OnceLock<GpuSink>,
}

impl CaptureShared {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            latest: Mutex::new(None),
            timeline: Mutex::new(None),
            mode: AtomicU32::new(MODE_PASSTHROUGH),
            auto: AtomicBool::new(true),
            quit: AtomicBool::new(false),
            path: AtomicU8::new(PATH_STARTING),
            gpu_size: Mutex::new(None),
            #[cfg(feature = "encode")]
            latest_gpu: Mutex::new(None),
            #[cfg(feature = "encode")]
            gpu_sink: OnceLock::new(),
        })
    }

    /// Current capture timeline, including time since the latest delivered frame.
    pub fn recording_now_us(&self) -> Option<u64> {
        self.recording_now().map(|(us, _)| us)
    }

    /// [`Self::recording_now_us`] and the `Instant` it was read at, from one
    /// clock sample (the long recording maps capture time onto wall time).
    pub fn recording_now(&self) -> Option<(u64, std::time::Instant)> {
        let now = std::time::Instant::now();
        self.timeline.lock().ok().and_then(|t| {
            t.map(|(pts, at)| (pts.saturating_add(now.saturating_duration_since(at).as_micros() as u64), now))
        })
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

/// The windows-capture handler: the zero-copy pipeline, or the CPU readback into the
/// shared slot (one frame Vec per arrival).
pub struct Handler {
    shared: Arc<CaptureShared>,
    frame_count: u64,
    timestamp_origin: Option<i64>,
    /// When set (via `RECALL_DUMP`), write one verification PNG then clear it.
    dump_path: Option<String>,
    /// When set (via `RECALL_DUMP_EVERY=<secs>`), write a sequenced PNG every
    /// interval — the stress-test harness for cycling display configs.
    dump_every: Option<std::time::Duration>,
    last_dump: std::time::Instant,
    dump_seq: u32,
    #[cfg(feature = "encode")]
    gpu: GpuState,
}

/// The capture thread's zero-copy pipeline, built on the first frame (it needs the
/// capture device).
#[cfg(feature = "encode")]
enum GpuState {
    Untried,
    Ready { pipe: crate::gpu_frames::Pipeline, failures: u32 },
    Off,
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
            frame_count: 0,
            timestamp_origin: None,
            dump_path,
            dump_every,
            last_dump: std::time::Instant::now(),
            dump_seq: 0,
            #[cfg(feature = "encode")]
            gpu: GpuState::Untried,
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

        #[cfg(feature = "encode")]
        if self.dump_path.is_none() && self.dump_every.is_none() && self.gpu_frame(frame, pts_us) {
            return Ok(());
        }
        self.shared.path.store(PATH_CPU, Relaxed);

        // CPU readback (fallback): map a staging copy and strip the row padding with one
        // single-threaded copy. (windows-capture's `as_nopadding_buffer` treats every
        // Rgba16F frame as padded and fans the copy out over a 32-thread rayon pool.)
        let mut fb = frame.buffer()?;
        let pitch = fb.row_pitch() as usize;
        let row = width as usize * 8;
        let raw = fb.as_raw_buffer();
        let mut bytes = Vec::with_capacity(row * height as usize);
        for y in 0..height as usize {
            bytes.extend_from_slice(&raw[y * pitch..y * pitch + row]);
        }

        let data = FrameData {
            pts_us,
            width,
            height,
            bytes,
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

#[cfg(feature = "encode")]
impl Handler {
    /// Zero-copy path: tonemap this frame on the GPU and hand it to the encoder. False sends
    /// the frame down the CPU path (no encoder sink, the pipeline cannot start, or it kept
    /// failing).
    fn gpu_frame(&mut self, frame: &mut Frame, pts_us: u64) -> bool {
        let Some(sink) = self.shared.gpu_sink.get() else { return false };
        if let GpuState::Untried = self.gpu {
            self.gpu = match crate::gpu_frames::Pipeline::new(frame.device(), frame.device_context()) {
                Ok(pipe) => {
                    (sink.log)("capture: zero-copy GPU path (D3D11 tonemap -> NVENC)".into());
                    GpuState::Ready { pipe, failures: 0 }
                }
                Err(e) => {
                    (sink.log)(format!("capture: GPU path unavailable ({e}); using CPU copies"));
                    GpuState::Off
                }
            };
        }
        let GpuState::Ready { pipe, failures } = &mut self.gpu else { return false };
        let (width, height) = (frame.width(), frame.height());
        let target = crate::gpu_frames::target_size(pipe.target(), (width, height));
        let mode = self.shared.mode.load(Relaxed);
        match pipe.render(frame.as_raw_texture(), width, height, target, mode) {
            Ok(hw) => {
                *failures = 0;
                self.frame_count += 1;
                self.shared.path.store(PATH_GPU, Relaxed);
                if let Ok(mut size) = self.shared.gpu_size.lock() {
                    *size = Some(target);
                }
                if let (Some(copy), Ok(mut latest)) = (hw.try_clone(), self.shared.latest_gpu.lock()) {
                    *latest = Some(copy);
                }
                (sink.send)(pts_us, hw);
                true
            }
            Err(e) => {
                *failures += 1;
                if *failures == 1 || *failures >= GPU_FAILURES_BEFORE_FALLBACK {
                    (sink.log)(format!("capture: GPU frame failed ({e}), failure {}", *failures));
                }
                if *failures < GPU_FAILURES_BEFORE_FALLBACK {
                    return true; // skip this frame; the next one retries the GPU path
                }
                (sink.log)("capture: falling back to CPU copies".into());
                self.gpu = GpuState::Off;
                if let Ok(mut latest) = self.shared.latest_gpu.lock() {
                    *latest = None;
                }
                false
            }
        }
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
    let start = |interval| {
        Handler::start_free_threaded(Settings::new(
            monitor,
            CursorCaptureSettings::WithoutCursor,
            DrawBorderSettings::WithoutBorder,
            SecondaryWindowSettings::Default,
            interval,
            DirtyRegionSettings::Default,
            ColorFormat::Rgba16F,
            shared.clone(),
        ))
    };
    with_rate_cap(start)
}

/// Frames per second the capture is capped at: the recording rate plus 10% headroom (the CFR
/// pacer makes exact frames from them). Uncapped, Windows hands over every frame the game
/// presents (500+ a second in VALORANT) and each is read back to the CPU (29.5 MB at 1440p
/// HDR), which costs the game frames.
pub(crate) fn capture_interval(recording_fps: u32) -> std::time::Duration {
    std::time::Duration::from_secs_f64(1.0 / (f64::from(recording_fps.clamp(1, 1000)) * 1.1))
}

/// `RECALL_FPS` as the encoder reads it (1..=1000, default 60).
fn recording_fps() -> u32 {
    std::env::var("RECALL_FPS").ok().and_then(|v| v.trim().parse().ok()).filter(|f| (1..=1000).contains(f)).unwrap_or(60)
}

/// Starts with the rate cap; Windows builds without `MinUpdateInterval` (before 11 24H2) fall
/// back to the uncapped stream.
fn with_rate_cap<S>(start: S) -> Result<CaptureControlHandle, CaptureError>
where
    S: Fn(MinimumUpdateIntervalSettings) -> Result<CaptureControlHandle, GraphicsCaptureApiError<CaptureError>>,
{
    let interval = capture_interval(recording_fps());
    match start(MinimumUpdateIntervalSettings::Custom(interval)) {
        Ok(control) => {
            eprintln!("[recall] capture capped at {:.0} fps", 1.0 / interval.as_secs_f64());
            Ok(control)
        }
        Err(e) => {
            eprintln!("[recall] capture rate cap unavailable ({e}); capturing every frame");
            start(MinimumUpdateIntervalSettings::Default).map_err(box_start_err)
        }
    }
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
    let start = |interval| {
        Handler::start_free_threaded(Settings::new(
            window, CursorCaptureSettings::WithoutCursor, DrawBorderSettings::WithoutBorder,
            SecondaryWindowSettings::Default, interval,
            DirtyRegionSettings::Default, ColorFormat::Rgba16F, shared.clone(),
        ))
    };
    with_rate_cap(start)
}

#[cfg(test)]
mod rate_cap_tests {
    #[test]
    fn interval_leaves_ten_percent_headroom_over_the_recording_rate() {
        let at60 = super::capture_interval(60);
        assert!((1.0 / at60.as_secs_f64() - 66.0).abs() < 0.01);
        assert!((1.0 / super::capture_interval(144).as_secs_f64() - 158.4).abs() < 0.01);
        assert_eq!(super::capture_interval(0), super::capture_interval(1));
    }
}
