//! Continuous NVENC VFR encode thread + replay ring + save hand-off (feature `encode`).
//!
//! The render loop feeds RGBA8 frames with capture-time microsecond PTS values;
//! this thread encodes them with `h264_nvenc` (pix_fmt RGB0 — NVENC ingests
//! RGBA directly, alpha ignored, no swscale stage) into a `PacketRing` holding
//! the last N seconds. A `SaveRequest` is turned into a self-contained
//! `SaveJob` here in milliseconds (shared packet window + PCM snapshot) and
//! written by the `recall-saver` thread (`saver.rs`, `mux.rs`), so a long save
//! never backs up the frame channel. VFR timing is retained in a 1/1_000_000
//! video time base.

mod frame_fit;
mod mux;
mod saver;
mod smoke;

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ffmpeg_next as ff;

use crate::audio::TrackRing;
use crate::ring::{EncodedPacket, PacketRing};
use frame_fit::normalize_small_size_jitter;
use mux::OwnedParams;
use saver::{ms, RawFrame, SaveJob, SaverControl, SaverQueue, Timing, VideoSrc};

pub use saver::clip_path;
pub use smoke::smoke_test;

/// One capture frame from the render thread. `idx` is capture PTS in microseconds.
pub struct PacedFrame {
    pub idx: u64,
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA8, row-major, no padding.
    pub rgba: Vec<u8>,
}

/// Ask the encoder thread to write the last `seconds` to `path`.
pub struct SaveRequest {
    pub path: String,
    pub seconds: u32,
    /// Capture-clock PTS at save request, used to retain a static tail.
    pub end_us: Option<u64>,
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
    /// Most recent initialization/reconfiguration failure for status consumers.
    pub error: Mutex<Option<String>>,
}

pub struct EncoderHandle {
    pub frames: SyncSender<PacedFrame>,
    pub commands: Sender<SaveRequest>,
    pub stats: Arc<EncStats>,
    saver: SaverControl,
}

