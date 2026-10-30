//! The preview-audio worker thread: command loop, buffer refill and the sample clock.
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use windows::Win32::Media::Audio::{waveOutPause, waveOutRestart};

use super::{
    load_tracks, mix_frames, mm_ok, Command, LoadedTrack, SharedStatus, WaveBuffer, WaveDevice,
    FRAMES_PER_BUFFER, OUTPUT_CHANNELS, SAMPLE_RATE,
};

/// Length of the clock in frames: the longest stem, or the clip when the stems end early.
pub(super) fn clock_frames(stem_frames: usize, clip_seconds: f64) -> usize {
    let clip = if clip_seconds.is_finite() && clip_seconds > 0.0 { (clip_seconds * SAMPLE_RATE as f64).round() as usize } else { 0 };
    stem_frames.max(clip)
}

pub(super) fn worker_main(rx: mpsc::Receiver<Command>, shared: Arc<SharedStatus>) {
    let mut worker = Worker::new(shared);
    loop {
        while let Ok(command) = rx.try_recv() {
            let shutdown = matches!(command, Command::Shutdown);
            if !shutdown {
                worker.apply(command);
            }
            // Published after the command took effect: the UI trusts the shared clock again.
            worker.shared.applied.fetch_add(1, Ordering::AcqRel);
            if shutdown {
                return;
            }
        }
        if worker.playing {
            worker.pump();
        }
        thread::sleep(Duration::from_millis(4));
    }
}

struct Worker {
    shared: Arc<SharedStatus>,
    tracks: Vec<LoadedTrack>,
    device: Option<WaveDevice>,
    cursor_frame: usize,
    clock_base_frame: usize,
    /// The whole clip, not just the stems: a stem that ends early is followed by silence, so
    /// the clock (which drives the picture) still runs to the end of the clip.
    duration_frames: usize,
    playing: bool,
    active_generation: u64,
    /// Sound was queued after the last refill: finding the queue empty next time is an underrun.
    queued: bool,
}

impl Worker {
    fn new(shared: Arc<SharedStatus>) -> Self {
        Self {
            shared,
            tracks: Vec::new(),
            device: None,
            cursor_frame: 0,
            clock_base_frame: 0,
            duration_frames: 0,
            playing: false,
            active_generation: 0,
            queued: false,
        }
    }

    fn stop(&mut self) {
        self.playing = false;
        self.queued = false;
        self.shared.playing.store(false, Ordering::Release);
    }

    fn apply(&mut self, command: Command) {
        match command {
            Command::Open { generation, tracks, duration } => self.open(generation, &tracks, duration),
            Command::Close { generation } => {
                self.active_generation = generation;
                self.stop();
                self.shared.ready.store(false, Ordering::Release);
                self.shared.set_position(0.0);
                self.tracks.clear();
                self.device = None;
                (self.cursor_frame, self.clock_base_frame, self.duration_frames) = (0, 0, 0);
                if self.shared.generation.load(Ordering::Acquire) == generation {
                    *self.shared.error.lock().unwrap_or_else(|e| e.into_inner()) = None;
                }
            }
            Command::Play => {
                if self.shared.ready.load(Ordering::Acquire) && self.cursor_frame < self.duration_frames {
                    self.playing = true;
                    self.shared.playing.store(true, Ordering::Release);
                }
            }
            Command::Pause => {
                if let Some(output) = self.device.as_ref() {
                    if let Err(error) = mm_ok(unsafe { waveOutPause(output.handle) }, "pause wave output") {
                        self.shared.fail_if_current(self.active_generation, error);
                    }
                }
                self.playing = false;
                self.shared.playing.store(false, Ordering::Release);
            }
            Command::Seek(seconds) => self.seek(seconds),
            Command::SetAudio { stream_index, gain_db, muted } => {
                if let Some(track) = self.tracks.iter_mut().find(|track| track.stream_index == stream_index) {
                    track.gain_db = gain_db;
                    track.muted = muted;
                } else {
                    self.shared.fail_if_current(self.active_generation, format!("preview track {stream_index} is not loaded"));
                }
            }
            Command::Shutdown => {}
        }
    }

