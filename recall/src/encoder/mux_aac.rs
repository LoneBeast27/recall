//! The AAC half of clip assembly (moved out of `mux.rs`, no behaviour change): every
//! snapshotted track encoded at once, Media Foundation where it fits, else FFmpeg's encoder.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use ffmpeg_next as ff;

use super::super::saver::ABORTED;
use crate::audio::AudioPcm;

/// One audio track's AAC-encoded save window. The encoder context is kept so
/// the muxer can copy its codec parameters (incl. AudioSpecificConfig).
pub(crate) struct AacTrack {
    pub(super) rate: u32,
    /// Stream `title` + `handler_name` ("Game", "Microphone", "Voice chat").
    pub(super) title: &'static str,
    /// Saved in the clip's `edit_model::AUDIO_SOURCES_TAG`.
    pub(super) source: &'static str,
    pub(super) encoder: ff::encoder::Audio,
    /// (pts in 1/rate, packet payload)
    pub(super) packets: Vec<(i64, Vec<u8>)>,
    pub coder: AacCoder,
    /// CPU time of the encoding thread.
    pub cpu_ms: f64,
}

/// Encode every track at once, one thread each: the save waits for the longest track, not
/// the sum (2 tracks of 119 s took 5.3 s one after another, 2026-11-04). Each worker runs at
/// below-normal priority with EcoQoS, so a save mid-game stays off the game's cores.
pub(super) fn encode_tracks(audio: &[AudioPcm], abort: &AtomicBool) -> Result<Vec<AacTrack>, String> {
    let progress = super::super::save_progress::current();
    std::thread::scope(|scope| {
        let workers: Vec<_> = audio
            .iter()
            .map(|pcm| {
                let progress = progress.clone();
                scope.spawn(move || {
                    crate::thread_qos::eco_background();
                    let _entered = super::super::save_progress::enter(progress);
                    // A source above 0 dB can pass full scale: limit it, never a hard clip in the encode.
                    let limited = pcm.boosted.then(|| pcm.limited());
                    encode_aac(limited.as_ref().unwrap_or(pcm), abort).map_err(|e| format!("audio track '{}': {e}", pcm.name))
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

/// Media Foundation's encoder takes 16-bit PCM at 44.1 or 48 kHz only. A silent track (a muted
/// or unused mic writes zeros) goes to FFmpeg's encoder too: Media Foundation is constant-rate,
/// so silence costs the full 192 kb/s (about 3 MB per 2-minute track) against ~2 kb/s with
/// FFmpeg's. `RECALL_AAC=native` forces FFmpeg's encoder (comparisons).
fn mf_fits(pcm: &AudioPcm) -> bool {
    matches!(pcm.rate, 44_100 | 48_000) && !silent(&pcm.samples) && std::env::var("RECALL_AAC").map_or(true, |v| v != "native")
}

/// Every sample at or below 1e-6 (-120 dBFS): nothing an AAC frame would keep.
pub(crate) fn silent(samples: &[f32]) -> bool {
    samples.iter().all(|s| s.abs() <= 1e-6)
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
        super::super::save_progress::step();
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
        title: pcm.title,
        source: pcm.source,
        encoder: enc,
        packets,
        coder,
        cpu_ms: crate::thread_qos::thread_cpu_ms() - started_cpu,
    })
}
