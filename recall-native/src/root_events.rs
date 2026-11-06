//! Controller events → toasts and library refreshes.
use super::{Root, ToastKind};
use crate::{
    services::{
        daemon::DaemonEvent,
        library::{format_duration, local_from_unix, normalize_game, watch::LibraryChange},
        storage::{GoneClip, Interaction, record_interaction},
    },
    views::storage_limit,
    worker::Job,
};

/// The long recording's toast for `event`: one per change. A continued part's stop
/// shows none (the next part's start says "continues in part N"); the final stop
/// shows the whole length and, past one file, how many it took; time missed between parts
/// or frozen (a same-header encoder reopen) past 2 s is named.
fn recording_toast(event: &DaemonEvent) -> Option<(ToastKind, String, Option<String>)> {
    match event {
        DaemonEvent::RecordingStarted => Some((ToastKind::Info, "Recording started".into(), Some("Press the same shortcut again to stop.".into()))),
        DaemonEvent::RecordingContinued { part, cause } => Some((ToastKind::Info, format!("Recording continues in part {part}"), Some(cause_text(cause).into()))),
        DaemonEvent::RecordingStopped { seconds, parts, gap_seconds, frozen_seconds, .. } => {
            let mut notes = Vec::new();
            if *parts > 1 {
                notes.push(format!("{parts} files"));
            }
            notes.extend(lost_notes(*parts > 1, *gap_seconds, *frozen_seconds));
            let detail = (!notes.is_empty()).then(|| notes.join(" · "));
            Some((ToastKind::Good, format!("Recording saved · {}", format_duration(*seconds)), detail))
        }
        DaemonEvent::RecordingFailed { reason, files, .. } if files.is_empty() => Some((ToastKind::Bad, "Recording failed".into(), Some(reason.clone()))),
        DaemonEvent::RecordingFailed { reason, files, gap_seconds, frozen_seconds } => {
            let mut detail = format!("{reason}. {}", saved(files));
            for note in lost_notes(files.len() > 1, *gap_seconds, *frozen_seconds) {
                detail.push_str(&format!(" · {note}"));
            }
            Some((ToastKind::Bad, "Recording ended early".into(), Some(detail)))
        }
        // A `.part` left by a kill (a hung engine restarted mid-recording, a crash), renamed
        // into place on the next start.
        DaemonEvent::RecordingRecovered { path, gap_seconds, frozen_seconds } => {
            let name = std::path::Path::new(path).file_name().map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
            let mut detail = format!("Recovered {name}");
            for note in lost_notes(false, *gap_seconds, *frozen_seconds) {
                detail.push_str(&format!(" · {note}"));
            }
            Some((ToastKind::Bad, "Recording ended early".into(), Some(detail)))
        }
        _ => None,
    }
}

/// Time missed between parts, or frozen, is shown past this (seconds).
const MISSING_OVER: f64 = 2.0;

/// "7 s missing" (only between several parts) and "40 s frozen", each past [`MISSING_OVER`].
fn lost_notes(several: bool, gap_seconds: f64, frozen_seconds: f64) -> Vec<String> {
    let mut notes = Vec::new();
    if several && gap_seconds > MISSING_OVER {
        notes.push(format!("{} missing", missing(gap_seconds)));
    }
    if frozen_seconds > MISSING_OVER {
        notes.push(format!("{} frozen", missing(frozen_seconds)));
    }
    notes
}

/// "7 s", or "1:05" from a minute.
fn missing(secs: f64) -> String {
    if secs.round() < 60.0 { format!("{} s", secs.round() as u64) } else { format_duration(secs) }
}

/// "Saved a.mp4", "Saved a.mp4, b.mp4", "Saved 3 files, a.mp4 … c.mp4".
fn saved(files: &[String]) -> String {
    let name = |p: &String| std::path::Path::new(p).file_name().map_or_else(|| p.clone(), |n| n.to_string_lossy().into_owned());
    match files {
        [one] => format!("Saved {}", name(one)),
        [a, b] => format!("Saved {}, {}", name(a), name(b)),
        [a, .., z] => format!("Saved {} files, {} … {}", files.len(), name(a), name(z)),
        [] => "Nothing was saved".into(),
    }
}

