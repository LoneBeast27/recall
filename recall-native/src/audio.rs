//! Preview audio: prepared stems mixed on a worker thread into WinMM wave-out.
//! Device/buffers in `audio_wave.rs`, the worker loop in `audio_worker.rs`, stems read from
//! disk as they play in `audio_stem.rs`.
use std::cell::Cell;
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
#[path = "audio_stream.rs"]
pub(crate) mod stream;
use worker::worker_main;

const SAMPLE_RATE: u32 = 48_000;
const OUTPUT_CHANNELS: usize = 2;
const FRAMES_PER_BUFFER: usize = 960; // 20 ms
const BUFFER_COUNT: usize = 4;

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

/// Where a clip's sound comes from.
enum Source {
    Stems(Vec<PreviewTrack>),
    /// Decoded as the clip loads (`audio_stream.rs`).
    Live(Vec<stream::LiveTrack>),
}

enum Command {
    Open {
        generation: u64,
        tracks: Source,
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
        self.open_source(Source::Stems(tracks.to_vec()), duration)
    }

    pub(crate) fn open_live(&self, tracks: Vec<stream::LiveTrack>, duration: f64) -> Result<(), String> {
        self.open_source(Source::Live(tracks), duration)
    }

    fn open_source(&self, tracks: Source, duration: f64) -> Result<(), String> {
        let generation = self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.shared.ready.store(false, Ordering::Release);
        self.shared.playing.store(false, Ordering::Release);
        self.shared.set_position(0.0);
        self.shared.duration_frames.store(0, Ordering::Release);
        *self.shared.error.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let sent = self.send(Command::Open {
            generation,
            tracks,
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
        // No ready check: a mute or gain set while the tracks load must not be lost. The worker
        // takes commands in order, so this one lands after the queued Open (2026-11-04).
        if let Some(error) = self.error() {
            return Err(error);
        }
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

/// A track's samples: the stem on disk, or what has been decoded so far.
enum Samples {
    Stem(stem::StemReader),
    Live(Arc<stream::LivePcm>),
    #[cfg(test)]
    Memory(Vec<f32>),
}

impl Samples {
    /// Interleaved samples (frames x channels) the track holds.
    fn len(&self) -> usize {
        match self {
            Samples::Stem(stem) => stem.frames() * stem.channels(),
            Samples::Live(pcm) => pcm.len(),
            #[cfg(test)]
            Samples::Memory(samples) => samples.len(),
        }
    }

    /// Interleaved samples of frames `[from, from + count)` into `out`; silence past the end.
    fn read(&mut self, channels: usize, from: usize, count: usize, out: &mut Vec<f32>) -> Result<(), String> {
        let values = from * channels..(from + count) * channels;
        out.clear();
        match self {
            Samples::Stem(stem) => return stem.read(from, count, out),
            Samples::Live(pcm) => out.extend(values.map(|i| if i < pcm.len() { pcm.sample(i) } else { 0.0 })),
            #[cfg(test)]
            Samples::Memory(samples) => out.extend(values.map(|i| samples.get(i).copied().unwrap_or(0.0))),
        }
        Ok(())
    }
}

struct LoadedTrack {
    stream_index: usize,
    samples: Samples,
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
    specs
        .iter()
        .map(|spec| {
            let stem = stem::StemReader::open(&spec.path)?;
            Ok(LoadedTrack { stream_index: spec.stream_index, channels: stem.channels(), samples: Samples::Stem(stem), gain_db: spec.gain_db, muted: spec.muted })
        })
        .collect()
}

fn live_tracks(tracks: Vec<stream::LiveTrack>) -> Vec<LoadedTrack> {
    tracks
        .into_iter()
        .map(|track| LoadedTrack { stream_index: track.stream_index, channels: track.pcm.channels, samples: Samples::Live(track.pcm), gain_db: track.gain_db, muted: track.muted })
        .collect()
}

#[path = "audio_flac.rs"]
mod flac;
#[path = "audio_stem.rs"]
mod stem;

fn mix_frames(tracks: &mut [LoadedTrack], start: usize, frames: usize, output: &mut [i16]) -> Result<(), String> {
    let mut mixed = vec![0.0f32; frames * OUTPUT_CHANNELS];
    let mut block = Vec::with_capacity(frames * OUTPUT_CHANNELS);
    for track in tracks.iter_mut() {
        if track.muted {
            continue;
        }
        let channels = track.channels;
        track.samples.read(channels, start, frames, &mut block)?;
        let gain = 10.0f32.powf((track.gain_db as f32).clamp(-96.0, 24.0) / 20.0);
        for (frame, pair) in block.chunks_exact(channels).zip(mixed.chunks_exact_mut(2)) {
            pair[0] += frame[0] * gain;
            pair[1] += frame[channels - 1] * gain;
        }
    }
    for (slot, value) in output.iter_mut().zip(&mixed) {
        *slot = float_to_i16(*value);
    }
    Ok(())
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
