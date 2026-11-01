//! Sound for a clip that is still loading. The stems the engine prepares need the whole file,
//! so until then the clip's own tracks are decoded here with a Media Foundation source reader
//! over the SSD fill, right behind the copy, into PCM the mixer plays as it arrives.
//!
//! Decoding runs from the playhead forward, then wraps to whatever was skipped, and only
//! touches bytes already on the SSD, so it never holds up a seek. The result fills the whole
//! clip, so no swap to the stems is needed while the clip stays open.
use std::sync::atomic::{AtomicBool, AtomicI16, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Win32::Media::MediaFoundation::{
    IMFMediaSource, IMFMediaType, IMFSample, IMFSourceReader, MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_NUM_CHANNELS,
    MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SOURCE_READER_ALL_STREAMS,
    MF_SOURCE_READER_ANY_STREAM, MF_SOURCE_READERF_ENDOFSTREAM, MFAudioFormat_PCM, MFCreateMediaType,
    MFCreateSourceReaderFromMediaSource, MFMediaType_Audio,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::core::GUID;

use super::SAMPLE_RATE;
use crate::services::library::mp4_map::ClipMap;
use crate::services::storage::fill::{ClipFill, Outcome};

const WAVE_BINS: usize = 1024;
/// Time decoded memory may take, all tracks together. Touched pages only as the clip loads.
const MAX_LIVE_BYTES: u64 = 2 << 30;
/// Decoding waits until this much past its position is on the SSD.
const READ_AHEAD: f64 = 1.0;
/// Frames a packet may sit off the back-to-back position before it counts as a real gap
/// (the stems ignore the same timestamp jitter).
const JITTER_FRAMES: i64 = 1200;

/// A track's PCM as it arrives. Frames not decoded yet read as silence.
pub(crate) struct LivePcm {
    pub(crate) channels: usize,
    frames: usize,
    samples: Box<[AtomicI16]>,
    /// Decoded `[start, end)` frame runs, sorted and merged.
    done: Mutex<Vec<(usize, usize)>>,
    peaks: Box<[AtomicU16]>,
    revision: AtomicU64,
}

impl LivePcm {
    pub(crate) fn new(frames: usize, channels: usize) -> Self {
        let zeroed = vec![0i16; frames * channels].into_boxed_slice();
        // AtomicI16 has the layout of i16; the zeroed pages stay untouched until written.
        let samples = unsafe { Box::from_raw(Box::into_raw(zeroed) as *mut [AtomicI16]) };
        let peaks = (0..WAVE_BINS).map(|_| AtomicU16::new(0)).collect();
        Self { channels, frames, samples, done: Mutex::new(Vec::new()), peaks, revision: AtomicU64::new(0) }
    }

    pub(crate) fn len(&self) -> usize {
        self.samples.len()
    }

    pub(crate) fn sample(&self, index: usize) -> f32 {
        self.samples[index].load(Ordering::Relaxed) as f32 / 32768.0
    }

    /// Stores `pcm` (interleaved) from `start` and counts those frames as decoded.
    fn write(&self, start: usize, pcm: &[i16]) {
        let frames = (pcm.len() / self.channels).min(self.frames.saturating_sub(start));
        for (slot, value) in self.samples[start * self.channels..].iter().zip(&pcm[..frames * self.channels]) {
            slot.store(*value, Ordering::Relaxed);
        }
        for (index, frame) in pcm[..frames * self.channels].chunks_exact(self.channels).enumerate() {
            let bin = ((start + index).saturating_mul(WAVE_BINS) / self.frames.max(1)).min(WAVE_BINS - 1);
            let level = frame.iter().map(|s| (*s as i32).unsigned_abs()).max().unwrap_or(0).min(u16::MAX as u32) as u16;
            self.peaks[bin].fetch_max(level, Ordering::Relaxed);
        }
        self.mark(start, start + frames);
    }

    /// Counts `[start, end)` as decoded (a real gap in the track stays silent).
    fn mark(&self, start: usize, end: usize) {
        if start < end {
            insert_run(&mut self.done.lock().unwrap_or_else(|e| e.into_inner()), start, end.min(self.frames));
            self.revision.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Every frame of `[start, end)` is decoded.
    pub(crate) fn has(&self, start: usize, end: usize) -> bool {
        let end = end.min(self.frames);
        start >= end || self.done.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|run| run.0 <= start && run.1 >= end)
    }

    /// First undecoded frame at or after `from`, else the first one before it.
    fn next_gap(&self, from: usize) -> Option<usize> {
        gap_after(&self.done.lock().unwrap_or_else(|e| e.into_inner()), from, self.frames)
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    /// The waveform so far, in the engine's 0..1 form.
    pub(crate) fn peaks(&self) -> Vec<f32> {
        self.peaks.iter().map(|peak| peak.load(Ordering::Relaxed) as f32 / 32768.0).collect()
    }
}

fn insert_run(runs: &mut Vec<(usize, usize)>, start: usize, end: usize) {
    let (mut start, mut end) = (start, end);
    runs.retain(|run| {
        let apart = run.1 < start || run.0 > end;
        if !apart {
            start = start.min(run.0);
            end = end.max(run.1);
        }
        apart
    });
    let at = runs.partition_point(|run| run.0 < start);
    runs.insert(at, (start, end));
}

fn gap_after(runs: &[(usize, usize)], from: usize, frames: usize) -> Option<usize> {
    let mut cursor = from.min(frames);
    for run in runs {
        if run.0 > cursor {
            break;
        }
        cursor = cursor.max(run.1);
    }
    if cursor < frames {
        return Some(cursor);
    }
    match runs.first() {
        Some(first) if first.0 > 0 => Some(0),
        Some(first) if first.1 < from.min(frames) => Some(first.1),
        None if frames > 0 => Some(0),
        _ => None,
    }
}

/// One track for the mixer.
#[derive(Clone)]
pub(crate) struct LiveTrack {
    pub(crate) stream_index: usize,
    pub(crate) pcm: Arc<LivePcm>,
    pub(crate) gain_db: f64,
    pub(crate) muted: bool,
}

struct Slot {
    reader_stream: u32,
    pcm: Arc<LivePcm>,
    rate: u32,
    channels: usize,
    /// Where the next packet goes if it follows on.
    next: Option<i64>,
}

/// The decoder, ready to start, and the handle the player keeps.
pub(crate) struct LiveSound {
    pub(crate) tracks: Vec<(usize, Arc<LivePcm>)>,
    playhead: AtomicU64,
    stop: AtomicBool,
    failed: Mutex<Option<String>>,
}

impl LiveSound {
    pub(crate) fn set_playhead(&self, seconds: f64) {
        self.playhead.store(seconds.to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Why the decoder gave up: the stems have to bring the sound.
    pub(crate) fn failed(&self) -> Option<String> {
        self.failed.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// `[from, to)` seconds can be heard on every track.
    pub(crate) fn has(&self, from: f64, to: f64) -> bool {
        let frame = |t: f64| (t.max(0.0) * SAMPLE_RATE as f64) as usize;
        self.tracks.iter().all(|(_, pcm)| pcm.has(frame(from), frame(to)))
    }
}

struct SendReader(IMFSourceReader);
unsafe impl Send for SendReader {}

/// Selects the audio streams of `source`, pairs them in order with `tracks`
/// (stream index, channels) and starts decoding. Err: this clip's sound must wait for the stems.
pub(crate) fn start(source: &IMFMediaSource, fill: ClipFill, map: Arc<ClipMap>, tracks: &[(usize, u16)], origin: f64, duration: f64) -> Result<Arc<LiveSound>, String> {
    let frames = (duration.max(0.0) * SAMPLE_RATE as f64).ceil() as usize;
    let bytes: u64 = tracks.iter().map(|(_, channels)| frames as u64 * u64::from((*channels).clamp(1, 2)) * 2).sum();
    if bytes > MAX_LIVE_BYTES {
        return Err("clip is too long to decode in memory".into());
    }
    let reader = unsafe { MFCreateSourceReaderFromMediaSource(source, None) }.map_err(|e| format!("sound reader: {e}"))?;
    let all = MF_SOURCE_READER_ALL_STREAMS.0 as u32;
    unsafe { reader.SetStreamSelection(all, false) }.map_err(|e| format!("sound reader: {e}"))?;
    let mut slots = Vec::with_capacity(tracks.len());
    let mut stream = 0u32;
    while slots.len() < tracks.len() {
        let Ok(native) = (unsafe { reader.GetNativeMediaType(stream, 0) }) else { return Err("clip has fewer sound streams than expected".into()) };
        if unsafe { native.GetGUID(&MF_MT_MAJOR_TYPE) }.is_ok_and(|major| major == MFMediaType_Audio) {
            slots.push(open_track(&reader, stream, tracks[slots.len()].1, frames)?);
        }
        stream += 1;
    }
    let sound = Arc::new(LiveSound {
        tracks: slots.iter().zip(tracks).map(|(slot, track)| (track.0, Arc::clone(&slot.pcm))).collect(),
        playhead: AtomicU64::new(0f64.to_bits()),
        stop: AtomicBool::new(false),
        failed: Mutex::new(None),
    });
    let handle = Arc::clone(&sound);
    let reader = SendReader(reader);
    std::thread::Builder::new()
        .name("recall-live-sound".into())
        .spawn(move || {
            let reader = reader;
            let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            if let Err(error) = decode(&reader.0, slots, &handle, &fill, &map, origin) {
                *handle.failed.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
            }
            if hr.is_ok() {
                unsafe { CoUninitialize() };
            }
        })
        .map_err(|e| format!("start sound decoder: {e}"))?;
    Ok(sound)
}

fn open_track(reader: &IMFSourceReader, stream: u32, channels: u16, frames: usize) -> Result<Slot, String> {
    let wanted = channels.clamp(1, 2);
    let target = pcm_type(u32::from(wanted)).map_err(|e| format!("sound format: {e}"))?;
    unsafe {
        reader.SetStreamSelection(stream, true).map_err(|e| format!("sound stream: {e}"))?;
        reader.SetCurrentMediaType(stream, None, &target).map_err(|e| format!("sound decoder: {e}"))?;
    }
    let current = unsafe { reader.GetCurrentMediaType(stream) }.map_err(|e| format!("sound format: {e}"))?;
    let rate = unsafe { current.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND) }.map_err(|e| format!("sound rate: {e}"))?;
    let decoded = unsafe { current.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS) }.map_err(|e| format!("sound channels: {e}"))? as usize;
    if !(1..=2).contains(&decoded) {
        return Err(format!("{decoded} sound channels"));
    }
    Ok(Slot { reader_stream: stream, pcm: Arc::new(LivePcm::new(frames, decoded)), rate, channels: decoded, next: None })
}

fn pcm_type(channels: u32) -> windows::core::Result<IMFMediaType> {
    unsafe {
        let target = MFCreateMediaType()?;
        target.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        target.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
        target.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        target.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels)?;
        target.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE)?;
        Ok(target)
    }
}

fn decode(reader: &IMFSourceReader, mut slots: Vec<Slot>, sound: &LiveSound, fill: &ClipFill, map: &ClipMap, origin: f64) -> Result<(), String> {
    let rate = SAMPLE_RATE as f64;
    let mut position: Option<f64> = None;
    let mut ended = vec![false; slots.len()];
    let mut passes = 0;
    loop {
        if sound.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let playhead = f64::from_bits(sound.playhead.load(Ordering::Relaxed));
        let from = (playhead.max(0.0) * rate) as usize;
        let Some(gap) = slots.iter().filter_map(|slot| slot.pcm.next_gap(from)).min() else { return Ok(()) };
        let want = gap as f64 / rate;
        if position.is_none_or(|at| want < at - 0.25 || want > at + READ_AHEAD * 2.0) {
            if !bytes_ready(fill, map, want)? {
                continue;
            }
            seek(reader, want + origin)?;
            for slot in &mut slots {
                slot.next = None;
            }
            ended.fill(false);
            position = Some(want);
        }
        if !bytes_ready(fill, map, position.unwrap_or(want))? {
            continue;
        }
        let (mut actual, mut flags, mut stamp) = (0u32, 0u32, 0i64);
        let mut sample: Option<IMFSample> = None;
        unsafe { reader.ReadSample(MF_SOURCE_READER_ANY_STREAM.0 as u32, 0, Some(&mut actual), Some(&mut flags), Some(&mut stamp), Some(&mut sample)) }.map_err(|e| format!("read sound: {e}"))?;
        let Some(slot) = slots.iter().position(|slot| slot.reader_stream == actual) else { continue };
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            ended[slot] = true;
            let pcm = &slots[slot].pcm;
            let tail = slots[slot].next.map_or(gap, |next| next.max(0) as usize);
            pcm.mark(tail, usize::MAX);
            if ended.iter().all(|done| *done) {
                // Whatever the file does not hold is silence: a second pass ends it.
                passes += 1;
                position = None;
                if passes > 3 {
                    for slot in &slots {
                        slot.pcm.mark(0, usize::MAX);
                    }
                }
            }
            continue;
        }
        let Some(sample) = sample else { continue };
        let seconds = stamp as f64 / 1e7 - origin;
        position = Some(seconds.max(0.0));
        place(&mut slots[slot], &sample, seconds)?;
    }
}

/// Waits a moment when the bytes at `seconds` are not on the SSD. False: look again.
fn bytes_ready(fill: &ClipFill, map: &ClipMap, seconds: f64) -> Result<bool, String> {
    match fill.outcome() {
        Outcome::Done => Ok(true),
        Outcome::Cancelled => Err(fill.error().unwrap_or_else(|| "clip closed".into())),
        Outcome::Filling => {
            let (start, end) = map.bytes_of(seconds, seconds + READ_AHEAD);
            let ready = fill.has(start, end);
            if !ready {
                std::thread::sleep(Duration::from_millis(15));
            }
            Ok(ready)
        }
    }
}

fn seek(reader: &IMFSourceReader, seconds: f64) -> Result<(), String> {
    let ticks = (seconds.max(0.0) * 1e7) as i64;
    let at = crate::media::propvariant_i64(ticks);
    unsafe { reader.SetCurrentPosition(&GUID::zeroed(), &at) }.map_err(|e| format!("seek sound: {e}"))
}

/// Puts one decoded packet into its track: back to back with the last one, unless the
/// timestamps show a real gap.
fn place(slot: &mut Slot, sample: &IMFSample, seconds: f64) -> Result<(), String> {
    let buffer = unsafe { sample.ConvertToContiguousBuffer() }.map_err(|e| format!("sound packet: {e}"))?;
    let (mut data, mut length) = (std::ptr::null_mut(), 0u32);
    unsafe { buffer.Lock(&mut data, None, Some(&mut length)) }.map_err(|e| format!("sound packet: {e}"))?;
    let pcm: Vec<i16> = unsafe { std::slice::from_raw_parts(data, length as usize) }.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
    let _ = unsafe { buffer.Unlock() };
    let pcm = if slot.rate == SAMPLE_RATE { pcm } else { resample(&pcm, slot.channels, slot.rate) };
    let stamped = (seconds * SAMPLE_RATE as f64).round() as i64;
    let start = match slot.next {
        Some(next) if (stamped - next).abs() <= JITTER_FRAMES => next,
        Some(next) if stamped > next => {
            slot.pcm.mark(next.max(0) as usize, stamped.max(0) as usize);
            stamped
        }
        _ => stamped,
    };
    let frames = (pcm.len() / slot.channels) as i64;
    slot.next = Some(start + frames);
    // Samples before the clip's first frame have nowhere to go.
    let skip = (-start).max(0).min(frames) as usize * slot.channels;
    slot.pcm.write(start.max(0) as usize, &pcm[skip..]);
    Ok(())
}

/// Linear resampling of one packet to the mixer's rate.
fn resample(pcm: &[i16], channels: usize, rate: u32) -> Vec<i16> {
    let frames = pcm.len() / channels;
    let out = frames * SAMPLE_RATE as usize / rate.max(1) as usize;
    let mut resampled = Vec::with_capacity(out * channels);
    for frame in 0..out {
        let at = frame as f64 * rate as f64 / SAMPLE_RATE as f64;
        let (low, part) = (at as usize, at.fract() as f32);
        for channel in 0..channels {
            let a = pcm[low.min(frames - 1) * channels + channel] as f32;
            let b = pcm[(low + 1).min(frames - 1) * channels + channel] as f32;
            resampled.push((a + (b - a) * part) as i16);
        }
    }
    resampled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoded_runs_merge_and_the_next_gap_follows_the_playhead() {
        let mut runs = Vec::new();
        insert_run(&mut runs, 100, 200);
        insert_run(&mut runs, 300, 400);
        assert_eq!(gap_after(&runs, 0, 1000), Some(0));
        assert_eq!(gap_after(&runs, 150, 1000), Some(200));
        assert_eq!(gap_after(&runs, 350, 1000), Some(400));
        insert_run(&mut runs, 190, 310);
        assert_eq!(runs, vec![(100, 400)]);
        insert_run(&mut runs, 400, 1000);
        assert_eq!(gap_after(&runs, 500, 1000), Some(0), "wraps to what the seek skipped");
        insert_run(&mut runs, 0, 100);
        assert_eq!(gap_after(&runs, 500, 1000), None);
    }

    #[test]
    fn frames_read_as_silence_until_decoded_and_peaks_fill_in() {
        let pcm = LivePcm::new(WAVE_BINS * 2, 2);
        assert_eq!(pcm.sample(0), 0.0);
        assert!(!pcm.has(0, 4));
        pcm.write(0, &[16384, -16384, 8192, 8192]);
        assert_eq!(pcm.sample(0), 0.5);
        assert_eq!(pcm.sample(1), -0.5);
        assert!(pcm.has(0, 2));
        assert!(!pcm.has(0, 3));
        assert_eq!(pcm.peaks()[0], 0.5);
        assert_eq!(pcm.peaks()[1], 0.0);
    }

    #[test]
    fn writes_past_the_clip_are_clipped() {
        let pcm = LivePcm::new(2, 1);
        pcm.write(1, &[1, 2, 3]);
        assert!(pcm.has(1, 2));
        assert_eq!(pcm.next_gap(0), Some(0));
    }

    #[test]
    fn packets_at_other_rates_come_out_at_48_khz() {
        let pcm: Vec<i16> = (0..441).map(|i| i as i16).collect();
        let out = resample(&pcm, 1, 44_100);
        assert_eq!(out.len(), 480);
        assert_eq!(out[0], 0);
        assert!((out[240] as i32 - 220).abs() <= 1);
    }
}
