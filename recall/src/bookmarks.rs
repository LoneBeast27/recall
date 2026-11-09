//! Bookmark hotkey: capture-clock marks held for as long as the replay buffer
//! can still contain them, then written beside each saved clip that covers
//! them as `<clip-stem>.recall-marks.json` (`{"version":1,"marks":[s,...]}`,
//! seconds from the clip's first frame).
//!
//! A save is noted with its capture-clock end when it is requested; when the
//! saver reports `Saved{path, seconds}` the clip spans
//! `[end - seconds, end]`, so the marks inside that window are its marks.
//! `path` is the clip's FINAL path even for a staged save (`encoder::staging`),
//! so the sidecar is written straight into the clip folder and is there before
//! the background move lands the clip beside it. The
//! long recording keeps its own marks (`longrec`). MP4 chapters are not
//! written: that would need the saver's muxer, and the sidecar carries the same
//! data for the editor.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const SIDECAR_SUFFIX: &str = ".recall-marks.json";
/// Hard bound on held marks (a stuck key cannot grow memory).
const MAX_MARKS: usize = 1024;
/// Saves awaiting their `Saved` event (failed saves never report a path).
const MAX_PENDING: usize = 8;
/// Marks are kept this much longer than the buffer (save latency slack).
const SLACK_US: u64 = 5_000_000;

#[derive(Default)]
struct State {
    /// Capture-clock microseconds, ascending.
    marks: VecDeque<u64>,
    /// (clip path, capture-clock end of the requested window).
    pending: VecDeque<(String, u64)>,
}

pub struct Bookmarks {
    retain_us: u64,
    state: Mutex<State>,
}

