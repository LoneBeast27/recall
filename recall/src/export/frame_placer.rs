//! Where decoded frames of one audio track go before the kept ranges are read out
//! (`track_reader`, `pcm_stream`): frames wait here until everything a margin behind them is
//! final, then they are written into the source window.
//!
//! A frame stamped far ahead of the previous one (more than the margin) would make everything
//! before it final at once, so the ranges up to it close as silence. A damaged or foreign file
//! can carry one such lone stamp; the next frame then jumps back. So a far jump waits for the
//! next frame: when that one lands earlier, the jumping frame is dropped (as the old
//! whole-track buffer did); when it follows on, the jump is a real gap and stands.

use std::collections::VecDeque;

use super::pcm_stream::{OutQueue, RangeEditor, SourceWindow};

struct Frame {
    start: i64,
    planes: Vec<Vec<f32>>,
    /// Placed more than the margin past where the previous frame ended: not trusted until the
    /// next frame agrees.
    leap: bool,
}

pub(crate) struct Placer {
    pending: VecDeque<Frame>,
    /// Source positions below this are final: no later frame lands there.
    final_end: i64,
    /// How far back a frame may land from where the decoder is (stamp jitter).
    margin: i64,
    input_done: bool,
    /// Frames queued so far (the decoder reads until this grows).
    queued: u64,
}

impl Placer {
    pub fn new(margin: i64) -> Self {
        Self { pending: VecDeque::new(), final_end: i64::MIN, margin, input_done: false, queued: 0 }
    }

    /// Queues `planes` at source sample `start`; `expected` is where the previous frame ended.
    pub fn push(&mut self, start: i64, expected: Option<i64>, planes: Vec<Vec<f32>>) {
        let leap = expected.is_some_and(|expected| start > expected.saturating_add(self.margin));
        self.pending.push_back(Frame { start, planes, leap });
        self.queued += 1;
    }

    pub fn queued(&self) -> u64 {
        self.queued
    }

    /// The input has ended: every frame it will give is queued.
    pub fn end_input(&mut self) {
        self.input_done = true;
    }

    /// One placement step; false when it needs another decoded frame first.
    fn place(&mut self, window: &mut SourceWindow, gain: f32) -> bool {
        let Some(front) = self.pending.front() else { return false };
        if front.leap {
            let start = front.start;
            match self.pending.get(1) {
                None if !self.input_done => return false,
                Some(next) if next.start < start => {
                    // A lone far-future stamp: the stream goes on where it was.
                    self.pending.pop_front();
                    return true;
                }
                _ => self.pending[0].leap = false,
            }
        }
        let frame = self.pending.pop_front().expect("front checked");
        let settled = frame.start.saturating_sub(self.margin);
        if settled > self.final_end {
            // Everything this far behind the new frame is final: edit it out first.
            self.final_end = settled;
            self.pending.push_front(frame);
        } else {
            window.write(frame.start, &frame.planes, gain);
        }
        true
    }
}

/// Output samples queued in `out`.
pub(crate) fn queued_samples(out: &OutQueue) -> usize {
    out.first().map_or(0, VecDeque::len)
}

/// Decodes (through `more`, which queues frames into the placer or ends its input) and edits
/// until `want` output samples are queued in `out` or every range is written out.
pub(crate) fn fill(
    placer: &mut Placer,
    window: &mut SourceWindow,
    editor: &mut RangeEditor,
    out: &mut OutQueue,
    gain: f32,
    want: usize,
    mut more: impl FnMut(&mut Placer) -> Result<(), String>,
) -> Result<(), String> {
    while queued_samples(out) < want && !editor.done() {
        let budget = want - queued_samples(out);
        if editor.pump(window, placer.final_end, budget, out)? {
            continue;
        }
        if placer.place(window, gain) {
            continue;
        }
        if placer.input_done {
            placer.final_end = i64::MAX;
        } else {
            more(placer)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "frame_placer_tests.rs"]
mod tests;
