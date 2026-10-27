//! Synthetic clips for CPU-only export tests, shaped like Recall recordings:
//! H.264 (libx264 standing in for NVENC) in a microsecond time base with
//! jittered VFR timestamps and no B-frames, plus stereo AAC tracks.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use ff::Rescale;
use ffmpeg_next as ff;

const US: ff::Rational = ff::Rational(1, 1_000_000);
pub(super) const RATE: u32 = 48_000;

pub(super) struct Spec {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub seconds: f64,
    pub gop: u32,
    /// Audio tone frequencies, one stereo AAC track each.
    pub tones: Vec<f32>,
    /// Stamp audio packets like SteelSeries Moments: the true time snapped to a 100-tick
    /// grid, so 1024-sample frames sit 1000 or 1100 ticks apart (the 2026-10-27 choppy audio).
    pub moments_audio_stamps: bool,
}

/// Track names written like the recorder does: as the MP4 `handler_name`.
pub(super) const TRACK_NAMES: [&str; 2] = ["Game", "Microphone"];

impl Default for Spec {
    fn default() -> Self {
        Self { width: 320, height: 240, fps: 60, seconds: 5.0, gop: 30, tones: vec![440.0, 660.0], moments_audio_stamps: false }
    }
}

pub(super) struct Clip {
    pub dir: PathBuf,
    pub path: PathBuf,
}

impl Clip {
    pub fn output(&self, name: &str) -> String {
        self.dir.join(name).to_string_lossy().into_owned()
    }
}

impl Drop for Clip {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn xorshift(state: &mut u32) -> u8 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    (*state >> 24) as u8
}

