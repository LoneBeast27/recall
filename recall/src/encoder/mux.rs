//! Clip assembly on the saver thread: AAC encode of the snapshotted PCM, MP4
//! stream-copy mux of the packet window, and the gallery thumbnail. Nothing here touches the live encoder, its ring, or the audio rings.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Instant;

use ffmpeg_next as ff;

use super::saver::{ms, window_secs, Events, Muxed, RawFrame, SaveJob, Timing, VideoSrc, ABORTED};
use crate::ring::EncodedPacket;

/// Codec parameters (incl. SPS/PPS extradata) copied out of an opened encoder.
///
/// Always an owned `AVCodecParameters`: never a stream's borrowed one, whose
/// `Rc` owner must not cross threads. Cloning allocates another owned copy,
/// so each `SaveJob` carries its own.
#[derive(Clone)]
pub(crate) struct OwnedParams(ff::codec::Parameters);

impl OwnedParams {
    pub fn from_encoder(encoder: &ff::encoder::Video) -> Self {
        Self(ff::codec::Parameters::from(encoder))
    }

    /// H.264 parameters of `w`x`h` with these parameter-set bytes (replay tests).
    #[cfg(test)]
    pub fn test_h264(w: i32, h: i32, extradata: &[u8]) -> Self {
        let params = ff::codec::Parameters::new();
        // SAFETY: a fresh owned AVCodecParameters; extradata is av_malloc'd with the
        // padding FFmpeg requires and freed with the parameters.
        unsafe {
            let p = &mut *params.as_ptr().cast_mut();
            p.codec_type = ff::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
            p.codec_id = ff::ffi::AVCodecID::AV_CODEC_ID_H264;
            p.width = w;
            p.height = h;
            let pad = ff::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;
            let buf = ff::ffi::av_mallocz(extradata.len() + pad) as *mut u8;
            std::ptr::copy_nonoverlapping(extradata.as_ptr(), buf, extradata.len());
            p.extradata = buf;
            p.extradata_size = extradata.len() as i32;
        }
        Self(params)
    }

    /// Both streams can share one MP4 sample entry: same codec, size and parameter sets
    /// (SPS/PPS extradata), so packets of either decode with these. The input pixel
    /// format (D3D11 or RGB0) is not part of the stream.
    /// The parameter-set bytes (SPS/PPS, HEVC VPS, AV1 sequence header).
    #[cfg(test)]
    pub fn extradata(&self) -> Vec<u8> {
        // SAFETY: a live, owned AVCodecParameters; extradata is valid for its size.
        unsafe {
            let p = &*self.0.as_ptr();
            if p.extradata.is_null() || p.extradata_size <= 0 {
                return Vec::new();
            }
            std::slice::from_raw_parts(p.extradata, p.extradata_size as usize).to_vec()
        }
    }

    pub fn same_stream(&self, other: &OwnedParams) -> bool {
        // SAFETY: both point at live, owned AVCodecParameters; the extradata pointer is
        // valid for `extradata_size` bytes (or null with size 0).
        unsafe {
            let (a, b) = (&*self.0.as_ptr(), &*other.0.as_ptr());
            let bytes = |p: &ff::ffi::AVCodecParameters| match p.extradata.is_null() || p.extradata_size <= 0 {
                true => &[][..],
                false => std::slice::from_raw_parts(p.extradata, p.extradata_size as usize),
            };
            a.codec_id == b.codec_id && a.width == b.width && a.height == b.height && bytes(a) == bytes(b)
        }
    }
}

/// Production `MuxFn`: still-encode a quiet tail if needed, encode every
/// track to AAC, then mux everything into `part`.
pub(crate) fn produce(
    part: &Path,
    job: &SaveJob,
    abort: &AtomicBool,
    timing: &mut Timing,
) -> Result<Muxed, String> {
    let still;
    let (packets, params, backend) = match &job.video {
        VideoSrc::Window { packets, params } => (packets.as_slice(), params, job.backend),
        VideoSrc::Still { frame, spec } => {
            let started = Instant::now();
            still = super::encode_still(frame, *spec, job.seconds)?;
            timing.still_ms = ms(started.elapsed());
            (still.0.as_slice(), &still.1, still.2)
        }
    };
    if packets.is_empty() {
        return Err("replay buffer empty (no keyframe yet)".into());
    }
    let secs = window_secs(packets);
    if (secs - job.secs).abs() > 0.001 {
        return Err(format!(
            "video window {secs:.6}s does not match the {:.6}s audio snapshot",
            job.secs
        ));
    }

    let started = Instant::now();
    let tracks = encode_tracks(&job.audio, abort)?;
    timing.aac_ms = ms(started.elapsed());
    timing.aac_cpu_ms = tracks.iter().map(|t| t.cpu_ms).sum();
    timing.aac_coder = tracks.first().map_or("", |t| t.coder.name());

    let wrote = mux_mp4(part, params, backend, packets, &tracks, abort, &mut timing.part_packets)?;
    timing.mux_ms = wrote.mux_ms;
    timing.trailer_ms = wrote.trailer_ms;
    let (frames, secs) = match &wrote.partial {
        None => (packets.len() as u64, secs),
        Some(_) => (wrote.video_packets, wrote.video_end_us as f64 / 1_000_000.0),
    };
    Ok(Muxed { frames, secs: secs as f32, partial: wrote.partial })
}

