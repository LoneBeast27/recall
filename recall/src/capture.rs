//! Windows.Graphics.Capture pipeline for Recall.
//!
//! Captures a window or monitor on a dedicated thread: `Bgra8` while the zero-copy path
//! records an SDR display (half the memory traffic of fp16), `Rgba16F` (scRGB-linear;
//! carries HDR) for HDR, the CPU fallback and the dump harness (`CaptureShared::wants_fp16`). Recording takes the zero-copy path (`gpu_frames`): each frame is
//! tonemapped on the capture device into an FFmpeg D3D11 frame and handed to the encoder,
//! with no CPU readback. When that path cannot start (or `RECALL_DUMP*` asks for fp16
//! pixels) the frame is read back to the CPU and parked in a shared slot for the render
//! thread to upload + tonemap (the Phase-0 path).
//!
//! A windowed game (title bar / borders: `client_area`) is cropped to its client area on
//! both paths and recorded on a canvas of its own size (`framing`).

mod client_area;
mod control;
mod dda;
mod dda_desktop;
mod dda_dxgi;
mod dda_logic;
mod dda_window;
#[cfg(feature = "encode")]
mod gpu_path;
pub mod screen;
mod start;

pub use control::CaptureControlHandle;
pub use start::{start_monitor, start_window};

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, AtomicUsize, Ordering::Relaxed};
#[cfg(feature = "encode")]
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};

use crate::framing::Rect;
use crate::protocol::MonitorInfo;
use client_area::ClientArea;

use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D};
use windows_capture::capture::{
    CaptureControl, Context, GraphicsCaptureApiHandler,
};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::ColorFormat;

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
    /// Hand one frame (capture PTS in us, and the region holding the picture when it sits
    /// on the canvas) to the encoder; false once the encoder is gone or cannot take GPU
    /// frames (capture then switches to the CPU path).
    pub send: Box<dyn Fn(u64, crate::gpu_frames::HwFrame, Option<Rect>) -> bool + Send + Sync>,
    pub log: Box<dyn Fn(String) + Send + Sync>,
    /// True once when the encoder wants GPU frames again after refusing them: a capture on
    /// CPU copies tries the zero-copy path again (`encoder/reopen.rs` `GpuProbe`).
    pub retry_gpu: Box<dyn Fn() -> bool + Send + Sync>,
}

/// A captured frame, tightly packed f16 RGBA (8 bytes/pixel), ready to upload.
pub struct FrameData {
    /// Native WGC timestamp, rebased to the first capture, in microseconds.
    pub pts_us: u64,
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
    /// The windowed-game canvas this frame is placed on (`width`x`height` is then the
    /// client area); None records the frame at its own size.
    pub canvas: Option<(u32, u32)>,
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
    /// Newest GPU frame and its picture region on the canvas (screenshots read it back on
    /// request).
    #[cfg(feature = "encode")]
    pub latest_gpu: Mutex<Option<(crate::gpu_frames::HwFrame, Option<Rect>)>>,
    /// The `stop_capture::next_frame_seq` of the frame in `latest_gpu`, set under its lock.
    #[cfg(feature = "encode")]
    pub latest_gpu_seq: std::sync::atomic::AtomicU64,
    /// The captured window (`start_window`), 0 for a monitor capture.
    pub window: AtomicUsize,
    /// Set once by the engine before capture starts; without it frames take the CPU path.
    #[cfg(feature = "encode")]
    pub gpu_sink: OnceLock<GpuSink>,
    /// The running capture delivers `Rgba16F` (false: `Bgra8`).
    pub fp16: AtomicBool,
    /// A `Bgra8` frame could not take the GPU path: the engine restarts capture in fp16.
    pub need_fp16: AtomicBool,
    /// The capture thread asks the engine for a new capture (Desktop Duplication gave up,
    /// or the followed game window left its monitor); taken by `restart_reason`.
    pub restart: AtomicBool,
    /// Desktop Duplication failed this run (device lost, re-create kept failing): later
    /// starts use WGC.
    pub dda_failed: AtomicBool,
    /// First WGC timestamp of the engine run, kept across capture restarts so PTS never
    /// jumps back.
    origin: Mutex<Option<i64>>,
    /// Capture interval in nanoseconds that the handler enforces itself, 0 for none: set when
    /// Windows has no `MinUpdateInterval` (before 11 24H2), so frames over the rate are dropped
    /// before any GPU work instead of each one being converted and sent to NVENC.
    pub(crate) soft_cap_ns: std::sync::atomic::AtomicU64,
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
            latest_gpu_seq: std::sync::atomic::AtomicU64::new(0),
            window: AtomicUsize::new(0),
            #[cfg(feature = "encode")]
            gpu_sink: OnceLock::new(),
            fp16: AtomicBool::new(true),
            need_fp16: AtomicBool::new(false),
            restart: AtomicBool::new(false),
            dda_failed: AtomicBool::new(false),
            origin: Mutex::new(None),
            soft_cap_ns: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// The capture format the next start should use: fp16 for HDR (tonemap mode), for the
    /// CPU fallback and the dump harness (they read 8-byte pixels), else `Bgra8`.
    /// `RECALL_CAPTURE_FP16=1` forces fp16.
    pub fn wants_fp16(&self) -> bool {
        !crate::gpu_route::gpu_path_possible()
            || self.mode.load(Relaxed) == MODE_TONEMAP
            || self.need_fp16.load(Relaxed)
            || std::env::var_os("RECALL_CAPTURE_FP16").is_some_and(|v| v == "1")
    }