/// Build a clip in a fresh temp dir; `None` when libx264 is missing.
pub(super) fn synth(tag: &str, spec: &Spec) -> Option<Clip> {
    ff::init().unwrap();
    let codec = ff::encoder::find_by_name("libx264")?;
    let dir = std::env::temp_dir().join(format!("recall-export-{tag}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("source.mp4");
    let mut octx = ff::format::output(&path).unwrap();

    let mut ctx = ff::codec::context::Context::new_with_codec(codec).encoder().video().unwrap();
    ctx.set_width(spec.width);
    ctx.set_height(spec.height);
    ctx.set_format(ff::format::Pixel::YUV420P);
    ctx.set_time_base(US);
    ctx.set_frame_rate(Some(ff::Rational(spec.fps as i32, 1)));
    ctx.set_gop(spec.gop);
    ctx.set_max_b_frames(0);
    ctx.set_bit_rate(4_000_000);
    ctx.set_flags(ff::codec::Flags::GLOBAL_HEADER);
    let mut options = ff::Dictionary::new();
    options.set("preset", "ultrafast");
    options.set("bf", "0");
    options.set("x264-params", &format!("keyint={0}:min-keyint={0}:scenecut=0", spec.gop));
    let mut video = ctx.open_with(options).unwrap();
    let video_index = {
        let mut stream = octx.add_stream(Some(codec)).unwrap();
        stream.set_parameters(&video);
        stream.set_time_base(US);
        stream.index()
    };

    let mut audio = Vec::new();
    for (ordinal, _) in spec.tones.iter().enumerate() {
        let aac = ff::encoder::find(ff::codec::Id::AAC).unwrap();
        let mut ctx = ff::codec::context::Context::new_with_codec(aac).encoder().audio().unwrap();
        ctx.set_rate(RATE as i32);
        ctx.set_format(ff::format::Sample::F32(ff::format::sample::Type::Planar));
        ctx.set_channel_layout(ff::ChannelLayout::STEREO);
        ctx.set_bit_rate(128_000);
        ctx.set_time_base(ff::Rational(1, RATE as i32));
        ctx.set_flags(ff::codec::Flags::GLOBAL_HEADER);
        let encoder = ctx.open().unwrap();
        let mut stream = octx.add_stream(Some(aac)).unwrap();
        stream.set_parameters(&encoder);
        stream.set_time_base(ff::Rational(1, RATE as i32));
        if let Some(name) = TRACK_NAMES.get(ordinal) {
            let mut metadata = ff::Dictionary::new();
            metadata.set("handler_name", name);
            stream.set_metadata(metadata);
        }
        audio.push((stream.index(), encoder));
    }
    octx.write_header().unwrap();

    // (time in seconds, output stream, packet in its encoder time base)
    let mut queue: Vec<(f64, usize, ff::Packet, ff::Rational)> = Vec::new();
    let frames = (spec.seconds * spec.fps as f64).round() as i64;
    let mut seed = 0x1234_5678u32;
    for i in 0..frames {
        let jitter = [0i64, 1_500, -1_200, 700][(i % 4) as usize];
        let pts = (i * 1_000_000 / spec.fps as i64 + if i == 0 { 0 } else { jitter }).max(0);
        let mut frame = ff::frame::Video::new(ff::format::Pixel::YUV420P, spec.width, spec.height);
        for plane in 0..3 {
            let stride = frame.stride(plane);
            let rows = if plane == 0 { spec.height } else { spec.height / 2 } as usize;
            let data = frame.data_mut(plane);
            for row in 0..rows {
                for x in 0..stride {
                    let wave = ((x + row + i as usize * 3) % 64) as u8 * 2;
                    data[row * stride + x] = wave.wrapping_add(xorshift(&mut seed) / 3);
                }
            }
        }
        frame.set_pts(Some(pts));
        video.send_frame(&frame).unwrap();
        drain_video(&mut video, video_index, &mut queue);
    }
    video.send_eof().unwrap();
    drain_video(&mut video, video_index, &mut queue);

    for ((index, encoder), tone) in audio.iter_mut().zip(&spec.tones) {
        let total = (spec.seconds * RATE as f64) as usize;
        let size = encoder.frame_size().max(1) as usize;
        let mut position = 0;
        while position < total {
            let count = size.min(total - position);
            let mut frame = ff::frame::Audio::new(
                ff::format::Sample::F32(ff::format::sample::Type::Planar),
                size,
                ff::ChannelLayout::STEREO,
            );
            frame.set_rate(RATE);
            frame.set_samples(count);
            frame.set_pts(Some(position as i64));
            for channel in 0..2 {
                let plane = frame.plane_mut::<f32>(channel);
                for (offset, sample) in plane.iter_mut().enumerate() {
                    let t = (position + offset) as f32 / RATE as f32;
                    *sample = if offset < count { (t * tone * std::f32::consts::TAU).sin() * 0.4 } else { 0.0 };
                }
            }
            encoder.send_frame(&frame).unwrap();
            drain_audio(encoder, *index, &mut queue);
            position += count;
        }
        encoder.send_eof().unwrap();
        drain_audio(encoder, *index, &mut queue);
    }

    queue.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let audio_streams: Vec<usize> = audio.iter().map(|(index, _)| *index).collect();
    for (_, stream, mut packet, tb) in queue {
        if spec.moments_audio_stamps && audio_streams.contains(&stream) {
            let snapped = |t: i64| (t + 50).div_euclid(100) * 100;
            let (pts, dts) = (packet.pts().map(snapped), packet.dts().map(snapped));
            packet.set_pts(pts);
            packet.set_dts(dts);
        }
        let out_tb = octx.stream(stream).unwrap().time_base();
        packet.rescale_ts(tb, out_tb);
        packet.set_stream(stream);
        packet.write_interleaved(&mut octx).unwrap();
    }
    octx.write_trailer().unwrap();
    Some(Clip { dir, path })
}

fn drain_video(encoder: &mut ff::encoder::Video, stream: usize, queue: &mut Vec<(f64, usize, ff::Packet, ff::Rational)>) {
    let mut packet = ff::Packet::empty();
    while encoder.receive_packet(&mut packet).is_ok() {
        let seconds = packet.dts().unwrap_or(0) as f64 / 1e6;
        queue.push((seconds, stream, packet.clone(), US));
    }
}

fn drain_audio(encoder: &mut ff::encoder::Audio, stream: usize, queue: &mut Vec<(f64, usize, ff::Packet, ff::Rational)>) {
    let mut packet = ff::Packet::empty();
    while encoder.receive_packet(&mut packet).is_ok() {
        let seconds = packet.dts().unwrap_or(0) as f64 / RATE as f64;
        queue.push((seconds, stream, packet.clone(), ff::Rational(1, RATE as i32)));
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct PacketInfo {
    pub pts_us: i64,
    pub key: bool,
    pub hash: u64,
}

pub(super) struct Probe {
    pub width: u32,
    pub height: u32,
    pub video: Vec<PacketInfo>,
    pub audio: Vec<Vec<PacketInfo>>,
    /// Decoded samples per audio track.
    pub audio_samples: Vec<usize>,
    /// Decoded first-channel PCM per audio track.
    pub audio_pcm: Vec<Vec<f32>>,
    /// `handler_name` tag per audio track as FFmpeg reads it back.
    pub audio_handlers: Vec<Option<String>>,
}

/// RMS and zero-crossing frequency (Hz) of the steady middle of `pcm`.
pub(super) fn tone(pcm: &[f32]) -> (f32, f64) {
    let middle = &pcm[pcm.len() / 8..pcm.len() * 7 / 8];
    let rms = (middle.iter().map(|s| s * s).sum::<f32>() / middle.len().max(1) as f32).sqrt();
    let crossings = middle.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count();
    (rms, crossings as f64 * RATE as f64 / middle.len().max(1) as f64 / 2.0)
}

fn hash(data: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    hasher.finish()
}

/// Packet hashes per stream plus decoded audio length (timestamps in µs).
pub(super) fn probe(path: &Path) -> Probe {
    let mut ictx = ff::format::input(path).unwrap();
    let video_stream = ictx.streams().best(ff::media::Type::Video).unwrap();
    let video_index = video_stream.index();
    let decoder = ff::codec::context::Context::from_parameters(video_stream.parameters())
        .unwrap().decoder().video().unwrap();
    let (width, height) = (decoder.width(), decoder.height());
    let audio_indices: Vec<usize> = ictx
        .streams()
        .filter(|s| s.parameters().medium() == ff::media::Type::Audio)
        .map(|s| s.index())
        .collect();
    let mut decoders: Vec<_> = audio_indices
        .iter()
        .map(|&i| {
            let params = ictx.stream(i).unwrap().parameters();
            ff::codec::context::Context::from_parameters(params).unwrap().decoder().audio().unwrap()
        })
        .collect();
    let audio_handlers = audio_indices
        .iter()
        .map(|&i| ictx.stream(i).unwrap().metadata().get("handler_name").map(str::to_owned))
        .collect();
    let time_bases: Vec<_> = ictx.streams().map(|s| s.time_base()).collect();
    let mut video = Vec::new();
    let mut audio = vec![Vec::new(); audio_indices.len()];
    let mut audio_samples = vec![0usize; audio_indices.len()];
    let mut audio_pcm = vec![Vec::new(); audio_indices.len()];
    for (stream, packet) in ictx.packets() {
        let info = PacketInfo {
            pts_us: packet.pts().unwrap_or(0).rescale(time_bases[stream.index()], US),
            key: packet.is_key(),
            hash: hash(packet.data().unwrap_or(&[])),
        };
        if stream.index() == video_index {
            video.push(info);
        } else if let Some(track) = audio_indices.iter().position(|&i| i == stream.index()) {
            audio[track].push(info);
            decoders[track].send_packet(&packet).unwrap();
            audio_samples[track] += drain_samples(&mut decoders[track], &mut audio_pcm[track]);
        }
    }
    for (track, decoder) in decoders.iter_mut().enumerate() {
        decoder.send_eof().unwrap();
        audio_samples[track] += drain_samples(decoder, &mut audio_pcm[track]);
    }
    Probe { width, height, video, audio, audio_samples, audio_pcm, audio_handlers }
}

fn drain_samples(decoder: &mut ff::decoder::Audio, pcm: &mut Vec<f32>) -> usize {
    let mut frame = ff::frame::Audio::empty();
    let mut total = 0;
    while decoder.receive_frame(&mut frame).is_ok() {
        assert_eq!(frame.format(), ff::format::Sample::F32(ff::format::sample::Type::Planar));
        pcm.extend_from_slice(&frame.plane::<f32>(0)[..frame.samples()]);
        total += frame.samples();
    }
    total
}
