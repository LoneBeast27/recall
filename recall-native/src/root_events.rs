//! Controller events → toasts and library refreshes.
use super::{Root, ToastKind};
use crate::{
    services::{
        daemon::DaemonEvent,
        library::{format_bytes, format_duration, normalize_game, watch::LibraryChange},
        storage::{GoneClip, Interaction, record_interaction},
    },
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
            DaemonEvent::StorageCleaned { removed, freed_bytes } => {
                let clips = if removed == 1 { "1 old clip".to_owned() } else { format!("{removed} old clips") };
                self.show_toast(ToastKind::Info, format!("Moved {clips} to the Recycle Bin"), Some(format!("Storage limit · {} freed", format_bytes(freed_bytes))));
                self.workers.job(Job::List);
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
            DaemonEvent::Error(message) => self.show_toast(ToastKind::Bad, "Recorder", Some(message)),
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