/// What [`mux_mp4`] wrote into a finished file.
pub(crate) struct Wrote {
    pub mux_ms: f64,
    pub trailer_ms: f64,
    /// Cut short (an abort, a packet that could not be read or written) and finished with
    /// the packets before it: why.
    pub partial: Option<String>,
    pub video_packets: u64,
    /// End of the last video packet written (its pts + duration, microseconds).
    pub video_end_us: u64,
}

/// A packet's bytes while muxing: audio in hand, video possibly in the spill file.
enum Bytes<'a> {
    Audio(&'a [u8]),
    Video(&'a crate::ring::Payload),
}

/// True when every PTS is strictly greater than the one before. The ring is
/// stream-copied with dts == pts, which is only valid without B-frames (bf=0).
pub(crate) fn pts_strictly_increase(video: &[EncodedPacket]) -> bool {
    video.windows(2).all(|w| w[0].frame_idx < w[1].frame_idx)
}

const fn fourcc(tag: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*tag)
}

/// MP4 sample-entry tag forced for a video codec. HEVC must be `hvc1`
/// (parameter sets in the sample entry): FFmpeg's default `hev1` does not
/// play through Windows Media Foundation. AV1 has only `av01`; H.264 keeps
/// FFmpeg's `avc1` default.
pub(crate) fn mp4_video_tag(id: ff::codec::Id) -> Option<u32> {
    match id {
        ff::codec::Id::HEVC => Some(fourcc(b"hvc1")),
        ff::codec::Id::AV1 => Some(fourcc(b"av01")),
        _ => None,
    }
}

/// Stream metadata editors label tracks by: `title` (FFmpeg / the native
/// editor) and `handler_name` (the MP4 `hdlr` name most NLEs show).
fn track_metadata(title: &str) -> ff::Dictionary<'static> {
    let mut meta = ff::Dictionary::new();
    meta.set("title", title);
    meta.set("handler_name", title);
    meta
}

