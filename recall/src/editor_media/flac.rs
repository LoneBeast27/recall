//! Editor preview stems as FLAC: lossless, so the editor hears exactly what the 16-bit WAV it
//! replaces held, at well under its size. Samples arrive in any amounts (decoded frames, gap
//! silence); whole encoder frames go out, the remainder with the trailer.

use std::path::Path;

use ffmpeg_next as ff;

use super::PREVIEW_RATE;

const PACKED_I16: ff::format::Sample = ff::format::Sample::I16(ff::format::sample::Type::Packed);

pub(super) struct FlacWriter {
    out: ff::format::context::Output,
    encoder: ff::encoder::Audio,
    stream: usize,
    channels: u16,
    layout: ff::ChannelLayout,
    /// Samples per channel in each encoder frame.
    frame_size: usize,
    /// Interleaved samples not yet sent.
    pending: Vec<i16>,
    /// Samples per channel sent so far (the next frame's pts).
    sent: i64,
}

impl FlacWriter {
    pub(super) fn create(path: &Path, channels: u16) -> Result<Self, String> {
        let codec = ff::encoder::find(ff::codec::Id::FLAC).ok_or("this FFmpeg build has no FLAC encoder")?;
        let layout = if channels == 1 { ff::ChannelLayout::MONO } else { ff::ChannelLayout::STEREO };
        let mut out = ff::format::output_as(path, "flac").map_err(|e| format!("create stem {}: {e}", path.display()))?;
        let mut context = ff::codec::context::Context::new_with_codec(codec).encoder().audio().map_err(|e| format!("FLAC encoder: {e}"))?;
        context.set_rate(PREVIEW_RATE as i32);
        context.set_format(PACKED_I16);
        context.set_channel_layout(layout);
        context.set_time_base(ff::Rational(1, PREVIEW_RATE as i32));
        let encoder = context.open().map_err(|e| format!("open FLAC encoder: {e}"))?;
        let stream = {
            let mut stream = out.add_stream(Some(codec)).map_err(|e| format!("stem stream: {e}"))?;
            stream.set_parameters(&encoder);
            stream.set_time_base(ff::Rational(1, PREVIEW_RATE as i32));
            stream.index()
        };
        out.write_header().map_err(|e| format!("write stem header: {e}"))?;
        let frame_size = if encoder.frame_size() > 0 { encoder.frame_size() as usize } else { 4096 };
        Ok(Self { out, encoder, stream, channels, layout, frame_size, pending: Vec::new(), sent: 0 })
    }

    pub(super) fn samples(&mut self, samples: &[i16]) -> Result<(), String> {
        self.pending.extend_from_slice(samples);
        self.flush_full()
    }

