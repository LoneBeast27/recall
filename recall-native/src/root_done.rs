//! Worker results (`worker::Done`) applied to the view: one flat dispatch over the variants.
use super::{Root, ToastKind};
use crate::worker::Done;

impl Root {
    pub(super) fn on_done(&mut self, done: Done) {
        match done {
            Done::Clips(Ok(listing)) => {
                crate::startup::mark(&format!("library listed ({} clips)", listing.entries.len()));
                self.lib.set_entries(listing)
            }
            Done::Drive(drive) => self.lib.drive = drive,
            Done::Ssd(usage) => self.lib.ssd.set(usage),
            Done::Streaming(generation, clip) => self.ed.apply_streaming(generation, clip),
            Done::OpenStep(generation, text) => self.ed.open_step(generation, text),
            Done::Library(change) => self.on_library_change(change),
            Done::Clips(Err(error)) => {
                self.lib.loading = false;
                self.lib.error = Some(error);
            }
            Done::Prepared(generation, result) => {
                self.ed.apply_prepared(generation, result);
                // Clips on a hard drive get an SSD copy in the background; playback swaps to it.
                if let Some(info) = self.ed.info.as_ref() {
                    let on_hdd = crate::services::storage::drive_kind(std::path::Path::new(&info.path)).is_hdd();
                    if on_hdd && self.ed.playback_path.as_deref() == Some(info.path.as_str()) {
                        self.workers.asset(crate::worker::AssetJob::CacheVideo(info.path.clone()));
                    }
                }
            }
            Done::Saved(result) => self.ed.on_saved(result),
            Done::Exported(result) => {
                match &result {
                    Err(error) if error == crate::backend::EXPORT_CANCELLED => self.show_toast(ToastKind::Info, "Export cancelled", None),
                    Err(error) => self.show_toast(ToastKind::Bad, "Export failed", Some(error.clone())),
                    Ok(_) => self.show_toast(ToastKind::Good, "Export finished", None),
                }
                self.ed.on_exported(result);
            }
            Done::Deleted(paths, Ok((recycled, hidden))) => {
                self.lib.deleting = 0;
                self.lib.forget(&paths);
                let (title, detail) = crate::services::library::delete_message(recycled, hidden);
                self.show_toast(ToastKind::Info, title, detail);
            }
            Done::Deleted(_, Err(error)) => {
                self.lib.deleting = 0;
                self.show_toast(ToastKind::Bad, "Delete failed", Some(error));
            }
            Done::Thumb(path, result) => {
                self.lib.on_thumb(path, result);
            }
            Done::Duration(path, secs) => {
                self.lib.on_duration(path, secs);
            }
            Done::Scrub(path, result) => {
                self.lib.on_scrub(path, result);
            }
            // Storage-tier results; compile shim until the UI wires them (see worker.rs).
            Done::VideoCached(clip, Ok(copy)) => self.ed.on_video_cached(&clip, &copy),
            Done::VideoCached(_, Err(_)) => {} // keeps playing from the clip drive
            Done::Proxy(clip, result) => self.ed.on_proxy(&clip, result),
            Done::Flushed(Ok(crate::backend::Flushed::Current(sidecar))) => self.lib.mark_edited_by_sidecar(&sidecar, true),
            Done::Flushed(Ok(crate::backend::Flushed::Removed(sidecar))) => self.lib.mark_edited_by_sidecar(&sidecar, false),
            Done::Flushed(Ok(crate::backend::Flushed::Nothing)) => {}
            Done::Flushed(Err(error)) => self.show_toast(ToastKind::Bad, "Couldn't save edits beside the clip", Some(error)),
            Done::Storage(report) => {
                if let Some(notice) = report.notice {
                    self.show_toast(ToastKind::Info, notice, None);
                }
                self.cache.refresh();
            }
        }
    }
}
