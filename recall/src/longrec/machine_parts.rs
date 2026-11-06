//! A long recording in parts. When the live encoder reopens (the capture size
//! or the frame path changed) its packets cannot go on in the same file: the
//! part ends (`Continued`) and the recording goes on in the next part
//! (`naming::longrec_next_part`, `__rec-2.mp4`, ...), which starts on the new
//! encoder's first keyframe (`PartStarted`). A stop, arm timeout or failure
//! before that names the finished part.
//!
//! At most one new part per [`PART_GAP`]: a reopen inside that window (a size
//! flap, a window being dragged) waits for the gap to pass, and every further
//! reopen while waiting holds the next part until the stream has been steady
//! for [`SETTLE`].

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{finish, Event, Phase, Recorder, SinkFactory};
use crate::naming;

/// A new part starts at most this often.
pub const PART_GAP: Duration = Duration::from_secs(10);
/// After a reopen while waiting for the next part, the stream must stay put this long.
pub const SETTLE: Duration = Duration::from_secs(2);

/// Why the live encoder reopened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// The capture size changed (a windowed game resized, a resolution change).
    Size,
    /// The frame path changed (GPU frames <-> CPU copies), same size.
    Input,
    /// The encoder reopened with the same stream shape.
    Encoder,
}

impl Cause {
    /// The protocol's `cause` value.
    pub fn as_str(self) -> &'static str {
        match self {
            Cause::Size => "size",
            Cause::Input => "input",
            Cause::Encoder => "encoder",
        }
    }

    fn log_text(self) -> &'static str {
        match self {
            Cause::Size => "capture size changed",
            Cause::Input => "capture input changed (GPU / CPU frames)",
            Cause::Encoder => "video encoder reopened",
        }
    }
}

/// The finished part a continued recording follows.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Series {
    pub path: PathBuf,
    pub seconds: f64,
    pub bytes: u64,
    pub marks: usize,
    /// The finished part's number and the seconds of every part so far.
    pub part: u32,
    pub total_seconds: f64,
    /// The next part opens on the first keyframe at or after this.
    pub not_before: Instant,
    /// Why the stream changed (the newest reopen's).
    pub cause: Cause,
}

impl Series {
    /// The recording stopped between two parts: it is the finished parts.
    pub fn stopped(self) -> Event {
        let Series { path, seconds, bytes, marks, part, total_seconds, .. } = self;
        Event::Stopped { path, seconds, bytes, marks, parts: part, total_seconds }
    }
}

/// The arm timeout runs from the request, or for a next part from when it may start.
pub(super) fn arm_wait_from(requested: Instant, previous: &Option<Series>) -> Instant {
    previous.as_ref().map_or(requested, |p| p.not_before.max(requested))
}

impl<F: SinkFactory> Recorder<F> {
    /// The live encoder reopened (`cause`) with these settings (`video`, its
    /// generation's own). A running part ends here and the recording arms the
    /// next part; its audio keeps the first part's wall-clock mapping. A part
    /// that fails to finish ends the recording.
    pub fn encoder_reopened(&mut self, now: Instant, cause: Cause, video: Option<F::Video>) -> Vec<Event> {
        self.video = video;
        match &mut self.phase {
            Phase::Idle | Phase::Armed { previous: None, .. } => Vec::new(),
            // Waiting for the next part: the stream is still changing.
            Phase::Armed { previous: Some(s), .. } => {
                s.cause = cause;
                s.not_before = s.not_before.max(now + SETTLE);
                Vec::new()
            }
            Phase::Recording(_) => self.next_part(now, cause),
        }
    }

    fn next_part(&mut self, now: Instant, cause: Cause) -> Vec<Event> {
        let Phase::Recording(active) = std::mem::replace(&mut self.phase, Phase::Idle) else { return Vec::new() };
        let start = Some((active.base_us, active.wall_base));
        let began = active.began;
        let mut events = vec![Event::Log(format!("{}; the long recording continues in a new file", cause.log_text()))];
        let mut ended = finish(*active);
        let last = ended.pop();
        events.extend(ended);
        let Some(Event::Stopped { path, seconds, bytes, marks, parts, total_seconds }) = last else {
            events.extend(last);
            return events;
        };
        let next = naming::longrec_next_part(&path, Path::exists);
        events.push(Event::Continued { path: path.clone(), seconds, bytes, marks, part: parts, total_seconds, cause });
        // Inside the gap since this part began: wait for it, and for the stream to settle.
        let not_before = if now < began + PART_GAP { (began + PART_GAP).max(now + SETTLE) } else { now };
        let previous = Some(Series { path, seconds, bytes, marks, part: parts, total_seconds, not_before, cause });
        self.phase = Phase::Armed { path: next, start, requested: now, previous };
        events
    }

    /// The live encoder failed for good: no more video will come. A running
    /// recording is finished as it stands and reported as `Failed` naming the
    /// saved file; an armed one fails.
    pub fn encoder_failed(&mut self, reason: &str) -> Vec<Event> {
        let reason = format!("the video encoder failed ({reason})");
        match &self.phase {
            Phase::Idle => Vec::new(),
            Phase::Armed { previous, .. } => {
                let kept = previous.as_ref().map(|p| p.path.clone());
                self.phase = Phase::Idle;
                vec![Event::Failed { reason, kept }]
            }
            Phase::Recording(_) => self
                .stop()
                .into_iter()
                .map(|e| match e {
                    Event::Stopped { path, .. } => Event::Failed { reason: format!("{reason}; saved up to here"), kept: Some(path) },
                    other => other,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
#[path = "machine_parts_tests.rs"]
mod tests;
