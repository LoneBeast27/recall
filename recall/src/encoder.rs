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
mod frame_fit;
mod gpu_smoke;
mod handoff;
#[cfg(test)]
pub(crate) use handoff::{audio_tail_us, video_end_us};
mod mover;
mod mux;
mod saver;
mod staging;
pub mod settings;
mod smoke;
mod still;
mod vram_watch;

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ffmpeg_next as ff;

use crate::audio::TrackRing;
use crate::ring::{EncodedPacket, PacketRing, Payload};
use crate::spill::Spill;
use frame_fit::{fill_rgbz, normalize_small_size_jitter, SmallFrames};
use handoff::handle_save;
use mux::OwnedParams;
use saver::{ms, RawFrame, SaverControl, SaverQueue};
use settings::{Codec, Quality};
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
    /// 0 = not initialized, 1 = nvenc, 3 = failed.
    pub backend: AtomicU64,
    /// `Codec::index`: the requested codec until an encoder opens, then the
    /// one actually recording (H.264 after a fallback).
    pub codec: AtomicU64,
    /// NVENC could not open on GPU frames: capture switches to the CPU path for good.
    pub gpu_refused: std::sync::atomic::AtomicBool,
    /// Most recent initialization/reconfiguration failure for status consumers.
    pub error: Mutex<Option<String>>,
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

struct Enc {
    encoder: ff::encoder::Video,
    /// Codec parameters (SPS/PPS extradata) copied once after open.
    params: OwnedParams,
    ring: PacketRing,
    nominal_fps: u32,
    bitrate: usize,
    codec: Codec,
    quality: Quality,
    width: u32,
    height: u32,
    backend: &'static str,
    /// Newest raw frame (moved in after encode) — thumbnail and still source.
    last_frame: Option<RawFrame>,
    /// Newest GPU frame: read back to a `RawFrame` only when a save needs one.
    last_gpu: Option<crate::gpu_frames::HwFrame>,
    /// The newest frame's picture region on the canvas (`PacedFrame::content`).
    last_content: Option<crate::framing::Rect>,
    /// `Some(pool_id)` when NVENC reads D3D11 frames from that pool, `None` for RGB0.
    gpu_pool: Option<usize>,
    last_input_pts: Option<u64>,
    /// The "RAM full, dropping old seconds" line was logged for this ring.
    dropped_noted: bool,
}

