//! A prepared stem read from disk as it plays: the mixer asks for the frames of each device
//! buffer when it fills it, so a long clip never sits decoded in memory. (Each stem used to be
//! decoded whole to f32 first, refused past 256 MiB in total: about 4 minutes of three stereo
//! tracks, and the preview failed.) FLAC in `audio_flac.rs`; WAV (stem caches before v6) here.
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use super::flac::{self, FlacStream};
use super::SAMPLE_RATE;

pub(super) enum StemReader {
    Wav(WavStream),
    Flac(Box<FlacStream>),
}

impl StemReader {
    pub(super) fn open(path: &str) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| format!("could not open preview stem '{path}': {e}"))?;
        let mut head = [0u8; 4];
        let is_flac = file.read_exact(&mut head).is_ok() && flac::is_flac(&head);
        file.seek(SeekFrom::Start(0)).map_err(|e| format!("could not read preview stem '{path}': {e}"))?;
        let stem = if is_flac { FlacStream::open(file, SAMPLE_RATE).map(|flac| StemReader::Flac(Box::new(flac))) } else { WavStream::open(file).map(StemReader::Wav) };
        stem.map_err(|e| format!("invalid preview stem '{path}': {e}"))
    }

    pub(super) fn channels(&self) -> usize {
        match self {
            StemReader::Wav(wav) => wav.channels,
            StemReader::Flac(flac) => flac.channels(),
        }
    }

    pub(super) fn frames(&self) -> usize {
        match self {
            StemReader::Wav(wav) => wav.frames,
            StemReader::Flac(flac) => flac.frames(),
        }
    }

    /// Interleaved samples of frames `[from, from + count)` into `out`; silence past the end.
    pub(super) fn read(&mut self, from: usize, count: usize, out: &mut Vec<f32>) -> Result<(), String> {
        match self {
            StemReader::Wav(wav) => wav.read(from, count, out),
            StemReader::Flac(flac) => flac.read(from, count, out),
        }
    }
}

/// 16-bit integer or 32-bit float PCM at 48 kHz, read where it lies in the file.
pub(super) struct WavStream {
    file: File,
    data_offset: u64,
    frames: usize,
    channels: usize,
    float: bool,
    bytes: Vec<u8>,
}

impl WavStream {
    fn open(mut file: File) -> Result<Self, String> {
        let length = file.metadata().map_err(|e| e.to_string())?.len();
        let (fmt, (data_offset, data_size)) = wav_chunks(&mut file, length)?;
        let (channels, float) = wav_format(&fmt)?;
        let width = if float { 4 } else { 2 };
        if data_size % width != 0 {
            return Err(format!("{}-bit PCM data is not sample-aligned", width * 8));
        }
        let samples = data_size / width;
        if samples % channels as u64 != 0 {
            return Err("PCM data is not frame-aligned".into());
        }
        let frames = usize::try_from(samples / channels as u64).map_err(|_| "WAV data is too large")?;
        Ok(Self { file, data_offset, frames, channels, float, bytes: Vec::new() })
    }

    fn read(&mut self, from: usize, count: usize, out: &mut Vec<f32>) -> Result<(), String> {
        out.clear();
        out.resize(count * self.channels, 0.0);
        if from >= self.frames {
            return Ok(());
        }
        let width = if self.float { 4 } else { 2 };
        let take = count.min(self.frames - from) * self.channels;
        self.bytes.resize(take * width, 0);
        let offset = self.data_offset + (from * self.channels * width) as u64;
        self.file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.file.read_exact(&mut self.bytes))
            .map_err(|e| format!("could not read preview stem: {e}"))?;
        for (slot, sample) in out.iter_mut().zip(self.bytes.chunks_exact(width)) {
            *slot = if self.float { f32::from_le_bytes(sample.try_into().unwrap()) } else { i16::from_le_bytes([sample[0], sample[1]]) as f32 / 32768.0 };
        }
        Ok(())
    }
}

/// The fmt chunk and the data chunk's (offset, size) of a RIFF/WAVE file `length` bytes long.
fn wav_chunks(file: &mut File, length: u64) -> Result<(Vec<u8>, (u64, u64)), String> {
    let mut header = [0u8; 12];
    if file.read_exact(&mut header).is_err() || &header[..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return Err("expected a RIFF/WAVE file".into());
    }
    let (mut offset, mut format, mut data) = (12u64, None, None);
    while offset + 8 <= length {
        let mut chunk = [0u8; 8];
        file.seek(SeekFrom::Start(offset)).and_then(|_| file.read_exact(&mut chunk)).map_err(|e| e.to_string())?;
        let size = u64::from(u32::from_le_bytes(chunk[4..8].try_into().unwrap()));
        let start = offset + 8;
        if start + size > length {
            return Err("truncated WAV chunk".into());
        }
        if &chunk[..4] == b"fmt " {
            let mut fmt = vec![0u8; size.min(64) as usize];
            file.read_exact(&mut fmt).map_err(|e| e.to_string())?;
            format = Some(fmt);
        } else if &chunk[..4] == b"data" {
            data = Some((start, size));
        }
        offset = start + size + (size & 1);
    }
    Ok((format.ok_or("missing fmt chunk")?, data.ok_or("missing data chunk")?))
}

/// (channels, 32-bit float) of a 48 kHz mono or stereo PCM fmt chunk.
fn wav_format(fmt: &[u8]) -> Result<(usize, bool), String> {
    if fmt.len() < 16 {
        return Err("truncated fmt chunk".into());
    }
    let mut tag = u16::from_le_bytes(fmt[0..2].try_into().unwrap());
    let channels = u16::from_le_bytes(fmt[2..4].try_into().unwrap()) as usize;
    let sample_rate = u32::from_le_bytes(fmt[4..8].try_into().unwrap());
    let bits = u16::from_le_bytes(fmt[14..16].try_into().unwrap());
    if tag == 0xfffe {
        if fmt.len() < 40 {
            return Err("truncated extensible fmt chunk".into());
        }
        tag = u16::from_le_bytes(fmt[24..26].try_into().unwrap());
    }
    if !(channels == 1 || channels == 2) {
        return Err(format!("expected mono or stereo PCM, got {channels} channels"));
    }
    if sample_rate != SAMPLE_RATE {
        return Err(format!("expected {SAMPLE_RATE} Hz prepared PCM, got {sample_rate} Hz"));
    }
    match (tag, bits) {
        (1, 16) => Ok((channels, false)),
        (3, 32) => Ok((channels, true)),
        _ => Err(format!("unsupported WAV encoding tag {tag}, {bits} bits")),
    }
}
