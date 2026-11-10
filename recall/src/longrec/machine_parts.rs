//! A long recording in parts. When the live encoder reopens (the capture size
//! or the frame path changed) its packets cannot go on in the same file: the
//! part ends (`Continued`) and the recording goes on in the next part
//! (`naming::longrec_next_part`, `__rec-2.mp4`, ...), which starts on the new
//! encoder's first keyframe (`PartStarted`). A stop, arm timeout or failure
//! before that names the finished part.
//!
//! At most one new part per [`PART_GAP`], and never later than that: the next
//! part may start [`PART_GAP`] after the last one began, and starts by
//! [`PART_GAP`] after the reopen that ended it whatever follows. Further reopens
//! while it waits (a size flap, a window being dragged) only move it to the
//! newest size. A reopen whose stream headers match (`Cause::Same`, e.g. GPU
//! frames to CPU copies at the same size) needs no new file at all; the time from
//! the old encoder's encode error to the new encoder's first keyframe (the last
//! frame held on screen through the error and its recovery) is reported as
//! `frozen_seconds`. With no error known (a reopen nothing failed before) it runs
//! from the old encoder's last packet. Gaps between packets with no reopen are
//! not counted: the encoder is VFR and a static screen sends nothing.

use crate::encoder::in_progress;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{finish, Event, Phase, Recorder, SinkFactory};
use crate::naming;

/// A new part starts at most this often, and at most this long after the reopen that ended a part.
pub const PART_GAP: Duration = Duration::from_secs(10);

/// Why the live encoder reopened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// The capture size changed (a windowed game resized, a resolution change).
    Size,
    /// The frame path changed (GPU frames <-> CPU copies), same size.
    Input,
    /// The encoder reopened with the same stream shape.
    Encoder,
    /// Same codec, size and parameter sets: the packets go on in the same file.
    Same,
}

impl Cause {
    /// The protocol's `cause` value.
    pub fn as_str(self) -> &'static str {
        match self {
            Cause::Size => "size",
            Cause::Input => "input",
            Cause::Encoder => "encoder",
            Cause::Same => "same",
        }
    }

    fn log_text(self) -> &'static str {
        match self {
            Cause::Size => "capture size changed",
            Cause::Input => "capture input changed (GPU / CPU frames)",
            Cause::Encoder => "video encoder reopened",
            Cause::Same => "video encoder reopened with the same stream",
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
    /// Capture µs where the finished part ended (None if unknown) and the seconds missed so far.
    pub end_us: Option<u64>,
    pub gap_seconds: f64,
    /// Seconds frozen so far (same-header reopens waiting for a keyframe).
    pub frozen_seconds: f64,
    /// The next part opens on the first keyframe at or after this (fixed at the first reopen).
    pub not_before: Instant,
    /// Why the stream changed (the newest reopen's).
    pub cause: Cause,
}

impl Series {
    /// The recording stopped between two parts: it is the finished parts.
    pub fn stopped(self) -> Event {
        let Series { path, seconds, bytes, marks, part, total_seconds, gap_seconds, frozen_seconds, .. } = self;
        Event::Stopped { path, seconds, bytes, marks, parts: part, total_seconds, gap_seconds, frozen_seconds }
    }
}

/// The arm timeout runs from the request, or for a next part from when it may start.
pub(super) fn arm_wait_from(requested: Instant, previous: &Option<Series>) -> Instant {
    previous.as_ref().map_or(requested, |p| p.not_before.max(requested))
}

impl<F: SinkFactory> Recorder<F> {
    /// The live encoder reopened (`cause`) at `now` with these settings (`video`, its
    /// generation's own). A running part ends here and the recording arms the
    /// next part; its audio keeps the first part's wall-clock mapping. A part
    /// that fails to finish ends the recording.
    pub fn encoder_reopened(&mut self, now: Instant, cause: Cause, video: Option<F::Video>) -> Vec<Event> {
        self.video = video;
        match &mut self.phase {
            Phase::Idle | Phase::Armed { previous: None, .. } => Vec::new(),
            // Waiting for the next part: it starts at the newest size, on time.
            Phase::Armed { previous: Some(s), .. } => {
                if cause != Cause::Same {
                    s.cause = cause;
                }
                Vec::new()
            }
            // Headers match: same file, from the new encoder's first keyframe. The old
            // encoder's last packet holds until then (an encode error and its recovery, up to
            // the 5th failed reopen): from the error on, that span is counted as frozen (a
            // static screen before it sent nothing and was not). No error known: from the
            // last packet.
            Phase::Recording(a) if cause == Cause::Same => {
                a.need_key = true;
                let last = a.held.as_ref().map(|h| h.0).or(a.last_pts);
                let error = a.error_us.take();
                if let Some(from) = error.map(|e| last.map_or(e, |l| e.max(l))).or(last) {
                    a.freeze_from.get_or_insert(from);
                }
                vec![Event::Log(format!("{}; the long recording goes on in the same file", cause.log_text()))]
            }
            Phase::Recording(_) => self.next_part(now, cause),
        }
    }