fn run(
    cfg: Config,
    frames: Receiver<PacedFrame>,
    commands: Receiver<SaveRequest>,
    stats: Arc<EncStats>,
    audio: Vec<Arc<TrackRing>>,
    saver: SaverQueue,
) {
    if let Err(e) = ff::init() {
        let reason = format!("ffmpeg init failed: {e}");
        report(cfg.json, &reason);
        stats.backend.store(3, Relaxed);
        *stats
            .error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(reason);
        return;
    }
    report(cfg.json, &cfg.summary());
    for note in &cfg.notes {
        report(cfg.json, note);
    }
    let mut enc: Option<Enc> = None;
    let mut failed_resolution: Option<(u32, u32)> = None;
    let mut vram = vram_watch::VramWatch::default();
    let mut small = SmallFrames::default();
    // After one fallback the engine stays on H.264: reopening on a resolution
    // change must not flap codecs or re-log the same failure.
    let mut want = cfg.codec;
    // One RAM budget and spill file for the whole run; every reopened ring shares it.
    let json = cfg.json;
    let spill = Arc::new(Spill::from_system(cfg.buffer_secs, cfg.bitrate, audio.len(), Arc::new(move |m: &str| report(json, m))));

    loop {
        // Saves first: they're rare and user-facing latency matters (R10).
        while let Ok(req) = commands.try_recv() {
            handle_save(enc.as_ref(), req, &stats, &audio, &saver);
        }

        match frames.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(mut frame) => {
                // After a refusal, GPU frames still queued are dropped (capture has switched).
                if frame.gpu.is_some() && stats.gpu_refused.load(Relaxed) {
                    continue;
                }
                if small.skips(&frame, |note| report(cfg.json, note)) {
                    continue;
                }
                // GPU frames arrive already cropped / padded to a stable size.
                if let Some(active) = enc.as_ref().filter(|_| frame.gpu.is_none()) {
                    if let Err(e) = normalize_small_size_jitter(
                        &mut frame,
                        active.width,
                        active.height,
                    ) {
                        report(cfg.json, &format!("capture frame rejected: {e}"));
                        continue;
                    }
                }
                let resolution_changed = enc.as_ref().is_some_and(|e| {
                    e.width != (frame.width & !1) || e.height != (frame.height & !1)
                });
                let input_changed = enc.as_ref().is_some_and(|e| e.gpu_pool != gpu_pool_of(&frame));
                if resolution_changed || input_changed {
                    report(
                        cfg.json,
                        if resolution_changed {
                            "capture resolution changed; reopening NVENC and resetting replay buffer"
                        } else {
                            "capture input changed (GPU / CPU frames); reopening NVENC and resetting replay buffer"
                        },
                    );
                    enc = None;
                    failed_resolution = None;
                    stats.clear_buffer();
                }
                if enc.is_none() && failed_resolution != Some((frame.width, frame.height)) {
                    enc = match open_encoder(&cfg, want, frame.width, frame.height, frame.gpu.as_ref(), Some(Arc::clone(&spill))) {
                        Ok((e, fallback)) => {
                            crate::longrec::tap::on_encoder_open(&e.encoder); // long-recording hook
                            if let Some(note) = fallback {
                                report(cfg.json, &note);
                                want = e.codec;
                            }
                            stats.codec.store(e.codec.index(), Relaxed);
                            stats.backend.store(1, Relaxed);
                            *stats.error.lock().unwrap_or_else(|e| e.into_inner()) = None;
                            report(
                                cfg.json,
                                &format!("encoder up: {} {}x{}", e.backend, e.width, e.height),
                            );
                            let _ = crate::gpu_frames::adapter::vram_line("encoder open").inspect(|l| report(cfg.json, l));
                            vram.opened();
                            Some(e)
                        }
                        Err(e) => {
                            stats.backend.store(3, Relaxed);
                            *stats.error.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some(e.clone());
                            report(cfg.json, &format!("encoder init failed: {e}"));
                            if frame.gpu.is_some() {
                                // Let the CPU frames that follow open the encoder instead.
                                stats.gpu_refused.store(true, Relaxed);
                                report(cfg.json, "NVENC refused GPU frames; capture switches to CPU copies");
                            } else {
                                failed_resolution = Some((frame.width, frame.height));
                            }
                            None
                        }
                    };
                }
                let mut fatal_encode_error = None;
                if let Some(enc) = enc.as_mut() {
                    let idx = frame.idx;
                    if let Err(e) = enc.encode(frame, &stats) {
                        report(cfg.json, &format!("encode error at frame {idx}: {e}"));
                        fatal_encode_error = Some(e);
                    }
                }
                let _ = vram.due().inspect(|l| report(cfg.json, l));
                if let Some(reason) = fatal_encode_error {
                    crate::longrec::tap::on_encoder_failed(&format!("NVENC encode failed: {reason}")); // long-recording hook
                    let failed = enc.as_ref().map(|e| (e.width, e.height));
                    enc = None;
                    failed_resolution = failed;
                    stats.backend.store(3, Relaxed);
                    *stats
                        .error
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                        Some(format!("NVENC encode failed: {reason}"));
                    stats.clear_buffer();
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    // Quit: saves requested before the frame channel closed still get written
    // (the saver drains them within the shutdown budget).
    while let Ok(req) = commands.try_recv() {
        handle_save(enc.as_ref(), req, &stats, &audio, &saver);
    }
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
    spill: Option<Arc<Spill>>,
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
        ring: PacketRing::with_spill(cfg.buffer_secs as u64 * 1_000_000, spill),
        dropped_noted: false,
        nominal_fps: cfg.fps,
        bitrate: cfg.bitrate,
        codec: opened.codec,
        quality: cfg.quality,
        width: w,
        height: h,
        backend: opened.codec.nvenc(),
        last_frame: None,
        last_gpu: None,
        last_content: None,
        gpu_pool,
        last_input_pts: None,
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
    fn ring_capacity_secs(&self) -> u64 {
        self.ring.capacity_us() / 1_000_000
    }

    /// Encode one paced frame; drain packets into the ring. Keeps the raw
    /// frame afterwards as the thumbnail/still source.
    fn encode(&mut self, frame: PacedFrame, stats: &EncStats) -> Result<(), String> {
        self.last_content = frame.content;
        match frame.gpu {
            Some(gpu) => self.send_gpu(frame.idx, gpu)?,
            None => {
                self.send_rgba(frame.idx, frame.width, &frame.rgba)?;
                self.last_frame = Some((frame.width, frame.height, Arc::new(frame.rgba)));
            }
        }
        self.drain(stats)
    }

    /// Hand a D3D11 frame to NVENC (it keeps its own reference until encoded).
    fn send_gpu(&mut self, idx: u64, mut gpu: crate::gpu_frames::HwFrame) -> Result<(), String> {
        gpu.set_pts(idx as i64);
        let ret = unsafe { ff::ffi::avcodec_send_frame(self.encoder.as_mut_ptr(), gpu.as_ptr()) };
        if ret < 0 {
            return Err(ff::Error::from(ret).to_string());
        }
        self.last_input_pts = Some(idx);
        self.last_gpu = Some(gpu);
        self.last_frame = None;
        Ok(())
    }

    fn send_rgba(&mut self, idx: u64, width: u32, rgba: &[u8]) -> Result<(), String> {
        let mut vf = ff::frame::Video::new(ff::format::Pixel::RGBZ, self.width, self.height);
        fill_rgbz(&mut vf, self.width.min(width), self.height, rgba);
        vf.set_pts(Some(idx as i64));
        self.encoder.send_frame(&vf).map_err(|e| e.to_string())?;
        self.last_input_pts = Some(idx);
        Ok(())
    }

    fn drain(&mut self, stats: &EncStats) -> Result<(), String> {
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
            // value until it can be refined by the next packet.
            if let Some(previous_pts) = self.ring.last_pts_us() {
                if pts > previous_pts {
                    self.ring.set_last_duration_us(pts - previous_pts);
                }
            }
            // The one payload copy; save windows share it by refcount.
            let data: Arc<[u8]> = Arc::from(data);
            crate::longrec::tap::tap_packet(pts, pkt.is_key(), &data); // long-recording hook
            self.ring.push(EncodedPacket {
                frame_idx: pts,
                duration_us: (1_000_000 / self.nominal_fps.max(1) as u64).max(1),
                keyframe: pkt.is_key(),
                data: Payload::from(data),
            });
            stats.encoded_frames.fetch_add(1, Relaxed);
        }
        if !self.dropped_noted && self.ring.dropped_for_ram() > 0 {
            self.dropped_noted = true;
            eprintln!(
                "[recall] replay buffer: RAM budget full and no spill file; keeping the last {:.0} s instead of {} s",
                self.ring.span_us() as f64 / 1e6,
                self.ring_capacity_secs()
            );
        }
        stats
            .buffered_frames
            .store(self.ring.span_frames(), Relaxed);
        stats.buffered_us.store(self.ring.span_us(), Relaxed);
        stats
            .buffer_start_us
            .store(self.ring.first_pts_us().unwrap_or(0), Relaxed);
        stats
            .buffered_bytes
            .store(self.ring.bytes() as u64, Relaxed);
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