    fn open(&mut self, generation: u64, specs: &[super::PreviewTrack], duration: f64) {
        self.active_generation = generation;
        self.stop();
        self.device = None;
        (self.cursor_frame, self.clock_base_frame) = (0, 0);
        self.shared.set_position(0.0);
        match load_tracks(specs).and_then(|loaded| Ok((loaded, WaveDevice::open()?))) {
            Ok((loaded, device)) => {
                if self.shared.generation.load(Ordering::Acquire) != generation {
                    return;
                }
                let stems = loaded.iter().map(|track| track.samples.len() / track.channels).max().unwrap_or(0);
                self.duration_frames = clock_frames(stems, duration);
                self.shared.duration_frames.store(self.duration_frames as u64, Ordering::Release);
                self.tracks = loaded;
                self.device = Some(device);
                self.shared.ready.store(true, Ordering::Release);
            }
            Err(error) => {
                self.tracks.clear();
                self.duration_frames = 0;
                self.shared.fail_if_current(generation, error);
            }
        }
    }

    fn seek(&mut self, seconds: f64) {
        let target = ((seconds * SAMPLE_RATE as f64).round() as usize).min(self.duration_frames);
        let resume = self.playing;
        self.playing = false;
        self.queued = false;
        if let Some(output) = self.device.as_mut() {
            if let Err(error) = output.reset() {
                self.shared.fail_if_current(self.active_generation, error);
                return;
            }
            let _ = unsafe { waveOutPause(output.handle) };
        }
        self.cursor_frame = target;
        self.clock_base_frame = target;
        self.shared.set_position(target as f64 / SAMPLE_RATE as f64);
        self.playing = resume && target < self.duration_frames;
        self.shared.playing.store(self.playing, Ordering::Release);
    }

    /// Refills finished buffers, restarts output, and publishes the clock.
    fn pump(&mut self) {
        let Some(output) = self.device.as_mut() else {
            self.shared.fail_if_current(self.active_generation, "preview audio device is unavailable");
            self.playing = false;
            return;
        };
        if self.queued && self.cursor_frame < self.duration_frames && output.buffers.iter().all(WaveBuffer::done) {
            self.shared.underruns.fetch_add(1, Ordering::Relaxed);
        }
        let mut submitted = false;
        for buffer in &mut output.buffers {
            if self.cursor_frame >= self.duration_frames || !buffer.done() {
                continue;
            }
            if let Err(error) = buffer.unprepare(output.handle) {
                self.shared.fail_if_current(self.active_generation, error);
                self.playing = false;
                break;
            }
            let frames = (self.duration_frames - self.cursor_frame).min(FRAMES_PER_BUFFER);
            mix_frames(&self.tracks, self.cursor_frame, frames, &mut buffer.samples);
            if frames < FRAMES_PER_BUFFER {
                buffer.samples[frames * OUTPUT_CHANNELS..].fill(0);
            }
            if let Err(error) = buffer.submit(output.handle) {
                self.shared.fail_if_current(self.active_generation, error);
                self.playing = false;
                break;
            }
            self.cursor_frame += frames;
            submitted = true;
        }
        self.queued = output.buffers.iter().any(|buffer| !buffer.done());
        if submitted {
            if let Err(error) = mm_ok(unsafe { waveOutRestart(output.handle) }, "start wave output") {
                self.shared.fail_if_current(self.active_generation, error);
                self.playing = false;
            }
        }
        match output.played_frames() {
            Ok(frames) => {
                let absolute = (self.clock_base_frame + frames as usize).min(self.duration_frames);
                self.shared.set_position(absolute as f64 / SAMPLE_RATE as f64);
            }
            Err(error) => {
                self.shared.fail_if_current(self.active_generation, error);
                self.playing = false;
            }
        }
        if self.cursor_frame >= self.duration_frames && output.buffers.iter().all(WaveBuffer::done) {
            self.playing = false;
            self.shared.playing.store(false, Ordering::Release);
            self.shared.set_position(self.duration_frames as f64 / SAMPLE_RATE as f64);
        }
    }
}
