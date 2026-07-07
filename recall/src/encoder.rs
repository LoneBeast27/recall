//! Continuous NVENC encode thread + replay ring + save/mux (feature `encode`).
//!
//! The render loop feeds paced RGBA8 frames (exact CFR indices from `Pacer`);
//! this thread encodes them with `h264_nvenc` (pix_fmt RGB0 — NVENC ingests
//! RGBA directly, alpha ignored, no swscale stage) into a `PacketRing` holding
//! the last N seconds. A `SaveRequest` muxes the ring window into a fast
//! stream-copy mp4 — PTS are frame indices in a 1/fps timebase, so the file is
//! CFR by construction (R1).
//!
//! Encoder fallback: `h264_nvenc` -> `libx264` (logged) so the chain still
//! proves out if NVENC sessions are exhausted.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Instant;

use ffmpeg_next as ff;

use crate::audio::TrackRing;
use crate::ring::{EncodedPacket, PacketRing};

/// One paced frame from the render thread. `idx` is the CFR frame index.
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
}

/// Live counters for Status reporting (written by the encoder thread).
#[derive(Default)]
pub struct EncStats {
    pub buffered_frames: AtomicU64,
    pub buffered_bytes: AtomicU64,
    pub encoded_frames: AtomicU64,
    /// 0 = not initialized, 1 = nvenc, 2 = libx264 fallback, 3 = failed.
    pub backend: AtomicU64,
}

pub struct EncoderHandle {
    pub frames: Sender<PacedFrame>,
    pub commands: Sender<SaveRequest>,
    pub stats: Arc<EncStats>,
}

pub struct Config {
    pub fps: u32,
    pub buffer_secs: u32,
    pub bitrate: usize,
    /// Emit machine JSON events (Saved/SaveFailed) instead of human logs.
    pub json: bool,
}

/// Spawn the encoder thread. Initialization is lazy (first frame carries the
/// resolution); failures are logged and saves report SaveFailed. `audio` rings
/// are snapshotted at save time and muxed as one AAC track each.
pub fn spawn(cfg: Config, audio: Vec<Arc<TrackRing>>) -> EncoderHandle {
    let (ftx, frx) = channel::<PacedFrame>();
    let (ctx, crx) = channel::<SaveRequest>();
    let stats = Arc::new(EncStats::default());
    let tstats = stats.clone();
    std::thread::Builder::new()
        .name("recall-encoder".into())
        .spawn(move || run(cfg, frx, crx, tstats, audio))
        .expect("spawn encoder thread");
    EncoderHandle {
        frames: ftx,
        commands: ctx,
        stats,
    }
}

struct Enc {
    encoder: ff::encoder::Video,
    ring: PacketRing,
    fps: u32,
    width: u32,
    height: u32,
    backend: &'static str,
    /// Newest raw frame (moved in after encode) — the thumbnail source.
    last_frame: Option<(u32, u32, Vec<u8>)>,
    /// One id per engine run, mirrored into Moments rows.
    session_id: String,
}

