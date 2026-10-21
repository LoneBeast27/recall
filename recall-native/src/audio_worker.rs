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

pub(super) fn worker_main(rx: mpsc::Receiver<Command>, shared: Arc<SharedStatus>) {
    let mut tracks = Vec::<LoadedTrack>::new();
    let mut device: Option<WaveDevice> = None;
    let mut cursor_frame = 0usize;
    let mut clock_base_frame = 0usize;
    let mut duration_frames = 0usize;
    let mut playing = false;
    let mut active_generation = 0u64;

    loop {
        while let Ok(command) = rx.try_recv() {
            match command {
                Command::Open {
                    generation,
                    tracks: specs,
                } => {
                    active_generation = generation;
                    playing = false;
                    shared.playing.store(false, Ordering::Release);
                    device = None;
                    cursor_frame = 0;
                    clock_base_frame = 0;
                    shared.set_position(0.0);
                    match load_tracks(&specs).and_then(|loaded| {
                        let next_device = WaveDevice::open()?;
                        Ok((loaded, next_device))
                    }) {
                        Ok((loaded, next_device)) => {
                            if shared.generation.load(Ordering::Acquire) != generation {
                                continue;
                            }
                            duration_frames = loaded
                                .iter()
                                .map(|track| track.samples.len() / track.channels)
                                .max()
                                .unwrap_or(0);
                            tracks = loaded;
                            device = Some(next_device);
                            shared.ready.store(true, Ordering::Release);
                        }
                        Err(error) => {
                            tracks.clear();
                            duration_frames = 0;
                            shared.fail_if_current(generation, error);
                        }
                    }
                }
                Command::Close { generation } => {
                    active_generation = generation;
                    playing = false;
                    shared.playing.store(false, Ordering::Release);
                    shared.ready.store(false, Ordering::Release);
                    shared.set_position(0.0);
                    tracks.clear();
                    device = None;
                    cursor_frame = 0;
                    clock_base_frame = 0;
                    duration_frames = 0;
                    if shared.generation.load(Ordering::Acquire) == generation {
                        *shared.error.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    }
                }
                Command::Play => {
                    if shared.ready.load(Ordering::Acquire) && cursor_frame < duration_frames {
                        playing = true;
                        shared.playing.store(true, Ordering::Release);
                    }
                }
                Command::Pause => {
                    if let Some(output) = device.as_ref() {
                        if let Err(error) =
                            mm_ok(unsafe { waveOutPause(output.handle) }, "pause wave output")
                        {
                            shared.fail_if_current(active_generation, error);
                        }
                    }
                    playing = false;
                    shared.playing.store(false, Ordering::Release);
                }
                Command::Seek(seconds) => {
                    let target =
                        ((seconds * SAMPLE_RATE as f64).round() as usize).min(duration_frames);
                    let resume = playing;
                    playing = false;
                    if let Some(output) = device.as_mut() {
                        if let Err(error) = output.reset() {
                            shared.fail_if_current(active_generation, error);
                            continue;
                        }
                        let _ = unsafe { waveOutPause(output.handle) };
                    }
                    cursor_frame = target;
                    clock_base_frame = target;
                    shared.set_position(target as f64 / SAMPLE_RATE as f64);
                    playing = resume && target < duration_frames;
                    shared.playing.store(playing, Ordering::Release);
                }
                Command::SetAudio {
                    stream_index,
                    gain_db,
                    muted,
                } => {
                    if let Some(track) = tracks
                        .iter_mut()
                        .find(|track| track.stream_index == stream_index)
                    {
                        track.gain_db = gain_db;
                        track.muted = muted;
                    } else {
                        shared.fail_if_current(
                            active_generation,
                            format!("preview track {stream_index} is not loaded"),
                        );
                    }
                }
                Command::Shutdown => return,
            }
        }

        if playing {
            let Some(output) = device.as_mut() else {
                shared.fail_if_current(active_generation, "preview audio device is unavailable");
                playing = false;
                continue;
            };
            let mut submitted = false;
            for buffer in &mut output.buffers {
                if cursor_frame >= duration_frames || !buffer.done() {
                    continue;
                }
                if let Err(error) = buffer.unprepare(output.handle) {
                    shared.fail_if_current(active_generation, error);
                    playing = false;
                    break;
                }
                let frames = (duration_frames - cursor_frame).min(FRAMES_PER_BUFFER);
                mix_frames(&tracks, cursor_frame, frames, &mut buffer.samples);
                if frames < FRAMES_PER_BUFFER {
                    buffer.samples[frames * OUTPUT_CHANNELS..].fill(0);
                }
                if let Err(error) = buffer.submit(output.handle) {
                    shared.fail_if_current(active_generation, error);
                    playing = false;
                    break;
                }
                cursor_frame += frames;
                submitted = true;
            }
            if submitted {
                if let Err(error) = mm_ok(
                    unsafe { waveOutRestart(output.handle) },
                    "start wave output",
                ) {
                    shared.fail_if_current(active_generation, error);
                    playing = false;
                }
            }
            match output.played_frames() {
                Ok(frames) => {
                    let absolute = (clock_base_frame + frames as usize).min(duration_frames);
                    shared.set_position(absolute as f64 / SAMPLE_RATE as f64);
                }
                Err(error) => {
                    shared.fail_if_current(active_generation, error);
                    playing = false;
                }
            }
            if cursor_frame >= duration_frames && output.buffers.iter().all(WaveBuffer::done) {
                playing = false;
                shared.playing.store(false, Ordering::Release);
                shared.set_position(duration_frames as f64 / SAMPLE_RATE as f64);
            }
        }
        thread::sleep(Duration::from_millis(4));
    }
}