    /// Why the running capture must be replaced, if it must: the capture thread asked
    /// (`restart`, taken here), or a Bgra8 capture needs fp16 (HDR display or CPU fallback).
    pub fn restart_reason(&self) -> Option<&'static str> {
        // A plain load first: the engine asks every tick, and a swap is a locked write each time.
        if self.restart.load(Relaxed) && self.restart.swap(false, Relaxed) {
            return Some(if self.dda_failed.load(Relaxed) { "(desktop duplication stopped; WGC from now on)" } else { "(capture source changed)" });
        }
        (!self.fp16.load(Relaxed) && self.wants_fp16()).then_some("in Rgba16F (HDR display or CPU fallback)")
    }

    /// Whether this start may use Desktop Duplication (`dda_logic::dda_wanted`).
    fn dda_wanted(&self) -> Result<(), &'static str> {
        dda_logic::dda_wanted(std::env::var("RECALL_CAPTURE").ok().as_deref(), self.dda_failed.load(Relaxed))
    }

    /// Picks the format for a capture start and records it in `fp16`.
    fn start_format(&self) -> ColorFormat {
        let fp16 = self.wants_fp16();
        self.fp16.store(fp16, Relaxed);
        eprintln!("[recall] capture format {}", if fp16 { "Rgba16F" } else { "Bgra8" });
        if fp16 { ColorFormat::Rgba16F } else { ColorFormat::Bgra8 }
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

/// The WGC capture's control (one variant of `CaptureControlHandle`).
pub(crate) type WgcControl = CaptureControl<Handler, CaptureError>;

/// One captured frame's GPU side as `Handler::deliver` sees it: device, context and texture
/// borrowed from the source (no COM AddRef/Release per frame).
pub(crate) struct Source<'a> {
    pub device: &'a ID3D11Device,
    pub context: &'a ID3D11DeviceContext,
    pub texture: &'a ID3D11Texture2D,
    pub size: (u32, u32),
    /// QPC-based time in 100 ns units (WGC `SystemRelativeTime`).
    pub timestamp: i64,
}

/// A captured frame from either source (WGC, Desktop Duplication), handed to `Handler::deliver`.
pub(crate) trait FrameSource {
    /// The frame's borrowed GPU side, for the zero-copy path.
    fn source(&self) -> Source<'_>;
    /// Reads the frame's `rect` back to the CPU as tightly packed fp16 RGBA (CPU path only).
    fn read(&mut self, rect: Rect) -> Result<Vec<u8>, CaptureError>;
}

/// A WGC frame as a `FrameSource`; the timestamp is read once on arrival.
struct WgcFrame<'f, 'a> {
    frame: &'f mut Frame<'a>,
    timestamp: i64,
}

impl FrameSource for WgcFrame<'_, '_> {
    fn source(&self) -> Source<'_> {
        let f = &*self.frame;
        Source { device: f.device(), context: f.device_context(), texture: f.as_raw_texture(), size: (f.width(), f.height()), timestamp: self.timestamp }
    }

    /// CPU readback (fallback): map a staging copy and copy out the client area (the whole
    /// frame unless the window has chrome), stripping the row padding, with one
    /// single-threaded copy. (windows-capture's `as_nopadding_buffer` treats every Rgba16F
    /// frame as padded and fans the copy out over a 32-thread rayon pool.)
    fn read(&mut self, rect: Rect) -> Result<Vec<u8>, CaptureError> {
        let mut fb = self.frame.buffer()?;
        let pitch = fb.row_pitch() as usize;
        Ok(crate::framing::copy_rect(fb.as_raw_buffer(), pitch, 8, rect))
    }
}

