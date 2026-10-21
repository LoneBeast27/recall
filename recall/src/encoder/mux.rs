//! Clip assembly on the saver thread: AAC encode of the snapshotted PCM, MP4
//! stream-copy mux of the packet window, and the gallery thumbnail + Moments
//! row. Nothing here touches the live encoder, its ring, or the audio rings.

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
    encoder: ff::encoder::Audio,
    /// (pts in 1/rate, packet payload)
    packets: Vec<(i64, Vec<u8>)>,
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
    let (packets, params) = match &job.video {
        VideoSrc::Window { packets, params } => (packets.as_slice(), params),
        VideoSrc::Still {
            frame,
            fps,
            bitrate,
        } => {
            let started = Instant::now();
            still = super::encode_still(frame, *fps, *bitrate, job.seconds)?;
            timing.still_ms = ms(started.elapsed());
            (still.0.as_slice(), &still.1)
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
    let mut tracks = Vec::with_capacity(job.audio.len());
    for pcm in &job.audio {
        let track = encode_aac(pcm, abort).map_err(|e| format!("audio track '{}': {e}", pcm.name))?;
        tracks.push(track);
    }
    timing.aac_ms = ms(started.elapsed());

    let (mux_ms, trailer_ms) = mux_mp4(part, params, job.backend, packets, &tracks, abort)?;
    timing.mux_ms = mux_ms;
    timing.trailer_ms = trailer_ms;
    Ok(Muxed {
        frames: packets.len() as u64,
        secs: secs as f32,
    })
}

/// Encode one snapshotted track to AAC (stereo/mono, native rate).
pub(crate) fn encode_aac(pcm: &AudioPcm, abort: &AtomicBool) -> Result<AacTrack, String> {
    let samples = &pcm.samples;
    let src_ch = pcm.channels.max(1) as usize;
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
    ctx.set_rate(pcm.rate as i32);
    ctx.set_format(ff::format::Sample::F32(ff::format::sample::Type::Planar));
    ctx.set_channel_layout(layout);
    ctx.set_bit_rate(192_000);
    ctx.set_time_base(ff::Rational(1, pcm.rate as i32));
    ctx.set_flags(ff::codec::Flags::GLOBAL_HEADER);
    let mut enc = ctx.open().map_err(|e| format!("aac open: {e}"))?;

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
        let mut af = ff::frame::Audio::new(
            ff::format::Sample::F32(ff::format::sample::Type::Planar),
            frame_size,
            layout,
        );
        af.set_rate(pcm.rate);
        let src = &samples[pos * src_ch..(pos + n) * src_ch];
        for c in 0..ch {
            let plane = af.plane_mut::<f32>(c);
            for (dst, frame) in plane[..n].iter_mut().zip(src.chunks_exact(src_ch)) {
                *dst = frame[c];
            }
            plane[n..frame_size].fill(0.0);
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
        encoder: enc,
        packets,
    })
}

/// True when every PTS is strictly greater than the one before. The ring is
/// stream-copied with dts == pts, which is only valid without B-frames (bf=0).
pub(crate) fn pts_strictly_increase(video: &[EncodedPacket]) -> bool {
    video.windows(2).all(|w| w[0].frame_idx < w[1].frame_idx)
}

/// Mux a rebased video window (stream copy, 1/1_000_000) plus one AAC stream
/// per track into `path` as MP4. Returns (header+packets ms, trailer ms).
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
    for t in tracks {
        let mut ast = octx
            .add_stream(ff::encoder::find(ff::codec::Id::AAC))
            .map_err(|e| e.to_string())?;
        ast.set_parameters(&t.encoder);
        ast.set_time_base(ff::Rational(1, t.rate as i32));
    }
    octx.write_header()
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

/// Production `GalleryFn`: thumbnail + version-guarded Moments DB row.
/// Failures here never un-save the clip — they're reported and skipped.
pub(crate) fn finish_gallery(job: &SaveJob, secs: f32, bytes: u64, events: &Events) {
    if !crate::moments::enabled() {
        return;
    }
    let thumb = match job
        .thumb
        .as_ref()
        .ok_or_else(|| "no frame for thumbnail".to_string())
        .and_then(|frame| write_thumbnail(&job.path, frame))
    {
        Ok(t) => t,
        Err(e) => {
            events.log(&format!("thumbnail failed: {e}"));
            String::new()
        }
    };
    // Anchor to the request: the clip ends when the user asked for it.
    let requested_ago = chrono::Duration::from_std(job.requested.elapsed()).unwrap_or_default();
    let recorded = chrono::Local::now()
        - requested_ago
        - chrono::Duration::milliseconds((secs * 1000.0) as i64);
    let meta = crate::moments::ClipMeta {
        path: job.path.clone(),
        thumbnail_path: thumb,
        width: job.width,
        height: job.height,
        fps: job.fps,
        seconds: secs as f64,
        filesize: bytes,
        recorded_iso: recorded.format("%Y-%m-%dT%H:%M:%S%:z").to_string(),
        session_id: job.session_id.clone(),
        audio_tracks: job.audio.iter().map(|t| t.name.to_string()).collect(),
    };
    match crate::moments::insert_clip(&meta) {
        Ok(id) => events.log(&format!("moments: row inserted {id}")),
        Err(e) => events.log(&format!("moments: SKIPPED -- {e}")),
    }
}

/// Scale the request-time raw frame to ~640-wide and write a JPEG next to the
/// clip per Moments' convention: `thumbs\<stem>-<epoch>-thumb.jpg`.
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
