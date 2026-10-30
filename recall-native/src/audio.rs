//! Preview audio: prepared WAV stems mixed on a worker thread into WinMM wave-out.
//! Device/buffers in `audio_wave.rs`, the worker loop in `audio_worker.rs`.
use std::cell::Cell;
use std::fs::{self, File};
use std::io::Read;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};

use windows::Win32::Media::Audio::{HWAVEOUT, WAVEHDR};
use windows::Win32::Media::MMSYSERR_NOERROR;

use super::media::PreviewTrack;

#[path = "audio_wave.rs"]
mod wave;
#[path = "audio_worker.rs"]
mod worker;
use worker::worker_main;

const SAMPLE_RATE: u32 = 48_000;
const OUTPUT_CHANNELS: usize = 2;
const FRAMES_PER_BUFFER: usize = 960; // 20 ms
const BUFFER_COUNT: usize = 4;
const MAX_DECODED_BYTES: usize = 256 * 1024 * 1024;

#[derive(Default)]
struct SharedStatus {
    ready: AtomicBool,
    playing: AtomicBool,
    position_bits: AtomicU64,
    generation: AtomicU64,
    /// Times the device ran out of queued sound while the clip still had more (a gap heard).
    underruns: AtomicU64,
    /// Commands the worker has carried out (the mixer counts the ones it sent).
    applied: AtomicU64,
    /// Length of the clock (the clip, padded past shorter stems) in frames.
    duration_frames: AtomicU64,
    error: Mutex<Option<String>>,
}

impl SharedStatus {
    fn position(&self) -> f64 {
        f64::from_bits(self.position_bits.load(Ordering::Relaxed))
    }

    fn set_position(&self, value: f64) {
        self.position_bits.store(value.to_bits(), Ordering::Relaxed);
    }

    fn fail_if_current(&self, generation: u64, message: impl Into<String>) {
        if self.generation.load(Ordering::Acquire) != generation {
            return;
        }
        self.ready.store(false, Ordering::Release);
        self.playing.store(false, Ordering::Release);
        *self.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(message.into());
    }
}

enum Command {
    Open {
        generation: u64,
        tracks: Vec<PreviewTrack>,
        /// The clip's length: the clock runs this long even when the stems are shorter.
        duration: f64,
    },
    Close {
        generation: u64,
    },
    Play,
    Pause,
    Seek(f64),
    SetAudio {
        stream_index: usize,
        gain_db: f64,
        muted: bool,
    },
    Shutdown,
}

pub(crate) struct AudioMixer {
    tx: mpsc::Sender<Command>,
    shared: Arc<SharedStatus>,
    worker: Option<JoinHandle<()>>,
    /// Commands sent so far, and the command numbers of the newest seek and play/pause with
    /// their targets: until the worker has carried one out, the clock reads as if it had (a
    /// play pressed right after a seek must start from the seek, not from the old time).
    sent: Cell<u64>,
    seek_sent: Cell<(u64, f64)>,
    run_sent: Cell<(u64, bool)>,
}

