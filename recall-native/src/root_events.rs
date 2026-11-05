//! Controller events → toasts and library refreshes.
use super::{Root, ToastKind};
use crate::{
    services::{
        daemon::DaemonEvent,
        library::{format_bytes, format_duration, local_from_unix, normalize_game, watch::LibraryChange},
        storage::{GoneClip, Interaction, record_interaction},
    },
    views::storage_limit,
    worker::Job,
};

impl Root {
    pub(super) fn on_daemon_event(&mut self, event: DaemonEvent) {
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
            DaemonEvent::RecordingStarted => self.show_toast(ToastKind::Info, "Recording started", Some("Press the same shortcut again to stop.".into())),
            DaemonEvent::RecordingStopped { path, seconds } => {
                record_interaction(Interaction::Saved(path));
                self.show_toast(ToastKind::Good, format!("Recording saved · {}", format_duration(seconds)), None);
                self.workers.job(Job::List);
            }
            DaemonEvent::RecordingFailed { reason } => self.show_toast(ToastKind::Bad, "Recording failed", Some(reason)),
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
            DaemonEvent::StoragePreview { cap_gb, used_bytes, removed, freed_bytes, through } => {
                // Only the answer for the limit still being asked about counts.
                if self.storage_asking == Some(cap_gb) {
                    self.storage_asking = None;
                    if removed == 0 {
                        self.set_config("storage", "cap_gb", serde_json::json!(cap_gb));
                    } else {
                        let through = through.map(local_from_unix);
                        self.storage_confirm = Some(storage_limit::Preview { cap_gb, used_bytes, removed, freed_bytes, through });
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
                if message.starts_with("storage preview") || message.starts_with("storage_preview") {
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
