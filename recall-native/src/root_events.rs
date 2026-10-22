//! Controller events → toasts and library refreshes.
use super::{Root, ToastKind};
use crate::{
    services::{
        daemon::DaemonEvent,
        library::{format_bytes, format_duration, normalize_game},
        storage::{Interaction, record_interaction},
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
            // A clip saved to the SSD first appears in the library once it is in the clip folder.
            DaemonEvent::ClipMoved { .. } => self.workers.job(Job::List),
            DaemonEvent::ClipMoveFailed { reason, .. } => self.show_toast(
                ToastKind::Bad,
                "Couldn't move a clip to the clip folder",
                Some(format!("It's safe on the SSD and Recall will retry. {reason}")),
            ),
            DaemonEvent::Error(message) => self.show_toast(ToastKind::Bad, "Recorder", Some(message)),
            DaemonEvent::MicMuted(_) | DaemonEvent::ExportStarted { .. } | DaemonEvent::Exported { .. } => {}
        }
    }
}
