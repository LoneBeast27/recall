//! Editor persistence and export: save/export results, flushing the edit file beside
//! the clip, the SSD playback swap, and export settings.
use super::EditorState;
use crate::{
    backend::ExportSummary,
    edit_model::ExportMode,
    worker::{Job, Workers},
};

impl EditorState {
    pub fn on_saved(&mut self, result: Result<String, String>) {
        self.saving = false;
        match result {
            Ok(_) => {
                self.saved = self.dirty_since.is_none();
                self.save_error = None;
            }
            Err(error) => self.save_error = Some(error),
        }
    }

    pub fn on_exported(&mut self, result: Result<ExportSummary, String>) {
        self.exporting = false;
        self.cancelling = false;
        match result {
            Err(error) if error == crate::backend::EXPORT_CANCELLED => {}
            Ok(summary) if self.export_is_for_open_clip() => {
                self.exported = Some(summary);
                self.export_error = None;
            }
            Err(error) if self.export_is_for_open_clip() => self.export_error = Some(error),
            _ => {} // The toast reports it; this footer is another clip's.
        }
    }

    fn export_is_for_open_clip(&self) -> bool {
        self.export_of.as_deref().is_some_and(|path| self.is_open(path))
    }

    /// Stops the running export; `Done::Exported` follows once the engine is gone.
    pub fn cancel_export(&mut self) {
        if self.exporting {
            self.cancelling = true;
            crate::backend::cancel_export();
        }
    }

    pub fn export(&mut self, workers: &Workers) {
        if self.exporting {
            return;
        }
        if let Some(info) = self.info.clone() {
            crate::services::storage::record_interaction(crate::services::storage::Interaction::Exported(info.path.clone()));
            self.exporting = true;
            self.export_of = Some(info.path.clone());
            self.exported = None;
            self.export_error = None;
            self.flush(workers);
            crate::backend::arm_export();
            workers.job(Job::Export(info, self.plan.clone(), self.fps, self.export_settings.clone()));
        }
    }

    /// A flush may rewrite or remove the sidecar: it loaded at open, or there was none then and
    /// any sidecar now came from an edit in this session. One that appeared without an edit here
    /// (another writer) stays.
    pub(super) fn may_touch_sidecar(&self) -> bool {
        self.sidecar_ours && (self.sidecar_at_open || !self.undo.is_empty() || !self.redo.is_empty())
    }

    /// Writes the edit file beside the clip (HDD) when the open clip changed since the last
    /// flush. Autosave only touches the SSD working copy; this runs on leave / switch / export / close.
    pub fn flush(&mut self, workers: &Workers) {
        if !self.needs_flush {
            return;
        }
        if let Some(info) = self.info.clone() {
            self.needs_flush = false;
            workers.job(Job::FlushProject(info, self.plan.clone(), self.fps, self.export_settings.clone(), self.may_touch_sidecar()));
        }
    }

    /// Synchronous flush for app close (the worker thread may not outlive the window).
    pub fn flush_now(&mut self) {
        if !self.needs_flush {
            return;
        }
        if let Some(info) = self.info.as_ref() {
            self.needs_flush = false;
            let _ = crate::backend::flush_project(info, &self.plan, self.fps, &self.export_settings, self.may_touch_sidecar());
        }
    }

    /// The player cannot show the current source: hide the video area and, once per clip,
    /// ask for a 720p H.264 preview copy (export keeps using the original).
    pub(super) fn on_preview_failed(&mut self, error: String, workers: &crate::worker::Workers) {
        self.hide_preview();
        let Some(info) = self.info.as_ref() else { return };
        if self.proxy_for.as_deref() == Some(info.path.as_str()) {
            self.status = format!("Can't preview this clip. Trim and export still work. ({error})");
            return;
        }
        self.proxy_for = Some(info.path.clone());
        self.status = "Making a preview copy of this clip…".into();
        workers.asset(crate::worker::AssetJob::Proxy(info.path.clone()));
    }

    /// A preview copy finished (or failed) for `clip`.
    pub fn on_proxy(&mut self, clip: &str, result: Result<String, String>) {
        if self.info.as_ref().is_none_or(|info| info.path != clip) {
            return;
        }
        match result {
            Ok(copy) => self.on_video_cached(clip, &copy),
            Err(error) => self.status = format!("Can't preview this clip. Trim and export still work. ({error})"),
        }
    }

    /// The SSD copy of the open clip is ready: switch playback to it, keeping the position.
    pub fn on_video_cached(&mut self, clip: &str, copy: &str) {
        let Some(info) = self.info.clone() else { return };
        if info.path != clip || self.playback_path.as_deref() == Some(copy) || self.playing {
            return;
        }
        let at = self.position();
        let tracks = self.preview_tracks(&info);
        if let Some(preview) = self.preview.as_mut() {
            // A failed swap has already closed the old source: reopen the original.
            // A preview copy is another file: its own picture end (a re-encode ends elsewhere).
            let limits = crate::services::library::video_limits(copy, info.duration);
            let (path, result) = match preview.open(copy, &tracks, info.duration, limits) {
                Ok(()) => (copy.to_owned(), Ok(())),
                Err(_) => (info.path.clone(), preview.open(&info.path, &tracks, info.duration, self.limits)),
            };
            match result {
                Ok(()) => {
                    self.playback_path = Some(path);
                    self.applied_rect = (0, 0, 0, 0);
                    self.pending_seek = Some(at);
                    self.preview_failed = false;
                    self.status = "Loading preview…".into();
                }
                Err(error) => self.last_error = Some(error),
            }
        }
    }

    pub fn lossless(&self) -> bool {
        self.export_settings.mode == ExportMode::Lossless
    }

    pub fn set_export_mode(&mut self, mode: ExportMode) {
        if self.export_settings.mode != mode {
            self.export_settings.mode = mode;
            self.touch();
        }
    }

    pub fn set_max_bytes(&mut self, max_bytes: Option<u64>) {
        if self.export_settings.max_bytes != max_bytes {
            self.export_settings.max_bytes = max_bytes;
            self.touch();
        }
    }

    pub fn set_height(&mut self, height: Option<u32>) {
        if self.export_settings.height != height {
            self.export_settings.height = height;
            self.touch();
        }
    }

    /// Playback speed of the selected segment in the export (0.25–4×).
    pub fn set_speed(&mut self, speed: f64) {
        let Some(current) = self.plan.ranges.get(self.selected).map(|r| r.speed) else { return };
        if (current - speed).abs() > f64::EPSILON {
            self.revise();
            self.plan.ranges[self.selected].speed = speed.clamp(0.25, 4.0);
        }
    }

    /// Where the selected cut actually starts in a lossless export.
    pub fn snapped_start(&self, index: usize) -> Option<f64> {
        let info = self.info.as_ref()?;
        let range = self.plan.ranges.get(index)?;
        (!info.keyframes.is_empty()).then(|| info.snap_to_keyframe(range.start))
    }

    pub fn set_fps(&mut self, fps: u32) {
        if fps > 0 && fps != self.fps {
            self.fps = fps;
            self.touch();
        }
    }
}
