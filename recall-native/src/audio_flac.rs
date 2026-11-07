//! FLAC editor stems (the engine writes them from stem cache version 6; WAV before), read from
//! disk as they play: a window of decoded frames at the playhead, a seek when it jumps. The
//! whole stem never sits decoded in memory. Pure Rust (symphonia's FLAC bundle): no codec
//! needs to be installed on the machine.
use std::fs::File;

use symphonia_bundle_flac::{FlacDecoder, FlacReader};
use symphonia_core::audio::SampleBuffer;
use symphonia_core::codecs::{Decoder, DecoderOptions};
use symphonia_core::errors::Error;
use symphonia_core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia_core::io::MediaSourceStream;

/// A jump forward up to this many frames decodes through instead of seeking (1 s).
const DECODE_THROUGH: usize = 48_000;
/// A gap between packets longer than this is not silence to fill: the window starts again.
const MAX_GAP: usize = 10 * DECODE_THROUGH;

pub(crate) fn is_flac(head: &[u8]) -> bool {
    head.starts_with(b"fLaC")
}

pub(crate) struct FlacStream {
    reader: FlacReader,
    decoder: FlacDecoder,
    channels: usize,
    frames: usize,
    buffer: Option<SampleBuffer<f32>>,
    /// Decoded interleaved samples, from frame `start`.
    window: Vec<f32>,
    start: usize,
    /// No packets after the window.
    ended: bool,
}

impl FlacStream {
    /// A mono or stereo FLAC at `rate`.
    pub(crate) fn open(file: File, rate: u32) -> Result<Self, String> {
        let source = MediaSourceStream::new(Box::new(file), Default::default());
        let reader = FlacReader::try_new(source, &FormatOptions::default()).map_err(|e| format!("read FLAC: {e}"))?;
        let params = reader.default_track().ok_or("FLAC has no audio")?.codec_params.clone();
        let channels = params.channels.map_or(0, |c| c.count());
        if !(channels == 1 || channels == 2) {
            return Err(format!("expected mono or stereo FLAC, got {channels} channels"));
        }
        if params.sample_rate != Some(rate) {
            return Err(format!("expected {rate} Hz FLAC, got {:?} Hz", params.sample_rate));
        }
        let decoder = FlacDecoder::try_new(&params, &DecoderOptions::default()).map_err(|e| format!("FLAC decoder: {e}"))?;
        let frames = params.n_frames.unwrap_or(0) as usize;
        Ok(Self { reader, decoder, channels, frames, buffer: None, window: Vec::new(), start: 0, ended: false })
    }

    pub(crate) fn channels(&self) -> usize {
        self.channels
    }

    /// Frames in the stem (0 when the file does not say).
    pub(crate) fn frames(&self) -> usize {
        self.frames
    }

    fn end(&self) -> usize {
        self.start + self.window.len() / self.channels
    }

    /// Interleaved samples of frames `[from, from + count)` into `out`; silence where the stem
    /// has none.
    pub(crate) fn read(&mut self, from: usize, count: usize, out: &mut Vec<f32>) -> Result<(), String> {
        let ch = self.channels;
        if from < self.start || from > self.end() + DECODE_THROUGH {
            self.seek(from)?;
        }
        while self.end() < from + count && !self.ended {
            self.next_packet()?;
        }
        // Frames before `from` are not needed again (a seek back decodes them anew).
        if from > self.start {
            let drop = ((from - self.start) * ch).min(self.window.len());
            self.window.drain(..drop);
            self.start += drop / ch;
        }
        out.clear();
        out.resize(count * ch, 0.0);
        // A seek that lands past `from` (a damaged file) leaves silence before the window.
        let lead = (self.start.saturating_sub(from) * ch).min(out.len());
        let have = self.window.len().min(out.len() - lead);
        out[lead..lead + have].copy_from_slice(&self.window[..have]);
        Ok(())
    }

    fn seek(&mut self, to: usize) -> Result<(), String> {
        self.window.clear();
        self.decoder.reset();
        self.start = to;
        match self.reader.seek(SeekMode::Accurate, SeekTo::TimeStamp { ts: to as u64, track_id: 0 }) {
            Ok(seeked) => {
                self.start = seeked.actual_ts as usize;
                self.ended = false;
                Ok(())
            }
            // Past the last frame: silence from here.
            Err(Error::SeekError(_)) => {
                self.ended = true;
                Ok(())
            }
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                self.ended = true;
                Ok(())
            }
            Err(e) => Err(format!("seek FLAC: {e}")),
        }
    }

    /// Decodes the next packet onto the window (an undecodable frame is skipped: its time
    /// plays as silence).
    fn next_packet(&mut self) -> Result<(), String> {
        let packet = match self.reader.next_packet() {
            Ok(packet) => packet,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                self.ended = true;
                return Ok(());
            }
            Err(e) => return Err(format!("read FLAC: {e}")),
        };
        let decoded = match self.decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(Error::DecodeError(_)) => return Ok(()),
            Err(e) => return Err(format!("decode FLAC: {e}")),
        };
        let needed = decoded.capacity() * self.channels;
        if self.buffer.as_ref().is_none_or(|b| b.capacity() < needed) {
            self.buffer = Some(SampleBuffer::new(decoded.capacity() as u64, *decoded.spec()));
        }
        let buffer = self.buffer.as_mut().expect("set above");
        buffer.copy_interleaved_ref(decoded);
        let samples = buffer.samples();
        let at = packet.ts() as usize;
        let end = self.start + self.window.len() / self.channels;
        if self.window.is_empty() || at > end + MAX_GAP || at + MAX_GAP < end {
            self.window.clear();
            self.start = at;
        } else if at > end {
            self.window.resize(self.window.len() + (at - end) * self.channels, 0.0);
        }
        let end = self.start + self.window.len() / self.channels;
        let overlap = (end.saturating_sub(at) * self.channels).min(samples.len());
        self.window.extend_from_slice(&samples[overlap..]);
        Ok(())
    }
}

#[cfg(test)]
#[path = "audio_flac_tests.rs"]
mod tests;