impl AudioMixer {
    pub(crate) fn new() -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(SharedStatus::default());
        let thread_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("recall-preview-audio".into())
            .spawn(move || worker_main(rx, thread_shared))
            .map_err(|e| format!("could not start preview audio worker: {e}"))?;
        Ok(Self {
            tx,
            shared,
            worker: Some(worker),
            sent: Cell::new(0),
            seek_sent: Cell::new((0, 0.0)),
            run_sent: Cell::new((0, false)),
        })
    }

    pub(crate) fn open(&self, tracks: &[PreviewTrack], duration: f64) -> Result<(), String> {
        let generation = self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.shared.ready.store(false, Ordering::Release);
        self.shared.playing.store(false, Ordering::Release);
        self.shared.set_position(0.0);
        self.shared.duration_frames.store(0, Ordering::Release);
        *self.shared.error.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let sent = self.send(Command::Open {
            generation,
            tracks: tracks.to_vec(),
            duration,
        });
        self.seek_sent.set((self.sent.get(), 0.0));
        self.run_sent.set((self.sent.get(), false));
        sent
    }

    pub(crate) fn close(&self) -> Result<(), String> {
        let generation = self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.shared.ready.store(false, Ordering::Release);
        self.shared.playing.store(false, Ordering::Release);
        self.shared.set_position(0.0);
        self.shared.duration_frames.store(0, Ordering::Release);
        *self.shared.error.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let sent = self.send(Command::Close { generation });
        self.seek_sent.set((self.sent.get(), 0.0));
        self.run_sent.set((self.sent.get(), false));
        sent
    }

    pub(crate) fn play(&self) -> Result<(), String> {
        self.require_ready()?;
        self.send(Command::Play)?;
        let runs = self.position() < self.duration() - 0.5 / SAMPLE_RATE as f64;
        self.run_sent.set((self.sent.get(), runs));
        Ok(())
    }

    pub(crate) fn pause(&self) -> Result<(), String> {
        self.send(Command::Pause)?;
        self.run_sent.set((self.sent.get(), false));
        Ok(())
    }

    pub(crate) fn seek(&self, seconds: f64) -> Result<(), String> {
        if !seconds.is_finite() || seconds < 0.0 {
            return Err("preview seek must be a finite non-negative time".into());
        }
        self.require_ready()?;
        let running = self.is_playing();
        self.send(Command::Seek(seconds))?;
        let target = seconds.min(self.duration());
        self.seek_sent.set((self.sent.get(), target));
        // A seek to the very end stops the sound; anywhere else it keeps its state.
        self.run_sent.set((self.sent.get(), running && target < self.duration()));
        Ok(())
    }

    /// Length of the clock in seconds (known once the stems are loaded).
    pub(crate) fn duration(&self) -> f64 {
        self.shared.duration_frames.load(Ordering::Acquire) as f64 / SAMPLE_RATE as f64
    }

    /// The worker has not yet carried out command number `seq`.
    fn pending(&self, seq: u64) -> bool {
        self.shared.applied.load(Ordering::Acquire) < seq
    }

    pub(crate) fn set_audio(
        &self,
        stream_index: usize,
        gain_db: f64,
        muted: bool,
    ) -> Result<(), String> {
        if !gain_db.is_finite() {
            return Err("preview track gain must be finite".into());
        }
        self.require_ready()?;
        self.send(Command::SetAudio {
            stream_index,
            gain_db,
            muted,
        })
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.shared.ready.load(Ordering::Acquire)
    }

    pub(crate) fn is_playing(&self) -> bool {
        let (seq, runs) = self.run_sent.get();
        if self.pending(seq) { runs } else { self.shared.playing.load(Ordering::Acquire) }
    }

    pub(crate) fn position(&self) -> f64 {
        let (seq, target) = self.seek_sent.get();
        if self.pending(seq) { target } else { self.shared.position() }
    }

    /// Output underruns since the mixer was created (test trace, player log).
    pub(crate) fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Relaxed)
    }

    pub(crate) fn error(&self) -> Option<String> {
        self.shared
            .error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn require_ready(&self) -> Result<(), String> {
        if let Some(error) = self.error() {
            return Err(error);
        }
        if !self.is_ready() {
            return Err("preview audio is still loading".into());
        }
        Ok(())
    }

    fn send(&self, command: Command) -> Result<(), String> {
        self.tx
            .send(command)
            .map_err(|_| "preview audio worker stopped".to_string())?;
        self.sent.set(self.sent.get() + 1);
        Ok(())
    }
}

impl Drop for AudioMixer {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct LoadedTrack {
    stream_index: usize,
    samples: Vec<f32>,
    channels: usize,
    gain_db: f64,
    muted: bool,
}

struct WaveBuffer {
    samples: Vec<i16>,
    header: Box<WAVEHDR>,
    prepared: bool,
}

struct WaveDevice {
    handle: HWAVEOUT,
    buffers: Vec<WaveBuffer>,
}

fn load_tracks(specs: &[PreviewTrack]) -> Result<Vec<LoadedTrack>, String> {
    if specs.is_empty() {
        return Err("preview requires at least one prepared audio stem".into());
    }
    let mut loaded = Vec::new();
    loaded
        .try_reserve_exact(specs.len())
        .map_err(|_| "not enough memory for preview track metadata")?;
    let mut decoded_bytes = 0usize;
    for spec in specs {
        let bytes = read_bounded(&spec.path)?;
        let remaining = MAX_DECODED_BYTES.saturating_sub(decoded_bytes);
        let (samples, channels) = parse_wav(&bytes, remaining)
            .map_err(|e| format!("invalid preview stem '{}': {e}", spec.path))?;
        decoded_bytes = decoded_bytes
            .checked_add(samples.len() * size_of::<f32>())
            .ok_or("preview decoded-audio size overflow")?;
        loaded.push(LoadedTrack {
            stream_index: spec.stream_index,
            samples,
            channels,
            gain_db: spec.gain_db,
            muted: spec.muted,
        });
    }
    Ok(loaded)
}

fn read_bounded(path: &str) -> Result<Vec<u8>, String> {
    let metadata =
        fs::metadata(path).map_err(|e| format!("could not inspect preview stem '{path}': {e}"))?;
    let length = usize::try_from(metadata.len())
        .map_err(|_| format!("preview stem is too large for this process: '{path}'"))?;
    if length > MAX_DECODED_BYTES {
        return Err(format!(
            "preview stem exceeds the 256 MiB pilot limit: '{path}'"
        ));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| format!("not enough memory to load preview stem '{path}'"))?;
    bytes.resize(length, 0);
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|e| format!("could not read preview stem '{path}': {e}"))?;
    Ok(bytes)
}