impl Bookmarks {
    pub fn new(buffer_secs: u32) -> Self {
        Bookmarks {
            retain_us: buffer_secs as u64 * 1_000_000 + SLACK_US,
            state: Mutex::new(State::default()),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Record a mark; marks older than the buffer (plus slack) are dropped.
    pub fn add(&self, at_us: u64) {
        let mut s = self.state();
        let insert_at = s.marks.partition_point(|m| *m <= at_us);
        s.marks.insert(insert_at, at_us);
        let newest = s.marks.back().copied().unwrap_or(at_us);
        let oldest_kept = newest.saturating_sub(self.retain_us);
        while s.marks.front().is_some_and(|m| *m < oldest_kept) || s.marks.len() > MAX_MARKS {
            s.marks.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.state().marks.len()
    }

    /// A save of `path` was requested with its window ending at `end_us`.
    pub fn note_save(&self, path: &str, end_us: u64) {
        let mut s = self.state();
        s.pending.push_back((path.to_string(), end_us));
        while s.pending.len() > MAX_PENDING {
            s.pending.pop_front();
        }
    }

    /// The marks of a finished save, relative to its first frame. `None` when
    /// the save was not noted here; `Some(empty)` when no mark is inside it.
    /// `video_end_us`: where the clip's video really ends (a save made after the
    /// game closed ends at its last frame, before the request); else the request's end.
    pub fn take_saved(&self, path: &str, seconds: f64, video_end_us: Option<u64>) -> Option<Vec<f64>> {
        let mut s = self.state();
        // Matched on the file name: the hotkey notes the clip straight in the clip folder and the
        // saver may move it into a game folder, keeping the (unique) file name.
        let name = file_name(path);
        let pos = s.pending.iter().position(|(p, _)| file_name(p) == name)?;
        let (_, requested_end_us) = s.pending.remove(pos)?;
        let end_us = video_end_us.unwrap_or(requested_end_us);
        let span_us = (seconds.max(0.0) * 1_000_000.0).round() as u64;
        Some(window_marks(s.marks.iter().copied(), end_us.saturating_sub(span_us), end_us))
    }
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// Marks inside `[start_us, end_us]`, as seconds from `start_us` rounded to
/// the millisecond, ascending.
pub fn window_marks(marks: impl IntoIterator<Item = u64>, start_us: u64, end_us: u64) -> Vec<f64> {
    let mut out: Vec<f64> = marks
        .into_iter()
        .filter(|m| (start_us..=end_us).contains(m))
        .map(|m| ((m - start_us) as f64 / 1_000.0).round() / 1_000.0)
        .collect();
    out.sort_by(f64::total_cmp);
    out
}

/// `<dir>\<clip stem>.recall-marks.json`.
pub fn sidecar_path(clip: &Path) -> PathBuf {
    let stem = clip.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    clip.with_file_name(format!("{stem}{SIDECAR_SUFFIX}"))
}

pub fn sidecar_json(marks: &[f64]) -> String {
    let list = serde_json::to_string(marks).unwrap_or_else(|_| "[]".into());
    format!(r#"{{"version":1,"marks":{list}}}"#)
}

/// `protocol` observer: on `Saved`, write the clip's marks sidecar (before the
/// event is printed, so a client reading `saved` finds it).
pub fn on_emit(marks: &Bookmarks, msg: &crate::protocol::Outbound) {
    let crate::protocol::Outbound::Saved { path, seconds, video_end_us, .. } = msg else { return };
    let Some(list) = marks.take_saved(path, f64::from(*seconds), *video_end_us) else { return };
    if list.is_empty() {
        return;
    }
    if let Err(e) = write_sidecar(Path::new(path), &list) {
        let message = format!("bookmarks for {path} not written: {e}");
        crate::protocol::emit(&crate::protocol::Outbound::EncoderLog { message });
    }
}

/// One small write beside the clip (never rewritten). Returns the sidecar path.
pub fn write_sidecar(clip: &Path, marks: &[f64]) -> std::io::Result<PathBuf> {
    let path = sidecar_path(clip);
    std::fs::write(&path, sidecar_json(marks))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 1_000_000;

    #[test]
    fn marks_older_than_the_buffer_are_dropped() {
        let b = Bookmarks::new(60);
        b.add(10 * S);
        b.add(40 * S);
        assert_eq!(b.len(), 2);
        b.add(70 * S); // 10 s is still inside 60 s + 5 s slack of 70 s
        assert_eq!(b.len(), 3);
        b.add(80 * S); // now 10 s is 70 s old -> gone
        assert_eq!(b.len(), 3);
        for i in 0..(MAX_MARKS as u64 + 50) {
            b.add(80 * S + i);
        }
        assert_eq!(b.len(), MAX_MARKS, "count bound");
    }

    #[test]
    fn saved_clip_gets_only_marks_inside_its_window() {
        let b = Bookmarks::new(120);
        for at in [5 * S, 31 * S + 250_000, 59 * S, 61 * S] {
            b.add(at);
        }
        b.note_save(r"E:\c\A.mp4", 60 * S);
        b.note_save(r"E:\c\B.mp4", 62 * S);
        // A: a 30 s clip ending at 60 s spans [30, 60].
        assert_eq!(b.take_saved(r"E:\c\A.mp4", 30.0, None), Some(vec![1.25, 29.0]));
        assert_eq!(b.take_saved(r"E:\c\A.mp4", 30.0, None), None, "taken once");
        // B: a 2 s clip ending at 62 s spans [60, 62] -> only the 61 s mark.
        assert_eq!(b.take_saved(r"E:\c\B.mp4", 2.0, None), Some(vec![1.0]));
        assert_eq!(b.take_saved(r"E:\c\never.mp4", 9.0, None), None, "unknown save");
    }

    /// "Sort clips into game folders": the hotkey notes the clip in the clip folder, the saver
    /// writes it into the game folder under the same file name; the marks still land on it.
    #[test]
    fn marks_follow_a_clip_the_saver_put_in_a_game_folder() {
        let b = Bookmarks::new(120);
        b.add(55 * S);
        b.note_save(r"E:\c\VALORANT__2026-11-10__04-00-00-000__ab.mp4", 60 * S);
        let placed = r"E:\c\VALORANT\VALORANT__2026-11-10__04-00-00-000__ab.mp4";
        assert_eq!(b.take_saved(placed, 10.0, None), Some(vec![5.0]));
        assert_eq!(b.take_saved(placed, 10.0, None), None, "taken once");
    }

    /// The game closed at 60 s and the save came 30 s later (the countdown): the
    /// clip ends at the last frame, so a mark made at 50 s, before the close,
    /// sits 10 s before the clip's end, not 40 s before the request.
    #[test]
    fn a_countdown_save_places_marks_against_its_last_frame() {
        let b = Bookmarks::new(120);
        b.add(50 * S);
        let clip = r"E:\c\Game__closed.mp4";
        b.note_save(clip, 90 * S); // the request, 30 s into the countdown
        assert_eq!(b.take_saved(clip, 30.0, Some(60 * S)), Some(vec![20.0]), "[30 s, 60 s]: 50 s is at 20 s");
        b.note_save(clip, 90 * S);
        assert_eq!(b.take_saved(clip, 60.0, Some(60 * S)), Some(vec![50.0]), "the whole buffer, [0, 60 s]");
        // Against the request time the same 30 s clip would be [60 s, 90 s]: the mark is lost.
        b.note_save(clip, 90 * S);
        assert_eq!(b.take_saved(clip, 30.0, None), Some(vec![]));
        // Through the Saved event, as the engine reports it.
        let dir = std::env::temp_dir().join(format!("recall-marks-drain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Game__closed.mp4").to_string_lossy().into_owned();
        b.note_save(&path, 90 * S);
        on_emit(
            &b,
            &crate::protocol::Outbound::Saved {
                path: path.clone(),
                seconds: 30.0,
                frames: 1,
                bytes: 1,
                elapsed_ms: 1,
                staged: false,
                staged_path: None,
                video_end_us: Some(60 * S),
            },
        );
        let sidecar = std::fs::read_to_string(sidecar_path(Path::new(&path))).unwrap();
        assert_eq!(sidecar, sidecar_json(&[20.0]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn window_marks_are_relative_sorted_and_inclusive() {
        let marks = [9 * S, 3 * S, 10 * S, 2 * S - 1];
        assert_eq!(window_marks(marks, 2 * S, 10 * S), vec![1.0, 7.0, 8.0]);
        assert!(window_marks([], 0, 10 * S).is_empty());
    }

    #[test]
    fn sidecar_shape_and_name() {
        let clip = Path::new(r"E:\Clips\VALORANT__2026-10-22__14-05-09-275__ab12.mp4");
        assert_eq!(
            sidecar_path(clip),
            Path::new(r"E:\Clips\VALORANT__2026-10-22__14-05-09-275__ab12.recall-marks.json")
        );
        let v: serde_json::Value = serde_json::from_str(&sidecar_json(&[1.25, 29.0])).unwrap();
        assert_eq!(v, serde_json::json!({"version": 1, "marks": [1.25, 29.0]}));
    }

    #[test]
    fn sidecar_is_written_beside_the_clip() {
        let dir = std::env::temp_dir().join(format!("recall-marks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("G__2026-10-22__14-05-09.mp4");
        let path = write_sidecar(&clip, &[0.5]).unwrap();
        assert_eq!(path, dir.join("G__2026-10-22__14-05-09.recall-marks.json"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"version":1,"marks":[0.5]}"#);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saved_event_writes_the_sidecar_only_for_clips_with_marks() {
        use crate::protocol::Outbound;
        let dir = std::env::temp_dir().join(format!("recall-marks-emit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let with = dir.join("A.mp4").to_string_lossy().into_owned();
        let without = dir.join("B.mp4").to_string_lossy().into_owned();
        let b = Bookmarks::new(60);
        b.add(10 * S);
        b.note_save(&with, 20 * S);
        b.note_save(&without, 40 * S);
        // A staged save: the sidecar goes beside the FINAL path, never into staging.
        let staging = dir.join("staging");
        let saved = |path: &str, seconds: f32| Outbound::Saved {
            path: path.into(),
            seconds,
            frames: 1,
            bytes: 1,
            elapsed_ms: 1,
            staged: true,
            staged_path: Some(staging.join("A.mp4").to_string_lossy().into_owned()),
            video_end_us: None,
        };
        on_emit(&b, &saved(&with, 15.0)); // [5 s, 20 s] holds the 10 s mark
        on_emit(&b, &saved(&without, 5.0)); // [35 s, 40 s] holds none
        on_emit(&b, &Outbound::EncoderLog { message: "ignored".into() });
        assert!(!staging.exists(), "no sidecar in the staging folder");
        assert_eq!(std::fs::read_to_string(dir.join("A.recall-marks.json")).unwrap(), r#"{"version":1,"marks":[5.0]}"#);
        assert!(!dir.join("B.recall-marks.json").exists(), "no marks, no file");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
