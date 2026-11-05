//! Clip assembly on the saver thread: AAC encode of the snapshotted PCM, MP4
//! stream-copy mux of the packet window, and the gallery thumbnail. Nothing here touches the live encoder, its ring, or the audio rings.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Instant;

use ffmpeg_next as ff;

use super::saver::{ms, window_secs, Events, Muxed, RawFrame, SaveJob, Timing, VideoSrc, ABORTED};
use crate::audio::AudioPcm;
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
}

/// One audio track's AAC-encoded save window. The encoder context is kept so
/// the muxer can copy its codec parameters (incl. AudioSpecificConfig).
pub(crate) struct AacTrack {
    rate: u32,
    /// Stream `title` + `handler_name` ("Game", "Microphone", "Voice chat").
    title: &'static str,
    /// Saved in the clip's `edit_model::AUDIO_SOURCES_TAG`.
    source: &'static str,
    encoder: ff::encoder::Audio,
    /// (pts in 1/rate, packet payload)
    packets: Vec<(i64, Vec<u8>)>,
    pub coder: AacCoder,
    /// CPU time of the encoding thread.
    pub cpu_ms: f64,
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

    let (mux_ms, trailer_ms) = mux_mp4(part, params, backend, packets, &tracks, abort)?;
    timing.mux_ms = mux_ms;
    timing.trailer_ms = trailer_ms;
    Ok(Muxed {
        frames: packets.len() as u64,
        secs: secs as f32,
    })
}

/// Encode every track at once, one thread each: the save waits for the longest track, not
/// the sum (2 tracks of 119 s took 5.3 s one after another, 2026-11-04). Each worker runs at
/// below-normal priority with EcoQoS, so a save mid-game stays off the game's cores.
fn encode_tracks(audio: &[AudioPcm], abort: &AtomicBool) -> Result<Vec<AacTrack>, String> {
    std::thread::scope(|scope| {
        let workers: Vec<_> = audio
            .iter()
            .map(|pcm| {
                scope.spawn(move || {
                    crate::thread_qos::eco_background();
                    encode_aac(pcm, abort).map_err(|e| format!("audio track '{}': {e}", pcm.name))
                })
            })
            .collect();
        workers.into_iter().map(|w| w.join().unwrap_or_else(|_| Err("audio encode thread panicked".into()))).collect()
    })
}

/// Which AAC encoder made a track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AacCoder {
    /// Windows' own AAC encoder (Media Foundation, through FFmpeg's `aac_mf`).
    MediaFoundation,
    /// FFmpeg's native encoder, fast coder: any rate and channel count.
    Native,
}

impl AacCoder {
    pub fn name(self) -> &'static str {
        match self {
            AacCoder::MediaFoundation => "mf",
            AacCoder::Native => "native",
        }
    }
}

/// Media Foundation's encoder takes 16-bit PCM at 44.1 or 48 kHz only. `RECALL_AAC=native`
/// forces FFmpeg's encoder (comparisons).
fn mf_fits(pcm: &AudioPcm) -> bool {
    matches!(pcm.rate, 44_100 | 48_000) && std::env::var("RECALL_AAC").map_or(true, |v| v != "native")
}

/// Encode one snapshotted track to AAC: Media Foundation where it fits and opens, else
/// FFmpeg's encoder.
pub(crate) fn encode_aac(pcm: &AudioPcm, abort: &AtomicBool) -> Result<AacTrack, String> {
    if mf_fits(pcm) {
        match encode_aac_with(pcm, abort, AacCoder::MediaFoundation) {
            Ok(track) => return Ok(track),
            Err(e) if e == ABORTED => return Err(e),
            Err(_) => {} // no Media Foundation (Windows N without the Media Feature Pack)
        }
    }
    encode_aac_with(pcm, abort, AacCoder::Native)
}