fn parse_wav(bytes: &[u8], decoded_budget: usize) -> Result<(Vec<f32>, usize), String> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("expected a RIFF/WAVE file".into());
    }
    let mut offset = 12usize;
    let mut format = None;
    let mut data = None;
    while offset.checked_add(8).is_some_and(|end| end <= bytes.len()) {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let start = offset + 8;
        let end = start.checked_add(size).ok_or("WAV chunk size overflow")?;
        if end > bytes.len() {
            return Err("truncated WAV chunk".into());
        }
        if id == b"fmt " {
            format = Some(&bytes[start..end]);
        } else if id == b"data" {
            data = Some(&bytes[start..end]);
        }
        offset = end
            .checked_add(size & 1)
            .ok_or("WAV chunk offset overflow")?;
    }
    let fmt = format.ok_or("missing fmt chunk")?;
    let payload = data.ok_or("missing data chunk")?;
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
        return Err(format!(
            "expected mono or stereo PCM, got {channels} channels"
        ));
    }
    if sample_rate != SAMPLE_RATE {
        return Err(format!(
            "expected {SAMPLE_RATE} Hz prepared PCM, got {sample_rate} Hz"
        ));
    }
    let sample_width = match (tag, bits) {
        (1, 16) => 2,
        (3, 32) => 4,
        _ => return Err(format!("unsupported WAV encoding tag {tag}, {bits} bits")),
    };
    if payload.len() % sample_width != 0 {
        return Err(format!("{bits}-bit PCM data is not sample-aligned"));
    }
    let sample_count = payload.len() / sample_width;
    let decoded_size = sample_count
        .checked_mul(size_of::<f32>())
        .ok_or("decoded PCM size overflow")?;
    if decoded_size > decoded_budget {
        return Err("decoded preview audio exceeds the 256 MiB pilot limit".into());
    }
    let mut samples = Vec::new();
    samples
        .try_reserve_exact(sample_count)
        .map_err(|_| "not enough memory to decode preview audio")?;
    match (tag, bits) {
        (1, 16) => {
            for sample in payload.chunks_exact(2) {
                samples.push(i16::from_le_bytes([sample[0], sample[1]]) as f32 / 32768.0);
            }
        }
        (3, 32) => {
            for sample in payload.chunks_exact(4) {
                samples.push(f32::from_le_bytes(sample.try_into().unwrap()));
            }
        }
        _ => unreachable!(),
    }
    if samples.len() % channels != 0 {
        return Err("PCM data is not frame-aligned".into());
    }
    Ok((samples, channels))
}

fn mix_frames(tracks: &[LoadedTrack], start: usize, frames: usize, output: &mut [i16]) {
    for frame in 0..frames {
        let mut left = 0.0f32;
        let mut right = 0.0f32;
        for track in tracks {
            if track.muted {
                continue;
            }
            let source_frame = start + frame;
            let source_index = source_frame * track.channels;
            if source_index >= track.samples.len() {
                continue;
            }
            let gain = 10.0f32.powf((track.gain_db as f32).clamp(-96.0, 24.0) / 20.0);
            left += track.samples[source_index] * gain;
            right += track.samples[source_index + usize::from(track.channels == 2)] * gain;
        }
        output[frame * 2] = float_to_i16(left);
        output[frame * 2 + 1] = float_to_i16(right);
    }
}

fn float_to_i16(value: f32) -> i16 {
    (value.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

fn mm_ok(code: u32, operation: &str) -> Result<(), String> {
    if code == MMSYSERR_NOERROR {
        Ok(())
    } else {
        Err(format!("could not {operation} (WinMM error {code})"))
    }
}

#[cfg(test)]
#[path = "audio_tests.rs"]
mod tests;
