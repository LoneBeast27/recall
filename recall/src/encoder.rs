//! Continuous NVENC VFR encode thread + replay ring + save hand-off (feature `encode`).
//!
//! Frames arrive with capture-time microsecond PTS values, either as D3D11 GPU frames from
//! the capture thread (the zero-copy path, `gpu_frames`: NVENC reads them from VRAM) or as
//! RGBA8 from the render loop (fallback; pix_fmt RGB0, NVENC ingests RGBA directly). This
//! thread encodes them with `h264_nvenc`, `hevc_nvenc` or `av1_nvenc`
//! (`RECALL_CODEC`, see `settings.rs`) into a `PacketRing` holding
//! the last N seconds. A `SaveRequest` is turned into a self-contained
//! `SaveJob` on this thread in milliseconds (`handoff.rs`; shared packet
//! window and PCM snapshot) and
//! written by the `recall-saver` thread (`saver.rs`, `mux.rs`), so a long save
//! never backs up the frame channel. A clip folder on another volume than the
//! SSD staging folder gets the clip staged first and moved in the background
//! (`staging.rs`, `mover.rs`). VFR timing is retained in a 1/1_000_000
//! video time base.

mod bg_io;
pub(crate) use bg_io::enter_background;
mod events;
#[cfg(test)]
mod faults;
mod frame_fit;
mod gpu_smoke;
mod handoff;
mod live;
#[cfg(test)]
pub(crate) use handoff::{audio_tail_us, video_end_us};
mod mover;
mod mux;
mod reopen;
mod replay;
#[cfg(test)]
mod recover_tests;
#[cfg(test)]
mod headers_tests;
#[cfg(test)]
mod decode_tests;
mod saver;
mod space;
mod staging;
pub mod settings;
mod smoke;
mod still;
mod vram_watch;

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{channel, sync_channel, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ffmpeg_next as ff;

use crate::audio::TrackRing;
use crate::ring::{EncodedPacket, Payload};
use frame_fit::fill_rgbz;
use live::run;
use mux::OwnedParams;
use replay::Replay;
use saver::{ms, SaverControl};
use settings::Codec;
use still::encode_still;
pub(crate) use still::StillSpec;

pub use saver::clip_path;
pub use settings::Config;
pub use gpu_smoke::gpu_smoke;
pub use smoke::smoke_test;

/// One capture frame. `idx` is capture PTS in microseconds.
pub struct PacedFrame {
    pub idx: u64,
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA8, row-major, no padding (empty for a GPU frame).
    pub rgba: Vec<u8>,
    /// The zero-copy path's frame: tonemapped BGRA in a D3D11 texture.
    pub gpu: Option<crate::gpu_frames::HwFrame>,
    /// Where the picture sits when the frame is a windowed game on the canvas (bars around
    /// it); thumbnails take only this region. None = the whole frame.
    pub content: Option<crate::framing::Rect>,
}

/// Ask the encoder thread to write the last `seconds` to `path`.
pub struct SaveRequest {
    pub path: String,
    pub seconds: u32,
    /// Capture-clock PTS at save request, used to retain a static tail.
    pub end_us: Option<u64>,
    /// End the clip at the newest frame instead of holding it up to `end_us`
    /// (the controller sets it once the game has closed: a drain).
    pub at_last_frame: bool,
    /// When the user asked; the `Saved` event's `elapsed_ms` starts here.
    pub requested: Instant,
}

/// Live counters for Status reporting (written by the encoder thread).
#[derive(Default)]
pub struct EncStats {
    pub buffered_frames: AtomicU64,
    pub buffered_us: AtomicU64,
    pub buffer_start_us: AtomicU64,
    pub buffered_bytes: AtomicU64,
    pub encoded_frames: AtomicU64,
    /// 0 = not initialized, 1 = nvenc, 3 = failed (recovery spent), 4 = recovering (`label`).
    pub backend: AtomicU64,
    /// `Codec::index`: the requested codec until an encoder opens, then the
    /// one actually recording (H.264 after a fallback).
    pub codec: AtomicU64,
    /// NVENC could not open on GPU frames: capture switches to the CPU path for good.
    pub gpu_refused: std::sync::atomic::AtomicBool,
    /// Most recent initialization/reconfiguration failure for status consumers.
    pub error: Mutex<Option<String>>,
    /// The GPU path may be tried again: capture's handler leaves CPU copies (taken by `install_gpu_sink`).
    pub gpu_retry: std::sync::atomic::AtomicBool,
    /// Asks capture for a new device (`CaptureShared::restart`; set by `install_gpu_sink`).
    pub capture_restart: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>,
    /// Injected failures (tests drive recovery on the real loop).
    #[cfg(test)]
    pub(crate) faults: faults::Faults,
}

pub struct EncoderHandle {
    pub frames: SyncSender<PacedFrame>,
    pub commands: Sender<SaveRequest>,
    pub stats: Arc<EncStats>,
    saver: SaverControl,
}

impl EncStats {
    /// Codec for `status.codec`: active once open, requested before that.
    pub fn codec(&self) -> Codec {
        Codec::from_index(self.codec.load(Relaxed))
    }

    /// `status.encoder`: "h264_nvenc" (replay ready, for every codec; the overlay,
    /// native app and hub compare it literally), "degraded" (recording from CPU copies
    /// after GPU frames were refused), "recovering" (the encoder is being rebuilt; the
    /// buffer is kept and saveable), "failed" (the engine's own recovery is spent),
    /// "starting".
    pub fn label(&self) -> &'static str {
        match self.backend.load(Relaxed) {
            1 if self.gpu_refused.load(Relaxed) => "degraded",
            1 => "h264_nvenc",
            3 => "failed",
            4 => "recovering",
            _ => "starting",
        }
    }

    /// The encoder cannot reopen on this capture device (`reopen::RESTART_EVERY`).
    fn request_capture_restart(&self) {
        if let Some(restart) = self.capture_restart.get() {
            restart();
        }
    }

    /// The replay buffer was reset (encoder reopened or failed).
    fn clear_buffer(&self) {
        [&self.buffered_frames, &self.buffered_us, &self.buffer_start_us, &self.buffered_bytes].iter().for_each(|a| a.store(0, Relaxed));
    }
}