/// What changed, from the engine's `cause`.
fn cause_text(cause: &str) -> &'static str {
    match cause {
        "size" => "The game's size changed",
        "input" => "Capture switched between GPU and CPU frames",
        "encoder" => "The video encoder restarted",
        _ => "The capture changed",
    }
}

impl Root {
    pub(super) fn on_daemon_event(&mut self, event: DaemonEvent) {
        if let Some((kind, text, detail)) = recording_toast(&event) {
            self.show_toast(kind, text, detail);
        }
        match event {
            DaemonEvent::Saved { path, seconds, .. } => {
                record_interaction(Interaction::Saved(path.clone()));
                let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned());
                let game = name.as_deref().and_then(|n| n.split("__").next()).map(|g| normalize_game(g).1);
                self.show_toast(ToastKind::Good, format!("Clip saved · {}", format_duration(seconds as f64)), game);
                self.workers.job(Job::List);
            }
            DaemonEvent::SaveFailed { reason } => self.show_toast(ToastKind::Bad, "Save failed", Some(reason)),
            DaemonEvent::ExportFailed { reason, .. } => self.show_toast(ToastKind::Bad, "Export failed", Some(reason)),
            DaemonEvent::AcProfile { ok, profile, error } => {
                if ok {
                    self.ac_profile = profile;
                    self.show_toast(ToastKind::Good, "Colour profile set", Some("Applies the next time HDR turns on".into()));
                } else {
                    self.show_toast(ToastKind::Bad, "Colour profile failed", error);
                }
            }
            DaemonEvent::Bookmarked => self.show_toast(ToastKind::Info, "Bookmark added", Some("Your next clip shows it on the timeline.".into())),
            DaemonEvent::Screenshot { path } => {
                let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned());
                self.show_toast(ToastKind::Good, "Screenshot saved", name);
            }
            DaemonEvent::ScreenshotFailed { reason } => self.show_toast(ToastKind::Bad, "Screenshot failed", Some(reason)),
            // Toasts: `recording_toast` above.
            DaemonEvent::RecordingStarted | DaemonEvent::RecordingContinued { .. } => {}
            DaemonEvent::RecordingStopped { path, .. } | DaemonEvent::RecordingPartSaved { path } => {
                record_interaction(Interaction::Saved(path));
                self.workers.job(Job::List);
            }
            DaemonEvent::RecordingRecovered { path, .. } => {
                // Toast: `recording_toast`. The recovered file reaches the library.
                record_interaction(Interaction::Saved(path));
                self.workers.job(Job::List);
            }
            DaemonEvent::RecordingFailed { files, .. } => {
                // Toast: `recording_toast`. Kept files reach the library.
                for path in files {
                    record_interaction(Interaction::Saved(path));
                }
                self.workers.job(Job::List);
            }
            DaemonEvent::StorageCleaned { removed, freed_bytes, through, cap_bytes, .. } => {
                let today = crate::services::library::today();
                let (title, detail) = storage_limit::cleaned(removed, freed_bytes, through.map(local_from_unix), cap_bytes, today);
                self.show_toast_for(ToastKind::Info, title, Some(detail), storage_limit::TOAST_SECS);
                self.workers.job(Job::List);
            }
            DaemonEvent::StorageWarning { level, used_bytes, cap_bytes, next_through } => {
                let today = crate::services::library::today();
                let (title, detail) = storage_limit::warning(level, used_bytes, cap_bytes, next_through.map(local_from_unix), today);
                let kind = if level >= 100 { ToastKind::Bad } else { ToastKind::Info };
                self.show_toast_for(kind, title, Some(detail), storage_limit::TOAST_SECS);
            }
            DaemonEvent::DiskLow { drive, free_bytes, need_bytes } => {
                let (title, detail) = storage_limit::disk_low(&drive, free_bytes, need_bytes);
                self.show_toast_for(ToastKind::Bad, title, Some(detail), storage_limit::TOAST_SECS);
            }
            DaemonEvent::StoragePreview { cap_gb, used_bytes, removed, freed_bytes, through } => {
                // Only the answer for the limit still being asked about counts.
                if self.storage_asking.is_some_and(|(asked, _)| asked == cap_gb) {
                    self.storage_asking = None;
                    if removed == 0 {
                        self.set_config("storage", "cap_gb", serde_json::json!(cap_gb));
                    } else {
                        let through = through.map(local_from_unix);
                        self.storage_confirm = Some(storage_limit::Preview { cap_gb, used_bytes, removed, freed_bytes, through, at: std::time::Instant::now() });
                    }
                }
            }
            // A clip saved to the SSD first appears in the library once it is in the clip folder;
            // the engine's kept SSD copy becomes its video-cache entry, so it opens from the SSD.
            DaemonEvent::ClipMoved { path, kept_path } => {
                if let Some(kept) = kept_path {
                    crate::services::storage::adopt_kept(path, kept);
                }
                self.workers.job(Job::List);
            }
            DaemonEvent::ClipMoveFailed { reason, .. } => self.show_toast(
                ToastKind::Bad,
                "Couldn't move a clip to the clip folder",
                Some(format!("It's safe on the SSD and Recall will retry. {reason}")),
            ),
            DaemonEvent::Error(message) => {
                // A dry run that failed (favourites unknown, folder unreadable) sets nothing.
                // An older recorder answers "unknown command 'storage_preview'".
                if message.starts_with("storage preview") || message.starts_with("storage_preview") || message.contains("'storage_preview'") {
                    self.storage_asking = None;
                }
                self.show_toast(ToastKind::Bad, "Recorder", Some(message));
            }
            DaemonEvent::MicMuted(_) | DaemonEvent::ExportStarted { .. } | DaemonEvent::Exported { .. } => {}
        }
    }

    /// The clip folders changed behind the app's back (`services::library::watch`).
    pub(super) fn on_library_change(&mut self, change: LibraryChange) {
        match change {
            LibraryChange::Clips(changes) => {
                crate::trace::note(&format!("watch: +{} -{} -{}", changes.added.len(), changes.gone.len(), changes.gone_under.len()));
                let retired = self.lib.apply_changes(changes);
                let gone = retired
                    .into_iter()
                    .map(|r| GoneClip { name: r.entry.name, bytes: r.entry.bytes, mtime_secs: r.entry.mtime, removed: r.removed })
                    .collect();
                self.workers.forget_clips(gone);
                self.workers.job(Job::DriveSpace);
                if self.tile_menu.as_ref().is_some_and(|menu| !self.lib.lists(&menu.path)) {
                    self.tile_menu = None;
                }
            }
            LibraryChange::Resync => {
                self.lib.forget_recent();
                self.workers.job(Job::List);
            }
            LibraryChange::RootsChanged => self.lib.forget_recent(),
            LibraryChange::FolderLost(folder) => self.show_toast(ToastKind::Bad, "A clip folder is unavailable", Some(folder)),
            LibraryChange::FolderBack(folder) => self.show_toast(ToastKind::Good, "A clip folder is back", Some(folder)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DaemonEvent, ToastKind, recording_toast};

    /// Merge gate 11-07 (P1/P2): Alt+Enter at minute 59 toasted "Recording saved · 59:00" then
    /// "Recording started"; the final toast gave the last part's length only.
    #[test]
    fn a_continued_recording_toasts_once_and_the_final_stop_gives_the_whole_length() {
        assert_eq!(recording_toast(&DaemonEvent::RecordingPartSaved { path: "E:/G__rec.mp4".into() }), None, "a part's stop is quiet");
        let toast = recording_toast(&DaemonEvent::RecordingContinued { part: 2, cause: "size".into() });
        assert_eq!(toast, Some((ToastKind::Info, "Recording continues in part 2".into(), Some("The game's size changed".into()))));
        let toast = recording_toast(&DaemonEvent::RecordingContinued { part: 3, cause: "input".into() });
        assert_eq!(toast.and_then(|t| t.2).as_deref(), Some("Capture switched between GPU and CPU frames"), "the reason matches the cause");
        let toast = recording_toast(&DaemonEvent::RecordingStopped { path: "E:/G__rec-3.mp4".into(), seconds: 3725.0, parts: 3, gap_seconds: 0.4, frozen_seconds: 0.0 });
        assert_eq!(toast, Some((ToastKind::Good, "Recording saved · 1:02:05".into(), Some("3 files".into()))));
        let toast = recording_toast(&DaemonEvent::RecordingStopped { path: "E:/G__rec.mp4".into(), seconds: 61.0, parts: 1, gap_seconds: 0.0, frozen_seconds: 0.0 });
        assert_eq!(toast, Some((ToastKind::Good, "Recording saved · 1:01".into(), None)), "one file reads as before");
    }

    /// Merge gate 11-07 #2/#3 (P1): the toast said nothing of time missed between parts, and a
    /// failure after a part change neither said it ended early nor named the files.
    #[test]
    fn the_final_toast_says_what_is_missing_and_a_failure_names_the_files() {
        let toast = recording_toast(&DaemonEvent::RecordingStopped { path: "E:/G__rec-2.mp4".into(), seconds: 3600.0, parts: 2, gap_seconds: 7.2, frozen_seconds: 0.0 });
        assert_eq!(toast, Some((ToastKind::Good, "Recording saved · 1:00:00".into(), Some("2 files · 7 s missing".into()))));
        let files = vec!["E:/c/G__rec.mp4".to_string(), "E:/c/G__rec-2.mp4".to_string()];
        let toast = recording_toast(&DaemonEvent::RecordingFailed { reason: "the video encoder failed".into(), files: files.clone(), gap_seconds: 0.0, frozen_seconds: 0.0 });
        let expected = "the video encoder failed. Saved G__rec.mp4, G__rec-2.mp4";
        assert_eq!(toast, Some((ToastKind::Bad, "Recording ended early".into(), Some(expected.into()))));
        // Review 11-07: an early end names what was lost too, as the saved toast does.
        let toast = recording_toast(&DaemonEvent::RecordingFailed { reason: "the video encoder failed".into(), files, gap_seconds: 7.2, frozen_seconds: 40.0 });
        let expected = "the video encoder failed. Saved G__rec.mp4, G__rec-2.mp4 · 7 s missing · 40 s frozen";
        assert_eq!(toast, Some((ToastKind::Bad, "Recording ended early".into(), Some(expected.into()))));
        let toast = recording_toast(&DaemonEvent::RecordingFailed { reason: "no keyframe".into(), files: Vec::new(), gap_seconds: 0.0, frozen_seconds: 0.0 });
        assert_eq!(toast, Some((ToastKind::Bad, "Recording failed".into(), Some("no keyframe".into()))));
    }

    /// Gate P2-d: a recording recovered from its `.part` on the next start said nothing.
    #[test]
    fn a_recovered_recording_says_it_ended_early_and_names_the_file() {
        let toast = recording_toast(&DaemonEvent::RecordingRecovered { path: "E:/c/G__rec.mp4".into(), gap_seconds: 0.0, frozen_seconds: 0.0 });
        assert_eq!(toast, Some((ToastKind::Bad, "Recording ended early".into(), Some("Recovered G__rec.mp4".into()))));
        let toast = recording_toast(&DaemonEvent::RecordingRecovered { path: "E:/c/G__rec.mp4".into(), gap_seconds: 7.2, frozen_seconds: 40.0 });
        assert_eq!(toast.and_then(|t| t.2).as_deref(), Some("Recovered G__rec.mp4 · 40 s frozen"), "frozen time named; a gap needs several parts");
    }

    /// Merge gate 11-07 #3 (P1): a same-header encoder reopen held the picture still until its
    /// keyframe, and the toast never said so.
    #[test]
    fn the_final_toast_says_how_long_the_picture_froze() {
        let stop = |parts: u32, gap_seconds: f64, frozen_seconds: f64| {
            recording_toast(&DaemonEvent::RecordingStopped { path: "E:/G__rec.mp4".into(), seconds: 3600.0, parts, gap_seconds, frozen_seconds }).and_then(|t| t.2)
        };
        assert_eq!(stop(1, 0.0, 40.2).as_deref(), Some("40 s frozen"));
        assert_eq!(stop(2, 7.2, 40.2).as_deref(), Some("2 files · 7 s missing · 40 s frozen"));
        assert_eq!(stop(2, 0.0, 95.0).as_deref(), Some("2 files · 1:35 frozen"));
        assert_eq!(stop(1, 0.0, 1.5), None, "a short freeze is not news");
    }
}
