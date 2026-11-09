//! Streaming edit of one audio track: decoded source frames go into a short window on the
//! source timeline, and the kept ranges are read out of it in order, as they become final.
//! Normal-speed ranges pass straight through; a range with a speed change is fed through
//! `atempo` as it arrives. Memory stays at about one second per track whatever the length
//! (the old whole-track buffer refused anything over 256 MiB, about 11.6 min of stereo).

use std::collections::VecDeque;

use ffmpeg_next as ff;

use super::edit_pcm::SampleRange;
use super::tempo::Stretcher;

/// Most source samples read out of the window in one step.
const STEP: usize = 4096;

/// Planar output samples waiting for the encoder or the mix.
pub(crate) type OutQueue = Vec<VecDeque<f32>>;

/// Source samples from `base` on, per channel; unwritten positions read as silence.
pub(crate) struct SourceWindow {
    base: i64,
    /// Nothing at or past this is ever read (the end of the last kept range).
    limit: i64,
    data: Vec<Vec<f32>>,
}

impl SourceWindow {
    pub fn new(channels: usize, base: i64, limit: i64) -> Self {
        Self { base, limit, data: vec![Vec::new(); channels] }
    }

    /// Writes `planes` (gain applied) at source sample `start`, overwriting what is there.
    /// Samples before the window or past the last range are dropped.
    pub fn write(&mut self, start: i64, planes: &[Vec<f32>], gain: f32) {
        let count = planes.first().map_or(0, Vec::len) as i64;
        let (from, to) = (start.max(self.base), start.saturating_add(count).min(self.limit));
        if to <= from {
            return;
        }
        let (at, skip, len) = ((from - self.base) as usize, (from - start) as usize, (to - from) as usize);
        for (target, source) in self.data.iter_mut().zip(planes) {
            if target.len() < at + len {
                target.resize(at + len, 0.0);
            }
            for (out, sample) in target[at..at + len].iter_mut().zip(&source[skip..skip + len]) {
                *out = sample * gain;
            }
        }
    }

    /// Source samples `from..to` per channel (silence where nothing was written).
    fn read(&self, from: i64, to: i64) -> Vec<Vec<f32>> {
        let len = (to - from).max(0) as usize;
        self.data
            .iter()
            .map(|channel| {
                let mut out = vec![0.0f32; len];
                let start = from - self.base;
                for (i, sample) in out.iter_mut().enumerate() {
                    let at = start + i as i64;
                    if at >= 0 && (at as usize) < channel.len() {
                        *sample = channel[at as usize];
                    }
                }
                out
            })
            .collect()
    }

    /// Forgets every sample before `position`.
    fn release_below(&mut self, position: i64) {
        if position <= self.base {
            return;
        }
        let drop = (position - self.base) as usize;
        for channel in &mut self.data {
            channel.drain(..drop.min(channel.len()));
        }
        self.base = position;
    }
}

/// Walks the kept ranges in order, turning final source samples into output samples.
pub(crate) struct RangeEditor {
    bounds: Vec<SampleRange>,
    rate: u32,
    layout: ff::ChannelLayout,
    index: usize,
    /// Source samples of the current range consumed so far.
    taken: usize,
    /// Output samples of the current range emitted so far.
    emitted: usize,
    stretcher: Option<Stretcher>,
}

impl RangeEditor {
    pub fn new(bounds: Vec<SampleRange>, rate: u32, layout: ff::ChannelLayout) -> Self {
        Self { bounds, rate, layout, index: 0, taken: 0, emitted: 0, stretcher: None }
    }

    /// Every range has been written out.
    pub fn done(&self) -> bool {
        self.index >= self.bounds.len()
    }

    /// Consumes the source below `final_end` (no later frame writes there) until about
    /// `budget` output samples came out. True when it stopped for the budget.
    pub fn pump(&mut self, window: &mut SourceWindow, final_end: i64, budget: usize, out: &mut OutQueue) -> Result<bool, String> {
        let mut produced = 0usize;
        let mut stopped = false;
        while let Some(range) = self.bounds.get(self.index).copied() {
            if produced >= budget {
                stopped = true;
                break;
            }
            let position = range.source_start + self.taken as i64;
            let end = range.source_end.min(final_end);
            if position < end {
                let count = ((end - position) as usize).min(STEP);
                let chunk = window.read(position, position + count as i64);
                produced += self.feed(range, &chunk, out)?;
                self.taken += count;
            } else if position >= range.source_end {
                produced += self.close(range, out)?;
                (self.index, self.taken, self.emitted) = (self.index + 1, 0, 0);
            } else {
                break;
            }
        }
        window.release_below(self.needed_from());
        Ok(stopped)
    }

    /// The earliest source sample a later step can still read.
    fn needed_from(&self) -> i64 {
        let Some(range) = self.bounds.get(self.index) else { return i64::MAX };
        let position = range.source_start + self.taken as i64;
        self.bounds.get(self.index + 1).map_or(position, |next| position.min(next.source_start))
    }

    fn feed(&mut self, range: SampleRange, chunk: &[Vec<f32>], out: &mut OutQueue) -> Result<usize, String> {
        if range.speed == 1.0 {
            return Ok(self.emit(range, chunk, out));
        }
        if self.stretcher.is_none() {
            self.stretcher = Some(Stretcher::new(self.rate, self.layout, range.speed)?);
        }
        let stretched = self.stretcher.as_mut().expect("stretcher created").push(chunk)?;
        Ok(self.emit(range, &stretched, out))
    }

    /// Ends the current range: the stretcher's tail, then silence up to its exact length.
    fn close(&mut self, range: SampleRange, out: &mut OutQueue) -> Result<usize, String> {
        let mut produced = 0;
        if let Some(stretcher) = self.stretcher.take() {
            let tail = stretcher.finish()?;
            produced += self.emit(range, &tail, out);
        }
        let missing = (range.output_end - range.output_start).saturating_sub(self.emitted);
        if missing > 0 {
            let silence = vec![vec![0.0f32; missing]; out.len()];
            produced += self.emit(range, &silence, out);
        }
        Ok(produced)
    }

    /// Queues `planes`, never more than the range's output length.
    fn emit(&mut self, range: SampleRange, planes: &[Vec<f32>], out: &mut OutQueue) -> usize {
        let left = (range.output_end - range.output_start).saturating_sub(self.emitted);
        let count = planes.first().map_or(0, Vec::len).min(left);
        for (queue, plane) in out.iter_mut().zip(planes) {
            queue.extend(&plane[..count]);
        }
        self.emitted += count;
        count
    }
}

/// Takes `count` samples per channel off the front of `queue`.
pub(crate) fn take(queue: &mut OutQueue, count: usize) -> Vec<Vec<f32>> {
    queue.iter_mut().map(|channel| channel.drain(..count.min(channel.len())).collect()).collect()
}

#[cfg(test)]
#[path = "pcm_stream_tests.rs"]
mod tests;
