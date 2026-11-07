//! Editor persistence and export: save/export results, flushing the edit file beside
//! the clip, the SSD playback swap, and export settings.
use super::EditorState;
use crate::edit_model::EditPlan;
use crate::{
    backend::ExportSummary,
    edit_model::ExportMode,
    worker::{Job, Workers},
};

/// The Export button's text while an export runs.
pub(crate) fn export_label(lossless: bool, fps: u32, progress: Option<f32>) -> String {
    let what = if lossless { "Exporting".to_owned() } else { format!("Exporting at {fps} fps") };
    match progress {
        Some(share) => format!("{what}… {}%", (share * 100.0).floor() as u32),
        None => format!("{what}…"),
    }
}

impl EditorState {
    /// An autosave finished. One of a clip no longer open only frees the next save (E9).
    pub fn on_saved(&mut self, clip: &str, result: Result<String, String>) {
        self.saving = false;
        if !self.is_open(clip) {
            return;
        }
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
                // The plan just exported needs no pre-encode (Export came before it settled,
                // or while recording held it).
                self.pre_encode_exported();
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
            let job = self.export_job(info);
            if let Job::Export(info, plan, fps, settings) = &job {
                crate::services::pre_encode::await_export(crate::services::pre_encode::key(info, plan, *fps, settings));
            }
            workers.job(job);
        }
    }

    /// What Export sends the engine: the plan and the right page's settings. The listening
    /// levels left of the timeline are not in it.
    pub(super) fn export_job(&self, info: crate::edit_model::ClipInfo) -> Job {
        Job::Export(info, self.export_plan(), self.fps, self.engine_settings())
    }

    /// The settings Export sends. Lossless hides the size limit and resolution and the engine
    /// refuses them for a stream copy, so they are left out; the stored values stay for a
    /// switch back to Re-encode.
    pub(crate) fn engine_settings(&self) -> crate::edit_model::ExportSettings {
        let mut settings = self.export_settings.clone();
        if settings.mode == ExportMode::Lossless {
            settings.max_bytes = None;
            settings.height = None;
        }
        settings
    }

    /// Lossless cannot change speed: the inspector says so while a part has one.
    pub fn lossless_blocked_by_speed(&self) -> bool {
        self.lossless() && self.plan.has_speed_changes()
    }

    /// The Export button while it runs: the engine's progress when it reports it (frame rate
    /// only for a re-encode; a stream copy keeps the recorded frames).
    pub fn export_label(&self) -> String {
        export_label(self.lossless(), self.fps, crate::backend::export_progress())
    }

    /// The output frame size: the source's for lossless, else after the Resolution choice.
    pub fn export_size(&self) -> Option<(u32, u32)> {
        let info = self.info.as_ref()?;
        Some(if self.lossless() { (info.width, info.height) } else { crate::services::pre_encode_room::output_dims(info.width, info.height, self.export_settings.height) })
    }

    /// The plan the engine gets: "Mix tracks" filled in from the remembered default when these
    /// edits never set it (the engine reads an unset choice as on).
    pub(crate) fn export_plan(&self) -> EditPlan {
        let mut plan = self.plan.clone();
        plan.mix_down = Some(self.mix_down());
        plan
    }

    /// "Mix tracks" for this clip: its own choice, else the remembered default.
    pub fn mix_down(&self) -> bool {
        self.plan.mix_down.unwrap_or(self.mix_down_default)
    }

    /// The Export section's Mix / Separate choice: one undo step, kept with the clip's edits,
    /// and remembered as the default for clips opened later that never set it.
    pub fn set_mix_down(&mut self, on: bool) {
        if self.mix_down() == on && self.plan.mix_down.is_some() {
            return;
        }
        self.revise();
        self.plan.mix_down = Some(on);
        // The remembered default changes for the next clip only: this clip's default stays, so an
        // undo back to "unset" shows what it showed before.
        let _ = crate::services::editor_prefs::EditorPrefs { mix_down: on }.save();
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
        self.flush_on_close(|job| workers.job(job));
    }

    /// [`Self::flush`] with the job handed to `send`. App close sends it to the edits thread too:
    /// that thread writes in order, so an older autosave still queued cannot land after it.
    pub fn flush_on_close(&mut self, mut send: impl FnMut(Job)) {
        if !self.needs_flush {
            return;
        }
        if let Some(info) = self.info.clone() {
            self.needs_flush = false;
            // The sidecar may wait behind an export; a reopen must still find these edits.
            if let Err(error) = crate::backend::save_working(&info, &self.plan, self.fps, &self.export_settings) {
                self.save_error = Some(error);
            }
            send(Job::FlushProject(info, self.plan.clone(), self.fps, self.export_settings.clone(), self.may_touch_sidecar()));
        }
    }

    /// The player cannot show the current source: hide the video area and, once per clip,
    /// ask for a 720p H.264 preview copy (export keeps using the original).
    pub(super) fn on_preview_failed(&mut self, error: String, workers: &crate::worker::Workers) {
        self.hide_preview();
        if let Some(failure) = self.preview.as_ref().and_then(|preview| preview.stream_failure()) {
            self.status = format!("Couldn't load this clip. {failure}");
            return;
        }
        if self.reopen_original() {
            return;
        }
        let Some(info) = self.info.as_ref() else { return };
        if !std::path::Path::new(&info.path).is_file() {
            self.status = "Can't find this clip's file. It was moved, renamed or deleted.".into();
            return;
        }
        if self.proxy_for.as_deref() == Some(info.path.as_str()) {
            self.status = format!("Can't preview this clip. Trim and export still work. ({error})");
            return;
        }
        self.proxy_for = Some(info.path.clone());
        self.status = "Making a preview copy of this clip…".into();
        workers.asset(crate::worker::AssetJob::Proxy(info.path.clone()));
    }

    /// The sound failed (or came back) while the picture plays: say so; no preview copy, which
    /// could not bring the sound back (`media_sound.rs`).
    pub(super) fn on_sound_notice(&mut self, notice: crate::media::SoundNotice) -> bool {
        const LOST: &str = "Sound unavailable: ";
        match notice {
            crate::media::SoundNotice::Lost(reason) => self.status = format!("{LOST}{reason}"),
            crate::media::SoundNotice::Back if self.status.starts_with(LOST) => self.status.clear(),
            crate::media::SoundNotice::Back => return false,
        }
        true
    }

    /// The player was reading an SSD copy that is gone (the cache let it go, or Settings
    /// cleared it): play the clip from its own drive, at the same spot, instead of a dead
    /// preview. False when there is no such copy to fall back from.
    fn reopen_original(&mut self) -> bool {
        let Some(info) = self.info.clone() else { return false };
        let Some(copy) = self.playback_path.clone() else { return false };
        let gone = |path: &str| !std::path::Path::new(path).is_file();
        if super::same_path(&copy, &info.path) || !gone(&copy) || gone(&info.path) {
            return false;
        }
        let at = self.position();
        let tracks = self.preview_tracks(&info);
        let Some(preview) = self.preview.as_mut() else { return false };
        crate::trace::player_event("preview copy gone: playing the original", serde_json::json!({ "clip": info.path, "copy": copy }));
        match preview.open(&info.path, &tracks, info.duration, self.limits) {
            Ok(()) => {
                self.playback_path = Some(info.path.clone());
                self.applied_rect = (0, 0, 0, 0);
                self.pending_seek = Some(at);
                self.preview_failed = false;
                self.status = "Loading preview…".into();
                true
            }
            Err(error) => {
                self.last_error = Some(error);
                false
            }
        }
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

#[cfg(test)]
mod sound_tests {
    use crate::editor_state::EditorState;
    use crate::media::SoundNotice;

    #[test]
    fn lost_sound_is_said_without_failing_the_preview() {
        // E1/E5: an audio-only failure hid the picture and queued a full preview-copy transcode.
        let mut ed = EditorState::new(Err("no preview".into()));
        assert!(ed.on_sound_notice(SoundNotice::Lost("could not open wave output (WinMM error 2)".into())));
        assert_eq!(ed.status, "Sound unavailable: could not open wave output (WinMM error 2)");
        assert!(!ed.preview_failed && ed.proxy_for.is_none(), "the picture stays and no copy is made");
        assert!(ed.on_sound_notice(SoundNotice::Back));
        assert!(ed.status.is_empty());
        ed.status = "Exported".into();
        assert!(!ed.on_sound_notice(SoundNotice::Back), "another message stays");
    }

    #[test]
    fn a_save_of_another_clip_frees_the_next_save_and_reports_nothing_here() {
        // E9: a save that finished after an export landed on whatever clip was open by then.
        let mut ed = EditorState::new(Err("no preview".into()));
        ed.info = Some(crate::edit_model::ClipInfo { path: "b.mp4".into(), duration: 1.0, width: 1, height: 1, video_fps: 60.0, source_bytes: 1, source_modified_ms: 1, audio: Vec::new(), keyframes: Vec::new() });
        ed.saving = true;
        ed.on_saved("a.mp4", Err("disk full".into()));
        assert!(!ed.saving, "the next autosave may go");
        assert_eq!(ed.save_error, None, "a.mp4's failure is not b.mp4's");
        ed.on_saved("b.mp4", Err("disk full".into()));
        assert_eq!(ed.save_error.as_deref(), Some("disk full"));
    }

    #[test]
    fn closing_queues_the_last_flush_behind_the_edit_writes_already_sent() {
        // R2-3: the close flush wrote from the UI thread, and an older autosave still queued on
        // the disk lane landed after it: the last edit was gone on the next open.
        let mut ed = EditorState::new(Err("no preview".into()));
        ed.info = Some(crate::edit_model::ClipInfo { path: "r2-3-close.mp4".into(), duration: 10.0, width: 1, height: 1, video_fps: 60.0, source_bytes: 1, source_modified_ms: 1, audio: Vec::new(), keyframes: Vec::new() });
        ed.plan.ranges = vec![crate::edit_model::TimeRange::new(1.0, 9.0)];
        ed.needs_flush = true;
        let mut sent = Vec::new();
        ed.flush_on_close(|job| sent.push(job));
        assert!(matches!(sent.as_slice(), [crate::worker::Job::FlushProject(..)]), "the close flush goes to the in-order edits thread");
        ed.flush_on_close(|job| sent.push(job));
        assert_eq!(sent.len(), 1, "nothing new to flush");
    }
}