    /// The live encoder had an encode error at `at` (wall clock, mapped onto the capture
    /// clock through this part's first frame): a same-header reopen after it counts the
    /// freeze from here. The first error before a reopen is kept.
    pub fn encode_error(&mut self, at: Instant) {
        if let Phase::Recording(a) = &mut self.phase {
            let us = match at >= a.wall_base {
                true => a.base_us + at.duration_since(a.wall_base).as_micros() as u64,
                false => a.base_us.saturating_sub(a.wall_base.duration_since(at).as_micros() as u64),
            };
            a.error_us.get_or_insert(us.max(a.base_us));
        }
    }

    /// (gap, frozen) seconds of the running recording so far ((0, 0) when idle).
    pub fn missed(&self) -> (f64, f64) {
        match &self.phase {
            Phase::Recording(a) => (a.earlier_gap, a.earlier_frozen + a.frozen_us as f64 / 1e6),
            Phase::Armed { previous: Some(s), .. } => (s.gap_seconds, s.frozen_seconds),
            _ => (0.0, 0.0),
        }
    }

    fn next_part(&mut self, now: Instant, cause: Cause) -> Vec<Event> {
        let Phase::Recording(active) = std::mem::replace(&mut self.phase, Phase::Idle) else { return Vec::new() };
        let start = Some((active.base_us, active.wall_base));
        let began = active.began;
        let base_us = active.base_us;
        let mut events = vec![Event::Log(format!("{}; the long recording continues in a new file", cause.log_text()))];
        let mut ended = finish(*active);
        let last = ended.pop();
        events.extend(ended);
        let Some(Event::Stopped { path, seconds, bytes, marks, parts, total_seconds, gap_seconds, frozen_seconds }) = last else {
            events.extend(last);
            return events;
        };
        let next = naming::longrec_next_part(&path, Path::exists);
        events.push(Event::Continued { path: path.clone(), seconds, bytes, marks, part: parts, total_seconds, cause });
        // Inside the gap since this part began: wait for it (fixed now: later reopens never push it).
        let not_before = (began + PART_GAP).max(now);
        let end_us = Some(base_us + (seconds * 1e6) as u64);
        let previous = Some(Series { path, seconds, bytes, marks, part: parts, total_seconds, end_us, gap_seconds, frozen_seconds, not_before, cause });
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

/// Rename a partial recording into place (its bookmarks from the in-progress list beside it);
/// `None` when that fails too (it then stays listed, for the next start).
pub(super) fn keep_partial(part: &Path) -> Option<PathBuf> {
    let target = crate::naming::recovered_path(part, Path::exists);
    // Bookmarks first: a crash between the rename and the unlisting must not lose them.
    if let Some(entry) = in_progress::get(part) {
        super::super::recover::write_marks(&target, &entry, None, &mut Vec::new());
    }
    std::fs::rename(part, &target).ok()?;
    // No event to carry a failure here: the kept file just stays uncounted.
    let _ = crate::game_folder_rule::record_saved(&target, Some(part));
    unlist_part(part);
    Some(target)
}

/// `part` joins the in-progress list (its first frame at capture µs `base_us`). On a failure
/// the recording still goes on (refusing it would lose it for sure), and the next start runs
/// the one-time scan again, which finds a crashed part in the clip folder; one log line.
pub(super) fn list_part(part: &Path, base_us: u64) -> Option<Event> {
    let entry = in_progress::Entry { base_us: Some(base_us), ..in_progress::Entry::long(part) };
    let e = in_progress::add(entry).err()?;
    let rescan = match in_progress::request_legacy_scan() {
        Ok(()) => "the next start scans for it".to_string(),
        Err(r) => format!("and the next start's scan could not be asked for: {r}"),
    };
    Some(Event::Log(format!("long recording {} not listed for recovery ({e}); {rescan}", part.display())))
}

/// A bookmark at capture µs `at_us`, kept in the list too (a crash keeps it).
pub(super) fn list_mark(part: &Path, at_us: u64) {
    let _ = in_progress::update(part, |e| e.marks.push(at_us));
}

pub(super) fn unlist_part(part: &Path) {
    let _ = in_progress::remove(part);
}

/// A part that never got a video packet: when its file holds nothing of the recording, a
/// diagnostic line and the Recycle Bin (never a permanent delete); anything else stays.
pub(crate) fn dispose_empty_part(part: &Path) -> Vec<Event> {
    unlist_part(part);
    if !part.exists() {
        return Vec::new();
    }
    match crate::encoder::part_recover::layout(part) {
        Ok(crate::encoder::part_recover::Layout::NoPackets) => {
            crate::encoder::recycle::dispose_unrecoverable(part, "no video packet was written").into_iter().map(Event::Log).collect()
        }
        _ => vec![Event::Log(format!("long recording {} holds data but no video packet went in; kept as it is", part.display()))],
    }
}

/// A game-folder marker update for `path` (`record_saved` / `record_writing`): a failure is one
/// log line (the file stays unowned, never lost).
pub(super) fn note(result: std::io::Result<()>, path: &Path) -> Option<Event> {
    let e = result.err()?;
    Some(Event::Log(format!("long recording {} not recorded in its game folder ({e}); it stays unowned", path.display())))
}
