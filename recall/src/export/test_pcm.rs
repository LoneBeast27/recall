//! Sample formats for the synthetic clips (`test_media`): any encoder's input format filled
//! with a tone, and any decoder's output read back as first-channel f32.

use ffmpeg_next as ff;
use ff::format::sample::Type;
use ff::format::Sample;

/// The input format `codec` takes, float first (most encoders take one).
pub(super) fn pick(codec: &ff::Codec) -> Sample {
    let offered: Vec<Sample> = codec.audio().ok().and_then(|a| a.formats()).map(|f| f.collect()).unwrap_or_default();
    let order = [
        Sample::F32(Type::Planar),
        Sample::F32(Type::Packed),
        Sample::I32(Type::Planar),
        Sample::I32(Type::Packed),
        Sample::I16(Type::Planar),
        Sample::I16(Type::Packed),
    ];
    order.into_iter().find(|s| offered.contains(s)).unwrap_or(Sample::F32(Type::Planar))
}

/// The layout for a channel count: 1 mono, 6 = 5.1, otherwise stereo.
pub(super) fn layout(channels: u16) -> ff::ChannelLayout {
    match channels {
        1 => ff::ChannelLayout::MONO,
        6 => ff::ChannelLayout::_5POINT1,
        _ => ff::ChannelLayout::STEREO,
    }
}

fn put(bytes: &mut [u8], at: usize, format: Sample, value: f32) {
    match format {
        Sample::F32(_) => bytes[at * 4..at * 4 + 4].copy_from_slice(&value.to_le_bytes()),
        Sample::I32(_) => bytes[at * 4..at * 4 + 4].copy_from_slice(&((value as f64 * i32::MAX as f64) as i32).to_le_bytes()),
        Sample::I16(_) => bytes[at * 2..at * 2 + 2].copy_from_slice(&((value * i16::MAX as f32) as i16).to_le_bytes()),
        other => panic!("no writer for {other:?}"),
    }
}

/// Fill `samples` frames of `frame` with `value(frame index)`: every channel of mono and stereo,
/// only front left and right of a surround layout (the rest silent), so a downmix keeps the level.
pub(super) fn fill(frame: &mut ff::frame::Audio, format: Sample, channels: usize, samples: usize, value: impl Fn(usize) -> f32) {
    let level = |channel: usize, n: usize| if channel < 2 { value(n) } else { 0.0 };
    if format.is_planar() {
        for channel in 0..channels {
            let plane = plane_mut(frame, channel);
            (0..samples).for_each(|n| put(plane, n, format, level(channel, n)));
        }
    } else {
        let plane = plane_mut(frame, 0);
        (0..samples).for_each(|n| (0..channels).for_each(|c| put(plane, n * channels + c, format, level(c, n))));
    }
}

/// Plane `index` as bytes. FFmpeg sets only `linesize[0]` for audio (every plane is that size),
/// so `Frame::data_mut` gives an empty slice past the first plane.
fn plane_mut(frame: &mut ff::frame::Audio, index: usize) -> &mut [u8] {
    // SAFETY: an allocated audio frame; each of its planes holds `linesize[0]` bytes.
    unsafe {
        let raw = frame.as_mut_ptr();
        std::slice::from_raw_parts_mut((*raw).data[index], (*raw).linesize[0] as usize)
    }
}

fn get(bytes: &[u8], at: usize, format: Sample) -> f32 {
    match format {
        Sample::F32(_) => f32::from_le_bytes(bytes[at * 4..at * 4 + 4].try_into().unwrap()),
        Sample::F64(_) => f64::from_le_bytes(bytes[at * 8..at * 8 + 8].try_into().unwrap()) as f32,
        Sample::I32(_) => (i32::from_le_bytes(bytes[at * 4..at * 4 + 4].try_into().unwrap()) as f64 / i32::MAX as f64) as f32,
        Sample::I16(_) => i16::from_le_bytes(bytes[at * 2..at * 2 + 2].try_into().unwrap()) as f32 / i16::MAX as f32,
        Sample::U8(_) => (bytes[at] as f32 - 128.0) / 128.0,
        other => panic!("no reader for {other:?}"),
    }
}

/// The first channel of a decoded frame as f32, whatever its sample format.
pub(super) fn first_channel(frame: &ff::frame::Audio) -> Vec<f32> {
    let (format, samples) = (frame.format(), frame.samples());
    let stride = if format.is_planar() { 1 } else { frame.channels().max(1) as usize };
    let plane = frame.data(0);
    (0..samples).map(|n| get(plane, n * stride, format)).collect()
}