/// Mux a rebased video window (stream copy, 1/1_000_000) plus one AAC stream
/// per track into `path` as MP4. Codec-agnostic: the codec comes from `params`
/// (H.264, HEVC or AV1). `written` counts the packets in the file as they go in.
///
/// Never lose a clip: an abort (the engine quitting, a stalled save given up) or a packet
/// that cannot be read or written stops the packets there and the trailer is written for
/// those already in the file (`Wrote::partial`). Err only when no packet went in, or the
/// trailer itself failed (the `.part` then stays for the next start, `part_recover`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn mux_mp4(
    path: &Path,
    params: &OwnedParams,
    backend: &str,
    video: &[EncodedPacket],
    tracks: &[AacTrack],
    abort: &AtomicBool,
    written: &mut u64,
) -> Result<Wrote, String> {
    debug_assert!(
        pts_strictly_increase(video),
        "save window PTS must strictly increase (bf=0 stream copy, dts == pts)"
    );
    let started = Instant::now();
    // Explicit format: the in-progress `.part` name has no usable extension.
    let mut octx = ff::format::output_as(path, "mp4")
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    let vcodec = ff::encoder::find_by_name(backend);
    let mut ost = octx.add_stream(vcodec).map_err(|e| e.to_string())?;
    ost.set_parameters(params.0.clone());
    ost.set_time_base(ff::Rational(1, 1_000_000));
    if let Some(tag) = mp4_video_tag(params.0.id()) {
        // SAFETY: `ost` is a live stream of `octx`; avformat_new_stream
        // allocated its codecpar, which set_parameters just filled.
        unsafe { (*(*ost.as_mut_ptr()).codecpar).codec_tag = tag };
    }
    for t in tracks {
        let mut ast = octx
            .add_stream(ff::encoder::find(ff::codec::Id::AAC))
            .map_err(|e| e.to_string())?;
        ast.set_parameters(&t.encoder);
        ast.set_time_base(ff::Rational(1, t.rate as i32));
        ast.set_metadata(track_metadata(t.title));
    }
    // Each track's source, for the editor (app tracks start out of the mix). MP4 keeps custom
    // tags only at file level, and only with use_metadata_tags.
    let mut file_tags = ff::Dictionary::new();
    let sources: Vec<&str> = tracks.iter().map(|t| t.source).collect();
    file_tags.set(crate::edit_model::AUDIO_SOURCES_TAG, &serde_json::to_string(&sources).unwrap_or_default());
    octx.set_metadata(file_tags);
    let mut options = ff::Dictionary::new();
    options.set("movflags", "use_metadata_tags");
    octx.write_header_with(options)
        .map_err(|e| format!("write_header: {e}"))?;

    // Merge all packets in presentation-time order so the interleaver
    // never buffers a whole stream. (stream_idx, seconds, src_tb, pts, data, key)
    let vtb = ff::Rational(1, 1_000_000);
    // Video payloads may sit in the spill file: they are read one at a time while writing.
    let mut merged: Vec<(usize, f64, ff::Rational, i64, i64, Bytes<'_>, bool)> = Vec::new();
    for p in video {
        let t = p.frame_idx as f64 / 1_000_000.0;
        merged.push((
            0,
            t,
            vtb,
            p.frame_idx as i64,
            p.duration_us as i64,
            Bytes::Video(&p.data),
            p.keyframe,
        ));
    }
    for (i, t) in tracks.iter().enumerate() {
        let atb = ff::Rational(1, t.rate as i32);
        for (pts, data) in &t.packets {
            merged.push((i + 1, *pts as f64 / t.rate as f64, atb, *pts, 0, Bytes::Audio(data), true));
        }
    }
    merged.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    // Time bases are final only after write_header; rescale against those.
    let out_tbs: Vec<ff::Rational> = octx.streams().map(|s| s.time_base()).collect();
    let (mut partial, mut video_packets, mut video_end_us) = (None, 0u64, 0u64);
    for (stream_idx, _, src_tb, pts, duration, data, key) in merged {
        if abort.load(Relaxed) || abort_here(*written) {
            partial = Some(ABORTED.to_string());
            break;
        }
        let mut pkt = match data {
            Bytes::Audio(bytes) => ff::Packet::copy(bytes),
            Bytes::Video(payload) => match payload.read() {
                Ok(bytes) => ff::Packet::copy(&bytes),
                Err(e) => {
                    partial = Some(e);
                    break;
                }
            },
        };
        pkt.set_pts(Some(pts));
        pkt.set_dts(Some(pts));
        pkt.set_duration(duration);
        if key {
            pkt.set_flags(ff::packet::Flags::KEY);
        }
        pkt.set_stream(stream_idx);
        pkt.rescale_ts(src_tb, out_tbs[stream_idx]);
        let size = pkt.size() as u64;
        if let Err(e) = pkt.write_interleaved(&mut octx) {
            partial = Some(e.to_string());
            break;
        }
        *written += 1;
        if stream_idx == 0 {
            video_packets += 1;
            video_end_us = (pts + duration).max(0) as u64;
        }
        super::save_progress::wrote(size);
    }
    if let Some(reason) = &partial {
        if *written == 0 {
            return Err(reason.clone());
        }
    }
    let mux_ms = ms(started.elapsed());
    let trailer = Instant::now();
    octx.write_trailer().map_err(|e| match &partial {
        Some(reason) => format!("{reason}; finishing the file with the {written} packets written failed: {e}"),
        None => e.to_string(),
    })?;
    Ok(Wrote { mux_ms, trailer_ms: ms(trailer.elapsed()), partial, video_packets, video_end_us })
}

/// Production `GalleryFn`: the recorder thumbnail the library and the controller read.
/// Failures here never un-save the clip; they're reported and skipped.
pub(crate) fn finish_gallery(job: &SaveJob, _secs: f32, _bytes: u64, events: &Events) {
    let made = job
        .thumb
        .as_ref()
        .ok_or_else(|| "no frame for thumbnail".to_string())
        .and_then(|frame| write_thumbnail(&job.path, frame));
    if let Err(e) = made {
        events.log(&format!("thumbnail failed: {e}"));
    }
}

/// Scale the request-time raw frame to ~640-wide and write a JPEG next to the
/// clip: `thumbs\<stem>-<epoch>-thumb.jpg`.
fn write_thumbnail(clip_path: &str, frame: &RawFrame) -> Result<String, String> {
    let (w, h, rgba) = (frame.0, frame.1, frame.2.as_slice());
    let (tw, th) = (640u32, ((640 * h / w) & !1).max(2));

    let mut src = ff::frame::Video::new(ff::format::Pixel::RGBZ, w, h);
    super::fill_rgbz(&mut src, w, h, rgba);
    let mut dst = ff::frame::Video::new(ff::format::Pixel::YUVJ420P, tw, th);
    let mut scaler = ff::software::scaling::Context::get(
        ff::format::Pixel::RGBZ,
        w,
        h,
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
    let dir = clip
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("thumbs");
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

#[path = "mux_aac.rs"]
mod aac;
pub(crate) use aac::AacTrack;
#[cfg(test)]
pub(crate) use aac::{encode_aac, encode_aac_with, silent, AacCoder};
use aac::encode_tracks;

#[cfg(test)]
thread_local! {
    /// Tests: the abort lands once this many packets are in the file (a real mux cut short).
    pub(crate) static ABORT_AFTER: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

fn abort_here(_written: u64) -> bool {
    #[cfg(test)]
    if ABORT_AFTER.with(|a| a.get()) == Some(_written) {
        return true;
    }
    false
}

#[cfg(test)]
#[path = "mux_tests.rs"]
pub(super) mod tests;