fn run(
    cfg: Config,
    frames: Receiver<PacedFrame>,
    commands: Receiver<SaveRequest>,
    stats: Arc<EncStats>,
    audio: Vec<Arc<TrackRing>>,
) {
    if let Err(e) = ff::init() {
        report(cfg.json, &format!("ffmpeg init failed: {e}"));
        stats.backend.store(3, Relaxed);
        return;
    }
    let mut enc: Option<Enc> = None;

    loop {
        // Saves first: they're rare and user-facing latency matters (R10).
        while let Ok(req) = commands.try_recv() {
            handle_save(&cfg, enc.as_mut(), &req, &stats, &audio);
        }

        match frames.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(frame) => {
                if enc.is_none() {
                    enc = match open_encoder(&cfg, frame.width, frame.height) {
                        Ok(e) => {
                            stats.backend.store(if e.backend == "h264_nvenc" { 1 } else { 2 }, Relaxed);
                            report(cfg.json, &format!("encoder up: {} {}x{}", e.backend, e.width, e.height));
                            Some(e)
                        }
                        Err(e) => {
                            stats.backend.store(3, Relaxed);
                            report(cfg.json, &format!("encoder init failed: {e}"));
                            // Drain frames forever; saves will fail with a reason.
                            None
                        }
                    };
                }
                if let Some(enc) = enc.as_mut() {
                    let idx = frame.idx;
                    if let Err(e) = enc.encode(frame, &stats) {
                        report(cfg.json, &format!("encode error at frame {idx}: {e}"));
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// Open h264_nvenc (fallback libx264) for even-cropped `w`x`h` RGB0 input.
fn open_encoder(cfg: &Config, w: u32, h: u32) -> Result<Enc, String> {
    let (w, h) = (w & !1, h & !1);
    for name in ["h264_nvenc", "libx264"] {
        match try_open(cfg, name, w, h) {
            Ok(encoder) => {
                return Ok(Enc {
                    encoder,
                    ring: PacketRing::new(cfg.buffer_secs as u64 * cfg.fps as u64),
                    fps: cfg.fps,
                    width: w,
                    height: h,
                    backend: if name == "h264_nvenc" { "h264_nvenc" } else { "libx264" },
                    last_frame: None,
                    session_id: uuid::Uuid::new_v4().to_string(),
                })
            }
            Err(e) => report(cfg.json, &format!("{name} open failed ({e}); trying next")),
        }
    }
    Err("no usable h264 encoder (nvenc, x264 both failed)".into())
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
    ctx.set_time_base(ff::Rational(1, cfg.fps as i32));
    ctx.set_frame_rate(Some(ff::Rational(cfg.fps as i32, 1)));
    ctx.set_gop(cfg.fps); // 1s GOP -> save granularity
    // No B-frames: packets leave in presentation order (dts == pts), so the
    // ring can stream-copy with monotonic timestamps (the DTS-regression fix).
    ctx.set_max_b_frames(0);
    ctx.set_bit_rate(cfg.bitrate);
    ctx.set_max_bit_rate(cfg.bitrate * 2);
    ctx.set_flags(ff::codec::Flags::GLOBAL_HEADER); // SPS/PPS in extradata for mp4
    let mut opts = ff::Dictionary::new();
    if name == "h264_nvenc" {
        opts.set("preset", "p5");
        opts.set("tune", "hq");
        opts.set("rc", "vbr");
        opts.set("bf", "0");
    } else {
        opts.set("preset", "veryfast");
        opts.set("bf", "0");
    }
    ctx.open_with(opts).map_err(|e| e.to_string())
}

impl Enc {
    /// Encode one paced frame; drain packets into the ring. Keeps the raw
    /// frame afterwards as the thumbnail source.
    fn encode(&mut self, frame: PacedFrame, stats: &EncStats) -> Result<(), String> {
        let mut vf = ff::frame::Video::new(ff::format::Pixel::RGBZ, self.width, self.height);
        fill_rgbz(&mut vf, self.width.min(frame.width), self.height, &frame.rgba);
        vf.set_pts(Some(frame.idx as i64));
        self.last_frame = Some((frame.width, frame.height, frame.rgba));
        self.encoder.send_frame(&vf).map_err(|e| e.to_string())?;
        self.drain(stats)
    }

    /// Scale the newest raw frame to ~640-wide and write a JPEG next to the
    /// clip per Moments' convention: `thumbs\<stem>-<epoch>-thumb.jpg`.
    fn write_thumbnail(&self, clip_path: &str) -> Result<String, String> {
        let (w, h, rgba) = self.last_frame.as_ref().ok_or("no frame for thumbnail")?;
        let (tw, th) = (640u32, ((640 * h / w) & !1).max(2));

        let mut src = ff::frame::Video::new(ff::format::Pixel::RGBZ, *w, *h);
        fill_rgbz(&mut src, *w, *h, rgba);
        let mut dst = ff::frame::Video::new(ff::format::Pixel::YUVJ420P, tw, th);
        let mut scaler = ff::software::scaling::Context::get(
            ff::format::Pixel::RGBZ,
            *w,
            *h,
            ff::format::Pixel::YUVJ420P,
            tw,
            th,
            ff::software::scaling::Flags::BILINEAR,
        )
        .map_err(|e| e.to_string())?;
        scaler.run(&src, &mut dst).map_err(|e| e.to_string())?;
        dst.set_pts(Some(0));

        let codec = ff::encoder::find_by_name("mjpeg").ok_or("mjpeg encoder missing")?;
        let mut ctx = ff::codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()
            .map_err(|e| e.to_string())?;
        ctx.set_width(tw);
        ctx.set_height(th);
        ctx.set_format(ff::format::Pixel::YUVJ420P);
        ctx.set_time_base(ff::Rational(1, 1));
        let mut jpeg = ctx.open().map_err(|e| e.to_string())?;
        jpeg.send_frame(&dst).map_err(|e| e.to_string())?;
        jpeg.send_eof().map_err(|e| e.to_string())?;
        let mut pkt = ff::Packet::empty();
        jpeg.receive_packet(&mut pkt).map_err(|e| e.to_string())?;
        let data = pkt.data().ok_or("empty jpeg packet")?;

        let clip = std::path::Path::new(clip_path);
        let dir = clip.parent().unwrap_or_else(|| std::path::Path::new(".")).join("thumbs");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let stem = clip.file_stem().and_then(|s| s.to_str()).unwrap_or("clip");
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let thumb = dir.join(format!("{stem}-{epoch}-thumb.jpg"));
        std::fs::write(&thumb, data).map_err(|e| e.to_string())?;
        Ok(thumb.to_string_lossy().into_owned())
    }

    fn drain(&mut self, stats: &EncStats) -> Result<(), String> {
        let mut pkt = ff::Packet::empty();
        while self.encoder.receive_packet(&mut pkt).is_ok() {
            let Some(data) = pkt.data() else { continue };
            self.ring.push(EncodedPacket {
                frame_idx: pkt.pts().unwrap_or(0).max(0) as u64,
                keyframe: pkt.is_key(),
                data: data.to_vec(),
            });
            stats.encoded_frames.fetch_add(1, Relaxed);
        }
        stats.buffered_frames.store(self.ring.span_frames(), Relaxed);
        stats.buffered_bytes.store(self.ring.bytes() as u64, Relaxed);
        Ok(())
    }

    /// Mux the last `seconds` of the ring into `path`: video stream-copied,
    /// one AAC stream per audio ring (snapshotted + encoded at save time).
    fn save(
        &mut self,
        path: &str,
        seconds: u32,
        audio: &[Arc<TrackRing>],
    ) -> Result<(u64, f32), String> {
        let window = self.ring.save_window(seconds as u64 * self.fps as u64);
        if window.is_empty() {
            return Err("replay buffer empty (no keyframe yet)".into());
        }
        let frames = window.last().map(|p| p.frame_idx + 1).unwrap_or(0);
        let secs = frames as f64 / self.fps as f64;

        // Encode each audio track's aligned window to AAC before opening the
        // muxer (encoder contexts must outlive add_stream/set_parameters).
        let mut tracks = Vec::new();
        for ring in audio {
            match encode_aac_window(ring, secs) {
                Ok(t) => tracks.push(t),
                Err(e) => return Err(format!("audio track '{}': {e}", ring.name)),
            }
        }

        let mut octx = ff::format::output(&path).map_err(|e| format!("open {path}: {e}"))?;
        let vcodec = ff::encoder::find_by_name(self.backend);
        let mut ost = octx.add_stream(vcodec).map_err(|e| e.to_string())?;
        ost.set_parameters(&self.encoder);
        ost.set_time_base(ff::Rational(1, self.fps as i32));
        for t in &tracks {
            let mut ast = octx
                .add_stream(ff::encoder::find(ff::codec::Id::AAC))
                .map_err(|e| e.to_string())?;
            ast.set_parameters(&t.encoder);
            ast.set_time_base(ff::Rational(1, t.rate as i32));
        }
        octx.write_header().map_err(|e| format!("write_header: {e}"))?;

        // Merge all packets in presentation-time order so the interleaver
        // never buffers a whole stream. (stream_idx, seconds, src_tb, pts, data, key)
        let vtb = ff::Rational(1, self.fps as i32);
        let mut merged: Vec<(usize, f64, ff::Rational, i64, &[u8], bool)> = Vec::new();
        for p in &window {
            let t = p.frame_idx as f64 / self.fps as f64;
            merged.push((0, t, vtb, p.frame_idx as i64, &p.data, p.keyframe));
        }
        for (i, t) in tracks.iter().enumerate() {
            let atb = ff::Rational(1, t.rate as i32);
            for (pts, data) in &t.packets {
                merged.push((i + 1, *pts as f64 / t.rate as f64, atb, *pts, data, true));
            }
        }
        merged.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        for (stream_idx, _, src_tb, pts, data, key) in merged {
            let tb = octx.stream(stream_idx).expect("stream").time_base();
            let mut pkt = ff::Packet::copy(data);
            pkt.set_pts(Some(pts));
            pkt.set_dts(Some(pts));
            if key {
                pkt.set_flags(ff::packet::Flags::KEY);
            }
            pkt.set_stream(stream_idx);
            pkt.rescale_ts(src_tb, tb);
            pkt.write_interleaved(&mut octx).map_err(|e| e.to_string())?;
        }
        octx.write_trailer().map_err(|e| e.to_string())?;
        Ok((frames, secs as f32))
    }
}

/// One audio track's AAC-encoded save window. The encoder context is kept so
/// the muxer can copy its codec parameters (incl. AudioSpecificConfig).
struct AacTrack {
    name: &'static str,
    rate: u32,
    encoder: ff::encoder::Audio,
    /// (pts in 1/rate, packet payload)
    packets: Vec<(i64, Vec<u8>)>,
}

/// Snapshot `secs` from a ring and encode to AAC (stereo/mono, native rate).
fn encode_aac_window(ring: &TrackRing, secs: f64) -> Result<AacTrack, String> {
    let samples = ring.snapshot(secs);
    let src_ch = ring.channels.max(1) as usize;
    let ch = src_ch.min(2); // fold >stereo down by taking the first two channels
    let layout = if ch == 1 {
        ff::ChannelLayout::MONO
    } else {
        ff::ChannelLayout::STEREO
    };

    let codec = ff::encoder::find(ff::codec::Id::AAC).ok_or("aac encoder missing")?;
    let mut ctx = ff::codec::context::Context::new_with_codec(codec)
        .encoder()
        .audio()
        .map_err(|e| e.to_string())?;
    ctx.set_rate(ring.rate as i32);
    ctx.set_format(ff::format::Sample::F32(ff::format::sample::Type::Planar));
    ctx.set_channel_layout(layout);
    ctx.set_bit_rate(192_000);
    ctx.set_time_base(ff::Rational(1, ring.rate as i32));
    ctx.set_flags(ff::codec::Flags::GLOBAL_HEADER);
    let mut enc = ctx.open().map_err(|e| format!("aac open: {e}"))?;

    let frame_size = if enc.frame_size() > 0 { enc.frame_size() as usize } else { 1024 };
    let total_frames = samples.len() / src_ch;
    let mut packets = Vec::new();
    let mut pkt = ff::Packet::empty();
    let mut pos = 0usize; // in sample-frames
    while pos < total_frames {
        let n = frame_size.min(total_frames - pos);
        let mut af = ff::frame::Audio::new(
            ff::format::Sample::F32(ff::format::sample::Type::Planar),
            frame_size,
            layout,
        );
        af.set_rate(ring.rate);
        for c in 0..ch {
            let plane = af.plane_mut::<f32>(c);
            for i in 0..frame_size {
                plane[i] = if i < n { samples[(pos + i) * src_ch + c] } else { 0.0 };
            }
        }
        af.set_pts(Some(pos as i64));
        enc.send_frame(&af).map_err(|e| e.to_string())?;
        while enc.receive_packet(&mut pkt).is_ok() {
            if let (Some(pts), Some(data)) = (pkt.pts(), pkt.data()) {
                packets.push((pts, data.to_vec()));
            }
        }
        pos += n;
    }
    enc.send_eof().map_err(|e| e.to_string())?;
    while enc.receive_packet(&mut pkt).is_ok() {
        if let (Some(pts), Some(data)) = (pkt.pts(), pkt.data()) {
            packets.push((pts, data.to_vec()));
        }
    }

    Ok(AacTrack {
        name: ring.name,
        rate: ring.rate,
        encoder: enc,
        packets,
    })
}

fn handle_save(cfg: &Config, enc: Option<&mut Enc>, req: &SaveRequest, _stats: &EncStats, audio: &[Arc<TrackRing>]) {
    let t = Instant::now();
    let Some(enc) = enc else {
        emit_save_failed(cfg.json, "encoder not initialized (no frames yet or init failed)");
        return;
    };
    match enc.save(&req.path, req.seconds, audio) {
        Ok((frames, secs)) => {
            let bytes = std::fs::metadata(&req.path).map(|m| m.len()).unwrap_or(0);
            emit_saved(cfg.json, &req.path, secs, frames, bytes, t.elapsed().as_millis() as u64);
            finish_gallery(cfg, enc, req, secs, bytes, audio);
        }
        Err(reason) => {
            // Never leave a half-muxed file behind.
            let _ = std::fs::remove_file(&req.path);
            emit_save_failed(cfg.json, &reason);
        }
    }
}

/// Post-save gallery hookup: thumbnail + version-guarded Moments DB row.
/// Failures here never un-save the clip — they're reported and skipped.
fn finish_gallery(cfg: &Config, enc: &Enc, req: &SaveRequest, secs: f32, bytes: u64, audio: &[Arc<TrackRing>]) {
    if !crate::moments::enabled() {
        return;
    }
    let thumb = match enc.write_thumbnail(&req.path) {
        Ok(t) => t,
        Err(e) => {
            report(cfg.json, &format!("thumbnail failed: {e}"));
            String::new()
        }
    };
    let recorded = chrono::Local::now() - chrono::Duration::milliseconds((secs * 1000.0) as i64);
    let meta = crate::moments::ClipMeta {
        path: req.path.clone(),
        thumbnail_path: thumb,
        width: enc.width,
        height: enc.height,
        fps: enc.fps,
        seconds: secs as f64,
        filesize: bytes,
        recorded_iso: recorded.format("%Y-%m-%dT%H:%M:%S%:z").to_string(),
        session_id: enc.session_id.clone(),
        audio_tracks: audio.iter().map(|r| r.name.to_string()).collect(),
    };
    match crate::moments::insert_clip(&meta) {
        Ok(id) => report(cfg.json, &format!("moments: row inserted {id}")),
        Err(e) => report(cfg.json, &format!("moments: SKIPPED -- {e}")),
    }
}

fn emit_saved(json: bool, path: &str, seconds: f32, frames: u64, bytes: u64, elapsed_ms: u64) {
    if json {
        crate::protocol::emit(&crate::protocol::Outbound::Saved {
            path: path.to_string(),
            seconds,
            frames,
            bytes,
            elapsed_ms,
        });
    } else {
        eprintln!("[recall] saved {path} ({seconds:.1}s, {frames} frames, {bytes} bytes, {elapsed_ms}ms)");
    }
}

fn emit_save_failed(json: bool, reason: &str) {
    if json {
        crate::protocol::emit(&crate::protocol::Outbound::SaveFailed {
            reason: reason.to_string(),
        });
    } else {
        eprintln!("[recall] save FAILED: {reason}");
    }
}

/// Build a Moments-convention clip path: `<Game>__<YYYY-MM-DD>__<HH-MM-SS>.mp4`.
/// Non-filename characters in the source name become dashes.
pub fn clip_path(out_dir: &str, source: &str) -> String {
    let mut name: String = source
        .chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '-' })
        .collect::<String>()
        .trim()
        .replace(' ', "-");
    if name.is_empty() {
        name = "Desktop".to_string();
    }
    let stamp = chrono::Local::now().format("%Y-%m-%d__%H-%M-%S");
    std::path::Path::new(out_dir)
        .join(format!("{name}__{stamp}.mp4"))
        .to_string_lossy()
        .into_owned()
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