impl EncoderHandle {
    /// Quit path: stop the encoder thread (it hands any already-requested saves
    /// to the saver first), then give in-flight saves up to `budget` to finish.
    /// Past the drain share the saver aborts and removes its `.part` file.
    /// Then the clip mover stops (<= `mover::STOP_WAIT`): a background move in
    /// flight is abandoned, never leaving a partial file under the final name;
    /// its staged copy is moved by the next start.
    pub fn shutdown(self, budget: Duration) {
        let EncoderHandle { frames, commands, saver, .. } = self;
        // Disconnecting frames ends the encoder loop, which drops the saver queue.
        drop((frames, commands));
        let started = Instant::now();
        let outcome = saver.shutdown(budget);
        eprintln!(
            "[recall] save writer shutdown: {outcome:?} after {:.0}ms",
            ms(started.elapsed())
        );
    }
}

/// Spawn the encoder thread and its saver. Initialization is lazy (first frame
/// carries the resolution); failures are logged and saves report SaveFailed.
/// `audio` rings are snapshotted at save time and muxed as one AAC track each.
pub fn spawn(cfg: Config, audio: Vec<Arc<TrackRing>>) -> EncoderHandle {
    // The render thread uses try_send: capture never queues unbounded raw RGBA.
    let (ftx, frx) = sync_channel::<PacedFrame>(3);
    let (ctx, crx) = channel::<SaveRequest>();
    let stats = Arc::new(EncStats::default());
    stats.codec.store(cfg.codec.index(), Relaxed);
    let tstats = stats.clone();
    let (queue, saver) = saver::spawn_default(saver::Events::new(cfg.json));
    std::thread::Builder::new()
        .name("recall-encoder".into())
        .spawn(move || run(cfg, frx, crx, tstats, audio, queue))
        .expect("spawn encoder thread");
    EncoderHandle {
        frames: ftx,
        commands: ctx,
        stats,
        saver,
    }
}

/// One open NVENC session. The replay buffer it fills lives apart (`replay.rs`), so
/// it survives this encoder failing.
struct Enc {
    encoder: ff::encoder::Video,
    /// Codec parameters (SPS/PPS extradata) copied once after open.
    params: OwnedParams,
    codec: Codec,
    width: u32,
    height: u32,
    backend: &'static str,
    /// `Some(pool_id)` when NVENC reads D3D11 frames from that pool, `None` for RGB0.
    gpu_pool: Option<usize>,
}

/// The retry schedule (tests shorten it).
fn retry_delays(_stats: &EncStats) -> [Duration; 4] {
    #[cfg(test)]
    return _stats.faults.delays(reopen::RETRY_DELAYS);
    #[cfg(not(test))]
    reopen::RETRY_DELAYS
}