/// Encode one snapshotted track to AAC (stereo/mono, native rate) with `coder`.
/// The native encoder uses the fast coder: FFmpeg's default two-loop search costs several
/// times the CPU for no audible gain at 192 kb/s.
pub(crate) fn encode_aac_with(pcm: &AudioPcm, abort: &AtomicBool, coder: AacCoder) -> Result<AacTrack, String> {
    let started_cpu = crate::thread_qos::thread_cpu_ms();
    let samples = &pcm.samples;
    let src_ch = pcm.channels.max(1) as usize;
    let ch = src_ch.min(2); // fold >stereo down by taking the first two channels
    let layout = if ch == 1 {
        ff::ChannelLayout::MONO
    } else {
        ff::ChannelLayout::STEREO
    };
    let (codec, format) = match coder {
        AacCoder::MediaFoundation => (
            ff::encoder::find_by_name("aac_mf").ok_or("aac_mf encoder missing")?,
            ff::format::Sample::I16(ff::format::sample::Type::Packed),
        ),
        AacCoder::Native => (
            ff::encoder::find(ff::codec::Id::AAC).ok_or("aac encoder missing")?,
            ff::format::Sample::F32(ff::format::sample::Type::Planar),
        ),
    };
    let mut ctx = ff::codec::context::Context::new_with_codec(codec)
        .encoder()
        .audio()
        .map_err(|e| e.to_string())?;
    ctx.set_rate(pcm.rate as i32);
    ctx.set_format(format);
    ctx.set_channel_layout(layout);
    ctx.set_bit_rate(192_000);
    ctx.set_time_base(ff::Rational(1, pcm.rate as i32));
    ctx.set_flags(ff::codec::Flags::GLOBAL_HEADER);
    let mut opts = ff::Dictionary::new();
    if coder == AacCoder::Native {
        opts.set("aac_coder", "fast");
    }
    let mut enc = ctx.open_with(opts).map_err(|e| format!("aac open ({}): {e}", coder.name()))?;

    let frame_size = if enc.frame_size() > 0 {
        enc.frame_size() as usize
    } else {
        1024
    };
    let total_frames = samples.len() / src_ch;
    let mut packets = Vec::new();
    let mut pkt = ff::Packet::empty();
    let mut pos = 0usize; // in sample-frames
    while pos < total_frames {
        if abort.load(Relaxed) {
            return Err(ABORTED.into());
        }
        let n = frame_size.min(total_frames - pos);
        let mut af = ff::frame::Audio::new(format, frame_size, layout);
        af.set_rate(pcm.rate);
        let src = &samples[pos * src_ch..(pos + n) * src_ch];
        match coder {
            AacCoder::Native => {
                for c in 0..ch {
                    let plane = af.plane_mut::<f32>(c);
                    for (dst, frame) in plane[..n].iter_mut().zip(src.chunks_exact(src_ch)) {
                        *dst = frame[c];
                    }
                    plane[n..frame_size].fill(0.0);
                }
            }
            AacCoder::MediaFoundation => {
                // Packed: one buffer of interleaved i16, addressed as raw bytes.
                let data = af.data_mut(0);
                let out: &mut [i16] = bytemuck::cast_slice_mut(&mut data[..frame_size * ch * 2]);
                for (dst, frame) in out.chunks_exact_mut(ch).zip(src.chunks_exact(src_ch)) {
                    for c in 0..ch {
                        dst[c] = (frame[c].clamp(-1.0, 1.0) * 32767.0).round() as i16;
                    }
                }
                out[n * ch..].fill(0);
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
        rate: pcm.rate,
        title: crate::audio::track_title(pcm.name),
        source: pcm.source,
        encoder: enc,
        packets,
        coder,
        cpu_ms: crate::thread_qos::thread_cpu_ms() - started_cpu,
    })
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
/// per track into `path` as MP4. Returns (header+packets ms, trailer ms).
/// Codec-agnostic: the codec comes from `params` (H.264, HEVC or AV1).
pub(crate) fn mux_mp4(
    path: &Path,
    params: &OwnedParams,
    backend: &str,
    video: &[EncodedPacket],
    tracks: &[AacTrack],
    abort: &AtomicBool,
) -> Result<(f64, f64), String> {
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
    let mut merged: Vec<(usize, f64, ff::Rational, i64, i64, &[u8], bool)> = Vec::new();
    for p in video {
        let t = p.frame_idx as f64 / 1_000_000.0;
        merged.push((
            0,
            t,
            vtb,
            p.frame_idx as i64,
            p.duration_us as i64,
            &p.data,
            p.keyframe,
        ));
    }
    for (i, t) in tracks.iter().enumerate() {
        let atb = ff::Rational(1, t.rate as i32);
        for (pts, data) in &t.packets {
            merged.push((i + 1, *pts as f64 / t.rate as f64, atb, *pts, 0, data, true));
        }
    }
    merged.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    // Time bases are final only after write_header; rescale against those.
    let out_tbs: Vec<ff::Rational> = octx.streams().map(|s| s.time_base()).collect();
    for (stream_idx, _, src_tb, pts, duration, data, key) in merged {
        if abort.load(Relaxed) {
            return Err(ABORTED.into());
        }
        let mut pkt = ff::Packet::copy(data);
        pkt.set_pts(Some(pts));
        pkt.set_dts(Some(pts));
        pkt.set_duration(duration);
        if key {
            pkt.set_flags(ff::packet::Flags::KEY);
        }
        pkt.set_stream(stream_idx);
        pkt.rescale_ts(src_tb, out_tbs[stream_idx]);
        pkt.write_interleaved(&mut octx)
            .map_err(|e| e.to_string())?;
    }
    let mux_ms = ms(started.elapsed());
    let trailer = Instant::now();
    octx.write_trailer().map_err(|e| e.to_string())?;
    Ok((mux_ms, ms(trailer.elapsed())))
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

#[cfg(test)]
#[path = "mux_tests.rs"]
mod tests;
