//! A track's sound as it arrives: PCM, which frames are decoded, and the waveform so far.
use std::sync::Mutex;
use std::sync::atomic::{AtomicI16, AtomicU16, AtomicU64, Ordering};

pub(super) const WAVE_BINS: usize = 1024;

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
    pub(super) fn write(&self, start: usize, pcm: &[i16]) {
        if start >= self.frames {
            return;
        }
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
    pub(super) fn mark(&self, start: usize, end: usize) {
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
    pub(super) fn next_gap(&self, from: usize) -> Option<usize> {
        gap_after(&self.done.lock().unwrap_or_else(|e| e.into_inner()), from, self.frames)
    }

    /// `[start, end)` frames not decoded yet.
    pub(super) fn missing(&self) -> Vec<(usize, usize)> {
        let mut holes = Vec::new();
        let mut cursor = 0;
        for run in self.done.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            if run.0 > cursor {
                holes.push((cursor, run.0));
            }
            cursor = run.1;
        }
        if cursor < self.frames {
            holes.push((cursor, self.frames));
        }
        holes
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    /// The waveform so far, in the engine's 0..1 form.
    pub(crate) fn peaks(&self) -> Vec<f32> {
        self.peaks.iter().map(|peak| peak.load(Ordering::Relaxed) as f32 / 32768.0).collect()
    }
}

pub(super) fn insert_run(runs: &mut Vec<(usize, usize)>, start: usize, end: usize) {
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

pub(super) fn gap_after(runs: &[(usize, usize)], from: usize, frames: usize) -> Option<usize> {
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