/// `open_encoder` for this frame (tests can make it fail).
fn open_checked(cfg: &Config, want: Codec, frame: &PacedFrame, _stats: &EncStats) -> Result<(Enc, Option<String>), String> {
    #[cfg(test)]
    if let Some(e) = _stats.faults.open(gpu_pool_of(frame)) {
        return Err(e);
    }
    open_encoder(cfg, want, frame.width, frame.height, frame.gpu.as_ref())
}

/// How long a GPU-path try waits for its answer (tests shorten it).
fn probe_verdict(_stats: &EncStats) -> Duration {
    #[cfg(test)]
    if _stats.faults.fast.load(Relaxed) {
        return Duration::from_secs(2);
    }
    reopen::PROBE_VERDICT
}

/// The GPU-path retry schedule (tests shorten it).
fn probe_delays(_stats: &EncStats) -> [Duration; 4] {
    #[cfg(test)]
    return _stats.faults.probe_delays(reopen::PROBE_DELAYS);
    #[cfg(not(test))]
    reopen::PROBE_DELAYS
}

/// The frames pool a GPU frame comes from (`None` for an RGBA frame).
fn gpu_pool_of(frame: &PacedFrame) -> Option<usize> {
    frame.gpu.as_ref().map(|g| g.pool_id())
}

/// Open `codec` (falling back to h264_nvenc) for even-cropped `w`x`h` input: D3D11 frames
/// from `gpu`'s pool when given, else RGB0. The second value is the fallback line to log.
fn open_encoder(
    cfg: &Config,
    codec: Codec,
    w: u32,
    h: u32,
    gpu: Option<&crate::gpu_frames::HwFrame>,
) -> Result<(Enc, Option<String>), String> {
    let (w, h) = (w & !1, h & !1);
    if w == 0 || h == 0 {
        return Err("NVENC cannot open a zero-sized capture".into());
    }
    let pool = gpu.map(|g| g.frames_ctx());
    let opened = settings::open_with_fallback(codec, |c| try_open(cfg, c, w, h, pool))?;
    let gpu_pool = gpu.map(|g| g.pool_id());
    let encoder = opened.value;
    let enc = Enc {
        params: OwnedParams::from_encoder(&encoder),
        encoder,
        codec: opened.codec,
        width: w,
        height: h,
        backend: opened.codec.nvenc(),
        gpu_pool,
    };
    Ok((enc, opened.fallback))
}

fn try_open(
    cfg: &Config,
    codec: Codec,
    w: u32,
    h: u32,
    gpu_pool: Option<*mut ff::ffi::AVBufferRef>,
) -> Result<ff::encoder::Video, String> {
    let name = codec.nvenc();
    let codec = ff::encoder::find_by_name(name)
        .ok_or_else(|| format!("{name} is not in this FFmpeg build"))?;
    let mut ctx = ff::codec::context::Context::new_with_codec(codec)
        .encoder()
        .video()
        .map_err(|e| e.to_string())?;
    ctx.set_width(w);
    ctx.set_height(h);
    match gpu_pool {
        // Dev check of the CPU fallback (scripts/dev/engine-cost.ps1 -RefuseGpu).
        Some(_) if std::env::var_os("RECALL_REFUSE_GPU_FRAMES").is_some() => {
            return Err("GPU frames refused (RECALL_REFUSE_GPU_FRAMES)".into());
        }
        // D3D11 frames: NVENC opens on the capture device and registers the pool textures.
        Some(pool) => {
            ctx.set_format(ff::format::Pixel::D3D11);
            unsafe { (*ctx.as_mut_ptr()).hw_frames_ctx = ff::ffi::av_buffer_ref(pool) };
        }
        // AV_PIX_FMT_RGB0 (RGBA bytes, alpha ignored) is `RGBZ` in the Rust enum.
        None => ctx.set_format(ff::format::Pixel::RGBZ),
    }
    ctx.set_time_base(ff::Rational(1, 1_000_000));
    ctx.set_frame_rate(Some(ff::Rational(cfg.fps as i32, 1)));
    ctx.set_gop(cfg.fps); // 1s GOP -> save granularity
    // No B-frames: packets leave in presentation order (dts == pts), so the ring can stream-copy.
    ctx.set_max_b_frames(0);
    ctx.set_bit_rate(cfg.bitrate);
    ctx.set_max_bit_rate(cfg.bitrate * 2);
    // Parameter sets (SPS/PPS, HEVC VPS, AV1 sequence header) go to extradata
    // for the mp4 sample entry instead of being repeated in-band.
    ctx.set_flags(ff::codec::Flags::GLOBAL_HEADER);
    let mut opts = ff::Dictionary::new();
    let quality = cfg.quality.for_gpu(crate::gpu_frames::adapter::low_end());
    for (key, value) in quality.nvenc_options().into_iter().chain(settings::queue_options(gpu_pool.is_some())) {
        opts.set(key, value);
    }
    ctx.open_with(opts).map_err(|e| e.to_string())
}