/// Dump the Nth frame so capture has warmed up (avoids the first blank frames).
const DUMP_AT_FRAME: u64 = 30;

/// The windows-capture handler: the zero-copy pipeline, or the CPU readback into the
/// shared slot (one frame Vec per arrival).
pub struct Handler {
    shared: Arc<CaptureShared>,
    frame_count: u64,
    /// When set (via `RECALL_DUMP`), write one verification PNG then clear it.
    dump_path: Option<String>,
    /// When set (via `RECALL_DUMP_EVERY=<secs>`), write a sequenced PNG every
    /// interval — the stress-test harness for cycling display configs.
    dump_every: Option<std::time::Duration>,
    last_dump: std::time::Instant,
    dump_seq: u32,
    /// The captured window's client area (windowed-game crop + canvas).
    client: ClientArea,
    #[cfg(feature = "encode")]
    gpu: gpu_path::GpuState,
    /// The rate cap kept by the handler when Windows cannot keep it (`soft_cap_ns`).
    soft_cap: Option<dda_logic::Pacer>,
}

impl GraphicsCaptureApiHandler for Handler {
    type Flags = Arc<CaptureShared>;
    type Error = CaptureError;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self::from_shared(ctx.flags))
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let timestamp = frame.timestamp()?.Duration;
        self.deliver(&mut WgcFrame { frame, timestamp })
    }
}

impl Handler {
    pub(crate) fn from_shared(shared: Arc<CaptureShared>) -> Self {
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
        let client = ClientArea::new(shared.window.load(Relaxed));
        let soft_cap = match shared.soft_cap_ns.load(Relaxed) {
            0 => None,
            ns => Some(dda_logic::Pacer::new(std::time::Duration::from_nanos(ns))),
        };
        Self {
            soft_cap,
            shared,
            frame_count: 0,
            dump_path,
            dump_every,
            last_dump: std::time::Instant::now(),
            dump_seq: 0,
            client,
            #[cfg(feature = "encode")]
            gpu: gpu_path::GpuState::Untried,
        }
    }

    /// For Desktop Duplication, which paces its own acquires: no second cap in the handler.
    pub(crate) fn paced_elsewhere(mut self) -> Self {
        self.soft_cap = None;
        self
    }

    /// One frame from either source: the zero-copy GPU path, else the CPU readback
    /// (`FrameSource::read`) into the shared slot.
    pub(crate) fn deliver(&mut self, frame: &mut impl FrameSource) -> Result<(), CaptureError> {
        if let Some(pacer) = self.soft_cap.as_mut() {
            if pacer.offer(std::time::Instant::now()) != dda_logic::Pace::Deliver {
                return Ok(()); // over the capture rate: no tonemap, copy or encode for this frame
            }
        }
        let src = frame.source();
        let (width, height) = src.size;
        let timestamp = src.timestamp;
        let origin = match self.shared.origin.lock() {
            Ok(mut origin) => *origin.get_or_insert(timestamp),
            Err(_) => timestamp,
        };
        let pts_us = (timestamp.saturating_sub(origin).max(0) as u64) / 10;
        if let Ok(mut timeline) = self.shared.timeline.lock() {
            *timeline = Some((pts_us, std::time::Instant::now()));
        }

        let (crop, canvas) = self.client.update((width, height), std::time::Instant::now());

        #[cfg(feature = "encode")]
        if self.dump_path.is_none() && self.dump_every.is_none() && self.gpu_frame(&src, pts_us, crop, canvas) {
            return Ok(());
        }
        if !self.shared.fp16.load(Relaxed) {
            // The CPU path reads 8-byte fp16 pixels: drop Bgra8 frames until the engine
            // restarts capture in fp16 (app_tick `restart_capture_if_needed`).
            self.shared.need_fp16.store(true, Relaxed);
            return Ok(());
        }
        self.shared.path.store(PATH_CPU, Relaxed);

        let rect = crop.unwrap_or(Rect::full((width, height)));
        let bytes = frame.read(rect)?;
        let (width, height) = rect.size();

        let data = FrameData {
            pts_us,
            width,
            height,
            bytes,
            canvas,
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

#[cfg(test)]
#[path = "capture/capture_tests.rs"]
mod tests;