    pub(super) fn silence(&mut self, frames: u64) -> Result<(), String> {
        let mut left = frames.saturating_mul(u64::from(self.channels));
        while left > 0 {
            let step = left.min((self.frame_size * usize::from(self.channels)) as u64) as usize;
            self.pending.resize(self.pending.len() + step, 0);
            left -= step as u64;
            self.flush_full()?;
        }
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<(), String> {
        let rest = self.pending.len() / usize::from(self.channels);
        if rest > 0 {
            self.emit(rest)?;
        }
        self.encoder.send_eof().map_err(|e| format!("finish stem: {e}"))?;
        self.drain()?;
        self.out.write_trailer().map_err(|e| format!("write stem trailer: {e}"))
    }

    fn flush_full(&mut self) -> Result<(), String> {
        while self.pending.len() >= self.frame_size * usize::from(self.channels) {
            self.emit(self.frame_size)?;
        }
        Ok(())
    }

    /// Sends the first `count` samples per channel of `pending` as one frame.
    fn emit(&mut self, count: usize) -> Result<(), String> {
        let values = count * usize::from(self.channels);
        let mut frame = ff::frame::Audio::new(PACKED_I16, count, self.layout);
        frame.set_rate(PREVIEW_RATE);
        frame.set_pts(Some(self.sent));
        let bytes = frame.data_mut(0);
        for (slot, sample) in bytes.chunks_exact_mut(2).zip(&self.pending[..values]) {
            slot.copy_from_slice(&sample.to_le_bytes());
        }
        self.encoder.send_frame(&frame).map_err(|e| format!("encode stem: {e}"))?;
        self.pending.drain(..values);
        self.sent += count as i64;
        self.drain()
    }

    fn drain(&mut self) -> Result<(), String> {
        let mut packet = ff::Packet::empty();
        while self.encoder.receive_packet(&mut packet).is_ok() {
            // The end-of-stream packet only carries an updated header (no audio); the muxer
            // refuses it, and a stem without the final sample count still decodes in full.
            if packet.size() == 0 {
                continue;
            }
            let time_base = self.out.stream(self.stream).map(|s| s.time_base()).unwrap_or(ff::Rational(1, PREVIEW_RATE as i32));
            packet.set_stream(self.stream);
            packet.rescale_ts(ff::Rational(1, PREVIEW_RATE as i32), time_base);
            packet.write_interleaved(&mut self.out).map_err(|e| format!("write stem: {e}"))?;
        }
        Ok(())
    }
}

/// A stem's interleaved samples and channel count (tests, and anything that needs raw PCM).
#[allow(dead_code)]
pub(crate) fn read_stem(path: &Path) -> Result<(Vec<i16>, u16), String> {
    let mut input = ff::format::input(path).map_err(|e| format!("open stem {}: {e}", path.display()))?;
    let stream = input.streams().best(ff::media::Type::Audio).ok_or("stem has no audio")?;
    let index = stream.index();
    let mut decoder = ff::codec::context::Context::from_parameters(stream.parameters())
        .and_then(|c| c.decoder().audio())
        .map_err(|e| format!("stem decoder: {e}"))?;
    let channels = decoder.channels();
    let mut samples = Vec::new();
    let mut frame = ff::frame::Audio::empty();
    let mut take = |decoder: &mut ff::decoder::Audio, samples: &mut Vec<i16>| {
        while decoder.receive_frame(&mut frame).is_ok() {
            let values = frame.samples() * usize::from(channels);
            samples.extend(frame.data(0).chunks_exact(2).take(values).map(|b| i16::from_le_bytes([b[0], b[1]])));
        }
    };
    for (stream, packet) in input.packets() {
        if stream.index() == index {
            decoder.send_packet(&packet).map_err(|e| format!("decode stem: {e}"))?;
            take(&mut decoder, &mut samples);
        }
    }
    decoder.send_eof().map_err(|e| format!("decode stem: {e}"))?;
    take(&mut decoder, &mut samples);
    Ok((samples, channels))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Samples in odd amounts and gap silence come back exactly, in order.
    #[test]
    fn a_flac_stem_round_trips_its_samples() {
        ff::init().unwrap();
        ff::log::set_level(ff::log::Level::Warning);
        let dir = std::env::temp_dir().join(format!("recall-flac-stem-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audio-0.flac");
        let tone: Vec<i16> = (0..30_001).map(|i| ((i as f32 * 0.05).sin() * 9000.0) as i16).collect();
        let mut writer = FlacWriter::create(&path, 1).unwrap();
        writer.samples(&tone[..7]).unwrap();
        writer.silence(5_000).unwrap();
        writer.samples(&tone[7..]).unwrap();
        writer.finish().unwrap();
        let (decoded, channels) = read_stem(&path).unwrap();
        assert_eq!(channels, 1);
        let mut expected = tone[..7].to_vec();
        expected.extend(std::iter::repeat_n(0i16, 5_000));
        expected.extend_from_slice(&tone[7..]);
        assert_eq!(decoded, expected);
        assert!(std::fs::metadata(&path).unwrap().len() < (expected.len() * 2) as u64, "smaller than 16-bit PCM");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