impl EncoderHandle {
    /// Quit path: stop the encoder thread (it hands any already-requested saves
    /// to the saver first), then give in-flight saves up to `budget` to finish.
    /// Past the drain share the saver aborts and removes its `.part` file.
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

pub struct Config {
    pub fps: u32,
    pub buffer_secs: u32,
    pub bitrate: usize,
    /// Emit machine JSON events (Saved/SaveFailed) instead of human logs.
    pub json: bool,
}

/// Spawn the encoder thread and its saver. Initialization is lazy (first frame
/// carries the resolution); failures are logged and saves report SaveFailed.
/// `audio` rings are snapshotted at save time and muxed as one AAC track each.
pub fn spawn(cfg: Config, audio: Vec<Arc<TrackRing>>) -> EncoderHandle {
    // The render thread uses try_send: capture never queues unbounded raw RGBA.
    let (ftx, frx) = sync_channel::<PacedFrame>(3);
    let (ctx, crx) = channel::<SaveRequest>();
    let stats = Arc::new(EncStats::default());
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
    width: u32,
    height: u32,
    backend: &'static str,
    /// Newest raw frame (moved in after encode) — thumbnail and still source.
    last_frame: Option<RawFrame>,
    last_input_pts: Option<u64>,
    /// One id per engine run, mirrored into Moments rows.
    session_id: String,
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
    let mut enc: Option<Enc> = None;
    let mut failed_resolution: Option<(u32, u32)> = None;

    loop {
        // Saves first: they're rare and user-facing latency matters (R10).
        while let Ok(req) = commands.try_recv() {
            handle_save(enc.as_ref(), req, &stats, &audio, &saver);
        }

        match frames.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(mut frame) => {
                if let Some(active) = enc.as_ref() {
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
                if resolution_changed {
                    report(
                        cfg.json,
                        "capture resolution changed; reopening NVENC and resetting replay buffer",
                    );
                    enc = None;
                    failed_resolution = None;
                    stats.buffered_frames.store(0, Relaxed);
                    stats.buffered_us.store(0, Relaxed);
                    stats.buffer_start_us.store(0, Relaxed);
                    stats.buffered_bytes.store(0, Relaxed);
                }
                if enc.is_none() && failed_resolution != Some((frame.width, frame.height)) {
                    enc = match open_encoder(&cfg, frame.width, frame.height) {
                        Ok(e) => {
                            crate::longrec::tap::on_encoder_open(&e.encoder); // long-recording hook
                            stats.backend.store(1, Relaxed);
                            *stats.error.lock().unwrap_or_else(|e| e.into_inner()) = None;
                            report(
                                cfg.json,
                                &format!("encoder up: {} {}x{}", e.backend, e.width, e.height),
                            );
                            Some(e)
                        }
                        Err(e) => {
                            stats.backend.store(3, Relaxed);
                            *stats.error.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some(e.clone());
                            report(cfg.json, &format!("encoder init failed: {e}"));
                            failed_resolution = Some((frame.width, frame.height));
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
                if let Some(reason) = fatal_encode_error {
                    let failed = enc.as_ref().map(|e| (e.width, e.height));
                    enc = None;
                    failed_resolution = failed;
                    stats.backend.store(3, Relaxed);
                    *stats
                        .error
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                        Some(format!("NVENC encode failed: {reason}"));
                    stats.buffered_frames.store(0, Relaxed);
                    stats.buffered_us.store(0, Relaxed);
                    stats.buffer_start_us.store(0, Relaxed);
                    stats.buffered_bytes.store(0, Relaxed);
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

/// Open mandatory h264_nvenc for even-cropped `w`x`h` RGB0 input.
fn open_encoder(cfg: &Config, w: u32, h: u32) -> Result<Enc, String> {
    let (w, h) = (w & !1, h & !1);
    if w == 0 || h == 0 {
        return Err("NVENC cannot open a zero-sized capture".into());
    }
    let encoder = try_open(cfg, "h264_nvenc", w, h)
        .map_err(|e| format!("h264_nvenc initialization failed: {e}"))?;
    Ok(Enc {
        params: OwnedParams::from_encoder(&encoder),
        encoder,
        ring: PacketRing::new(cfg.buffer_secs as u64 * 1_000_000),
        nominal_fps: cfg.fps,
        bitrate: cfg.bitrate,
        width: w,
        height: h,
        backend: "h264_nvenc",
        last_frame: None,
        last_input_pts: None,
        session_id: uuid::Uuid::new_v4().to_string(),
    })
}

/// Copy tightly packed RGBA into an RGBZ video frame, honoring the stride.
fn fill_rgbz(vf: &mut ff::frame::Video, w: u32, h: u32, rgba: &[u8]) {
    let stride = vf.stride(0);
    let src_row = (w * 4) as usize;
    let dst = vf.data_mut(0);
    for y in 0..h as usize {
        let n = src_row.min(rgba.len() - y * src_row);
        dst[y * stride..y * stride + n].copy_from_slice(&rgba[y * src_row..y * src_row + n]);
    }
}

fn try_open(cfg: &Config, name: &str, w: u32, h: u32) -> Result<ff::encoder::Video, String> {
    let codec = ff::encoder::find_by_name(name).ok_or_else(|| format!("codec {name} not found"))?;
    let mut ctx = ff::codec::context::Context::new_with_codec(codec)
        .encoder()
        .video()
        .map_err(|e| e.to_string())?;
    ctx.set_width(w);
    ctx.set_height(h);
    // AV_PIX_FMT_RGB0 (RGBA bytes, alpha ignored) is `RGBZ` in the Rust enum.
    ctx.set_format(ff::format::Pixel::RGBZ);
    ctx.set_time_base(ff::Rational(1, 1_000_000));
    ctx.set_frame_rate(Some(ff::Rational(cfg.fps as i32, 1)));
    ctx.set_gop(cfg.fps); // 1s GOP -> save granularity
                          // No B-frames: packets leave in presentation order (dts == pts), so the
                          // ring can stream-copy with monotonic timestamps (the DTS-regression fix).
    ctx.set_max_b_frames(0);
    ctx.set_bit_rate(cfg.bitrate);
    ctx.set_max_bit_rate(cfg.bitrate * 2);
    ctx.set_flags(ff::codec::Flags::GLOBAL_HEADER); // SPS/PPS in extradata for mp4
    let mut opts = ff::Dictionary::new();
    opts.set("preset", "p5");
    opts.set("tune", "hq");
    opts.set("rc", "vbr");
    opts.set("bf", "0");
    ctx.open_with(opts).map_err(|e| e.to_string())
}

impl Enc {
    /// Encode one paced frame; drain packets into the ring. Keeps the raw
    /// frame afterwards as the thumbnail/still source.
    fn encode(&mut self, frame: PacedFrame, stats: &EncStats) -> Result<(), String> {
        self.send_rgba(frame.idx, frame.width, &frame.rgba)?;
        self.last_frame = Some((frame.width, frame.height, Arc::new(frame.rgba)));
        self.drain(stats)
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
                data,
            });
            stats.encoded_frames.fetch_add(1, Relaxed);
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

    /// Detach everything a save needs from the live encoder: the ring window
    /// (refcounted payloads) or a still source, owned codec parameters, and
    /// each audio track's PCM snapshotted now, aligned to the video length.
    fn build_job(
        &self,
        req: SaveRequest,
        audio: &[Arc<TrackRing>],
        started: Instant,
    ) -> Result<SaveJob, String> {
        let mut timing = Timing {
            request_wait_ms: ms(started.saturating_duration_since(req.requested)),
            ..Timing::default()
        };
        let phase = Instant::now();
        let end_us = req
            .end_us
            .unwrap_or_else(|| self.ring.last_pts_us().unwrap_or(0));
        let want_us = req.seconds as u64 * 1_000_000;
        let quiet_tail = self
            .last_input_pts
            .is_some_and(|pts| pts < end_us.saturating_sub(want_us));
        let (video, secs) = if quiet_tail {
            // A fresh keyframe-only still: reusing an old inter-frame packet
            // would need its references and span the whole quiet gap. The still
            // lasts exactly the requested window (the ring holds it >= 1 µs).
            let frame = self
                .last_frame
                .clone()
                .ok_or("no frame available for static save")?;
            let still = VideoSrc::Still {
                frame,
                fps: self.nominal_fps,
                bitrate: self.bitrate,
            };
            (still, want_us.max(1) as f64 / 1_000_000.0)
        } else {
            let packets = self.ring.save_window_at(want_us, end_us);
            if packets.is_empty() {
                return Err("replay buffer empty (no keyframe yet)".into());
            }
            let secs = saver::window_secs(&packets);
            let params = self.params.clone();
            (VideoSrc::Window { packets, params }, secs)
        };
        timing.window_ms = ms(phase.elapsed());
        let phase = Instant::now();
        let audio = audio.iter().map(|ring| ring.snapshot_pcm(secs)).collect();
        timing.snapshot_ms = ms(phase.elapsed());
        Ok(SaveJob {
            path: req.path,
            seconds: req.seconds,
            requested: req.requested,
            secs,
            video,
            backend: self.backend,
            audio,
            thumb: self.last_frame.clone(),
            width: self.width,
            height: self.height,
            fps: self.nominal_fps,
            session_id: self.session_id.clone(),
            timing,
        })
    }
}

/// Encode one raw frame as a fresh keyframe-only NVENC stream lasting
/// `seconds` (quiet-tail saves). Runs on the saver thread with its own
/// session; the live encoder and its replay ring are untouched.
fn encode_still(
    frame: &RawFrame,
    fps: u32,
    bitrate: usize,
    seconds: u32,
) -> Result<(Vec<EncodedPacket>, OwnedParams), String> {
    let (width, height, rgba) = frame;
    let cfg = Config {
        fps,
        buffer_secs: seconds.max(1),
        bitrate,
        json: false,
    };
    let mut still = open_encoder(&cfg, *width, *height)?;
    let stats = EncStats::default();
    still.send_rgba(0, *width, rgba)?;
    still.drain(&stats)?;
    still
        .encoder
        .send_eof()
        .map_err(|e| format!("NVENC still flush: {e}"))?;
    still.drain(&stats)?;
    let want_us = seconds as u64 * 1_000_000;
    Ok((still.ring.save_window_at(want_us, want_us), still.params.clone()))
}

/// Encoder-thread half of a save: build the job and queue it. Never muxes;
/// a busy writer fails the save immediately instead of stalling capture.
fn handle_save(
    enc: Option<&Enc>,
    req: SaveRequest,
    stats: &EncStats,
    audio: &[Arc<TrackRing>],
    saver: &SaverQueue,
) {
    let started = Instant::now();
    let Some(enc) = enc else {
        let reason = stats
            .error
            .lock()
            .ok()
            .and_then(|e| e.clone())
            .unwrap_or_else(|| "encoder not initialized (no frames yet)".into());
        saver.events().save_failed(&reason);
        return;
    };
    match enc.build_job(req, audio, started) {
        Ok(mut job) => {
            job.timing.encoder_blocked_ms = ms(started.elapsed());
            saver.submit(job);
        }
        Err(reason) => saver.events().save_failed(&reason),
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
