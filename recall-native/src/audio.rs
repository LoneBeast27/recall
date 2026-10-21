use std::fs::{self, File};
use std::io::Read;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use windows::core::PSTR;
use windows::Win32::Media::Audio::{
    waveOutClose, waveOutGetPosition, waveOutOpen, waveOutPause, waveOutPrepareHeader,
    waveOutReset, waveOutRestart, waveOutUnprepareHeader, waveOutWrite, CALLBACK_NULL, HWAVEOUT,
    WAVEFORMATEX, WAVEHDR, WAVE_FORMAT_PCM, WAVE_MAPPER, WHDR_DONE,
};
use windows::Win32::Media::{MMSYSERR_NOERROR, MMTIME, TIME_SAMPLES};

use super::media::PreviewTrack;

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
        })
    }

    pub(crate) fn open(&self, tracks: &[PreviewTrack]) -> Result<(), String> {
        let generation = self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.shared.ready.store(false, Ordering::Release);
        self.shared.playing.store(false, Ordering::Release);
        self.shared.set_position(0.0);
        *self.shared.error.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.send(Command::Open {
            generation,
            tracks: tracks.to_vec(),
        })
    }

    pub(crate) fn close(&self) -> Result<(), String> {
        let generation = self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.shared.ready.store(false, Ordering::Release);
        self.shared.playing.store(false, Ordering::Release);
        self.shared.set_position(0.0);
        *self.shared.error.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.send(Command::Close { generation })
    }

    pub(crate) fn play(&self) -> Result<(), String> {
        self.require_ready()?;
        self.send(Command::Play)
    }

    pub(crate) fn pause(&self) -> Result<(), String> {
        self.send(Command::Pause)
    }

    pub(crate) fn seek(&self, seconds: f64) -> Result<(), String> {
        if !seconds.is_finite() || seconds < 0.0 {
            return Err("preview seek must be a finite non-negative time".into());
        }
        self.require_ready()?;
        self.send(Command::Seek(seconds))
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
        self.shared.playing.load(Ordering::Acquire)
    }

    pub(crate) fn position(&self) -> f64 {
        self.shared.position()
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
            .map_err(|_| "preview audio worker stopped".into())
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

impl WaveDevice {
    fn open() -> Result<Self, String> {
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: OUTPUT_CHANNELS as u16,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * OUTPUT_CHANNELS as u32 * 2,
            nBlockAlign: (OUTPUT_CHANNELS * 2) as u16,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        let mut handle = HWAVEOUT::default();
        let code = unsafe {
            waveOutOpen(
                Some(&mut handle),
                WAVE_MAPPER,
                &format,
                None,
                None,
                CALLBACK_NULL,
            )
        };
        mm_ok(code, "open default wave output")?;
        mm_ok(unsafe { waveOutPause(handle) }, "pause new wave output")?;
        let buffers = (0..BUFFER_COUNT)
            .map(|_| WaveBuffer {
                samples: vec![0; FRAMES_PER_BUFFER * OUTPUT_CHANNELS],
                header: Box::new(WAVEHDR::default()),
                prepared: false,
            })
            .collect();
        Ok(Self { handle, buffers })
    }

    fn reset(&mut self) -> Result<(), String> {
        mm_ok(unsafe { waveOutReset(self.handle) }, "reset wave output")?;
        for buffer in &mut self.buffers {
            buffer.unprepare(self.handle)?;
        }
        Ok(())
    }

    fn played_frames(&self) -> Result<u32, String> {
        let mut time = MMTIME {
            wType: TIME_SAMPLES,
            ..Default::default()
        };
        mm_ok(
            unsafe { waveOutGetPosition(self.handle, &mut time, size_of::<MMTIME>() as u32) },
            "read wave output clock",
        )?;
        if time.wType != TIME_SAMPLES {
            return Err("wave output did not provide a sample clock".into());
        }
        Ok(unsafe { time.u.sample })
    }
}

impl Drop for WaveDevice {
    fn drop(&mut self) {
        let _ = unsafe { waveOutReset(self.handle) };
        for buffer in &mut self.buffers {
            let _ = buffer.unprepare(self.handle);
        }
        let _ = unsafe { waveOutClose(self.handle) };
    }
}

impl WaveBuffer {
    fn done(&self) -> bool {
        !self.prepared
            || unsafe { std::ptr::read_volatile(std::ptr::addr_of!(self.header.dwFlags)) }
                & WHDR_DONE
                != 0
    }

    fn submit(&mut self, handle: HWAVEOUT) -> Result<(), String> {
        self.header.lpData = PSTR(self.samples.as_mut_ptr().cast());
        self.header.dwBufferLength = (self.samples.len() * size_of::<i16>()) as u32;
        self.header.dwFlags = 0;
        mm_ok(
            unsafe {
                waveOutPrepareHeader(handle, self.header.as_mut(), size_of::<WAVEHDR>() as u32)
            },
            "prepare wave buffer",
        )?;
        self.prepared = true;
        if let Err(error) = mm_ok(
            unsafe { waveOutWrite(handle, self.header.as_mut(), size_of::<WAVEHDR>() as u32) },
            "queue wave buffer",
        ) {
            let _ = self.unprepare(handle);
            return Err(error);
        }
        Ok(())
    }

    fn unprepare(&mut self, handle: HWAVEOUT) -> Result<(), String> {
        if self.prepared {
            mm_ok(
                unsafe {
                    waveOutUnprepareHeader(
                        handle,
                        self.header.as_mut(),
                        size_of::<WAVEHDR>() as u32,
                    )
                },
                "unprepare wave buffer",
            )?;
            self.prepared = false;
        }
        Ok(())
    }
}

fn worker_main(rx: mpsc::Receiver<Command>, shared: Arc<SharedStatus>) {
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
mod tests {
    use super::*;

    fn pcm16_wav(sample_rate: u32, data: &[u8]) -> Vec<u8> {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36u32 + data.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(data);
        wav
    }

    #[test]
    fn parses_pcm16_with_unknown_chunks() {
        let mut wav = b"RIFF\0\0\0\0WAVEJUNK\x02\0\0\0xxfmt \x10\0\0\0\x01\0\x01\0\x80\xbb\0\0\0w\x01\0\x02\0\x10\0data\x04\0\0\0\0\x80\xff\x7f".to_vec();
        let size = (wav.len() - 8) as u32;
        wav[4..8].copy_from_slice(&size.to_le_bytes());
        let (samples, channels) = parse_wav(&wav, MAX_DECODED_BYTES).unwrap();
        assert_eq!(channels, 1);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0], -1.0);
        assert!(samples[1] > 0.99);
    }

    #[test]
    fn mixes_gain_mute_and_mono_to_stereo() {
        let tracks = vec![
            LoadedTrack {
                stream_index: 0,
                samples: vec![0.5],
                channels: 1,
                gain_db: 0.0,
                muted: false,
            },
            LoadedTrack {
                stream_index: 1,
                samples: vec![1.0, 1.0],
                channels: 2,
                gain_db: -6.0206,
                muted: false,
            },
            LoadedTrack {
                stream_index: 2,
                samples: vec![1.0],
                channels: 1,
                gain_db: 0.0,
                muted: true,
            },
        ];
        let mut output = [0i16; 2];
        mix_frames(&tracks, 0, 1, &mut output);
        assert!((output[0] as i32 - 32767).abs() < 4);
        assert!((output[1] as i32 - 32767).abs() < 4);
    }

    #[test]
    fn rejects_malformed_truncated_and_non_48k_wav() {
        assert!(parse_wav(b"not a wave", MAX_DECODED_BYTES).is_err());
        let mut truncated = pcm16_wav(SAMPLE_RATE, &[0, 0]);
        truncated.truncate(truncated.len() - 1);
        assert!(parse_wav(&truncated, MAX_DECODED_BYTES).is_err());
        let non_48k = pcm16_wav(44_100, &[0, 0]);
        assert!(parse_wav(&non_48k, MAX_DECODED_BYTES).is_err());
    }

    #[test]
    fn clips_mixed_output() {
        let tracks = vec![LoadedTrack {
            stream_index: 0,
            samples: vec![1.0, -1.0],
            channels: 2,
            gain_db: 24.0,
            muted: false,
        }];
        let mut output = [0i16; 2];
        mix_frames(&tracks, 0, 1, &mut output);
        assert_eq!(output, [i16::MAX, -i16::MAX]);
    }

    #[test]
    fn stale_generation_cannot_publish_error() {
        let status = SharedStatus::default();
        status.generation.store(2, Ordering::Release);
        status.fail_if_current(1, "stale");
        assert!(status.error.lock().unwrap().is_none());
        status.fail_if_current(2, "current");
        assert_eq!(status.error.lock().unwrap().as_deref(), Some("current"));
    }
}