impl Enc {
    /// Encode one paced frame; drain packets into the replay buffer, which keeps the raw
    /// frame afterwards as the thumbnail/still source.
    fn encode(&mut self, frame: PacedFrame, stats: &EncStats, replay: &mut Replay) -> Result<(), String> {
        #[cfg(test)]
        if let Some(e) = stats.faults.encode() {
            return Err(e);
        }
        replay.last_content = frame.content;
        match frame.gpu {
            Some(mut gpu) => {
                gpu.set_pts(frame.idx as i64);
                // NVENC keeps its own reference to the D3D11 frame until encoded.
                let ret = unsafe { ff::ffi::avcodec_send_frame(self.encoder.as_mut_ptr(), gpu.as_ptr()) };
                if ret < 0 {
                    return Err(ff::Error::from(ret).to_string());
                }
                replay.last_gpu = Some(gpu);
                replay.last_frame = None;
            }
            None => {
                self.send_rgba(frame.idx, frame.width, &frame.rgba)?;
                replay.last_frame = Some((frame.width, frame.height, Arc::new(frame.rgba)));
            }
        }
        replay.last_input_pts = Some(frame.idx);
        self.drain(stats, replay)
    }

    fn send_rgba(&mut self, idx: u64, width: u32, rgba: &[u8]) -> Result<(), String> {
        let mut vf = ff::frame::Video::new(ff::format::Pixel::RGBZ, self.width, self.height);
        fill_rgbz(&mut vf, self.width.min(width), self.height, rgba);
        vf.set_pts(Some(idx as i64));
        self.encoder.send_frame(&vf).map_err(|e| e.to_string())
    }

    fn drain(&mut self, stats: &EncStats, replay: &mut Replay) -> Result<(), String> {
        let mut pkt = ff::Packet::empty();
        loop {
            match self.encoder.receive_packet(&mut pkt) {
                Ok(()) => {}
                Err(e) if crate::export::receive_is_drained(&e) => break,
                Err(e) => return Err(format!("receive NVENC packet: {e}")),
            }
            let Some(data) = pkt.data() else { continue };
            let pts = pkt.pts().unwrap_or(0).max(0) as u64;
            // NVENC may leave packet duration unset. The next PTS is the
            // authoritative duration for VFR; the current tail gets a nominal
            // value until it can be refined by the next packet (across an encoder
            // gap, that holds the last frame through it).
            if let Some(previous_pts) = replay.ring.last_pts_us() {
                if pts > previous_pts {
                    replay.ring.set_last_duration_us(pts - previous_pts);
                }
            }
            // The one payload copy; save windows share it by refcount.
            let data: Arc<[u8]> = Arc::from(data);
            crate::longrec::tap::tap_packet(pts, pkt.is_key(), &data); // long-recording hook
            replay.push(EncodedPacket {
                frame_idx: pts,
                duration_us: (1_000_000 / replay.nominal_fps.max(1) as u64).max(1),
                keyframe: pkt.is_key(),
                data: Payload::from(data),
            });
            stats.encoded_frames.fetch_add(1, Relaxed);
        }
        if !replay.dropped_noted && replay.ring.dropped_for_ram() > 0 {
            replay.dropped_noted = true;
            eprintln!(
                "[recall] replay buffer: RAM budget full and no spill file; keeping the last {:.0} s instead of {} s",
                replay.ring.span_us() as f64 / 1e6,
                replay.ring_capacity_secs()
            );
        }
        replay.publish(stats);
        Ok(())
    }
}

fn report(json: bool, msg: &str) {
    if json {
        crate::protocol::emit(&crate::protocol::Outbound::EncoderLog {
            message: msg.to_string(),
        });
    } else {
        eprintln!("[recall] {msg}");
    }
}
