//! Listen-only levels: the volume and mute beside each lane, left of the timeline. They change
//! what the preview plays and nothing else: never the plan, never the export, never an undo
//! step or the edit sidecar. They last while the clip is open.
//!
//! The export's own gain, mute and mix choice live on the right page (the inspector) and are
//! part of the plan. The preview plays both: export gain plus listening gain, muted when either
//! is muted, so with the listening level at 0 dB the preview sounds like the export.
use std::collections::HashMap;

use super::EditorState;

/// Listening range (dB); 0 dB hears the export as it is.
pub const LISTEN_MIN: f64 = -24.0;
pub const LISTEN_MAX: f64 = 12.0;

/// One lane's listening level.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Level {
    pub gain_db: f64,
    pub muted: bool,
}

/// Listening levels by stream index; a lane not in it listens at 0 dB, unmuted.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Monitor {
    levels: HashMap<usize, Level>,
}

impl Monitor {
    pub fn level(&self, stream_index: usize) -> Level {
        self.levels.get(&stream_index).copied().unwrap_or_default()
    }
}

impl EditorState {
    fn lane_stream(&self, track: usize) -> Option<usize> {
        self.info.as_ref()?.audio.get(track).map(|t| t.stream_index)
    }

    /// Lane `track`'s listening level.
    pub fn listen_level(&self, track: usize) -> Level {
        self.lane_stream(track).map(|s| self.monitor.level(s)).unwrap_or_default()
    }

    pub fn toggle_listen_mute(&mut self, track: usize) {
        let Some(stream) = self.lane_stream(track) else { return };
        let level = self.monitor.levels.entry(stream).or_default();
        level.muted = !level.muted;
        self.push_preview_audio(stream);
    }

    pub fn set_listen_gain(&mut self, track: usize, db: f64) {
        let Some(stream) = self.lane_stream(track) else { return };
        let db = ((db * 2.0).round() / 2.0).clamp(LISTEN_MIN, LISTEN_MAX);
        self.monitor.levels.entry(stream).or_default().gain_db = db;
        self.push_preview_audio(stream);
    }

    /// What the preview plays for `stream_index`: the export's level, then the listening level.
    pub fn preview_audio(&self, stream_index: usize) -> (f64, bool) {
        let export = self.setting(stream_index);
        let listen = self.monitor.level(stream_index);
        let gain = export.map_or(0.0, |s| s.gain_db) + listen.gain_db;
        (gain, export.is_some_and(|s| s.muted) || listen.muted)
    }

    /// Track `track`'s export level meter (dBFS RMS, peak): the track as it plays, after its
    /// export gain. The listening level never moves it. Export-muted or nothing playing: None.
    pub fn export_meter(&self, track: usize) -> Option<(f32, f32)> {
        let levels = match shot_meter_levels(self) {
            Some(levels) => levels,
            None => self.preview.as_ref()?.track_levels(),
        };
        self.export_meter_with(track, &levels)
    }

    /// The export audio panel (and its meters) is on screen or not; the player measures levels
    /// only while it is.
    pub fn set_meters_wanted(&self, wanted: bool) {
        if let Some(preview) = self.preview.as_ref() {
            preview.set_meters_wanted(wanted);
        }
    }

    /// [`Self::export_meter`] from the player's levels (before gain and mute).
    pub(crate) fn export_meter_with(&self, track: usize, levels: &[crate::audio::TrackLevel]) -> Option<(f32, f32)> {
        let stream = self.lane_stream(track)?;
        let export = self.setting(stream);
        if export.is_some_and(|s| s.muted) {
            return None;
        }
        let gain = export.map_or(0.0, |s| s.gain_db) as f32;
        let db = |linear: f32| if linear > 0.0 { 20.0 * linear.log10() } else { -90.0 };
        let &(_, rms, peak) = levels.iter().find(|l| l.0 == stream)?;
        Some((db(rms) + gain, db(peak) + gain))
    }

    /// Sends `stream_index`'s preview level to the player.
    pub(super) fn push_preview_audio(&mut self, stream_index: usize) {
        let (gain, muted) = self.preview_audio(stream_index);
        if let Some(preview) = self.preview.as_mut() {
            let _ = preview.set_audio(stream_index, gain, muted);
        }
    }
}

/// Screenshot runs only (offscreen, silent): `RECALL_SHOT_METER_DB` gives every track that
/// level (RMS; peak 3 dB above), so the meters show without playing sound.
fn shot_meter_levels(ed: &EditorState) -> Option<Vec<crate::audio::TrackLevel>> {
    std::env::var_os("RECALL_NATIVE_OFFSCREEN")?;
    let db: f32 = std::env::var("RECALL_SHOT_METER_DB").ok()?.parse().ok()?;
    let linear = |db: f32| 10f32.powf(db / 20.0);
    Some(ed.info.as_ref()?.audio.iter().map(|t| (t.stream_index, linear(db), linear(db + 3.0))).collect())
}

#[cfg(test)]
mod tests_support {
    use super::*;
    use crate::edit_model::{AudioTrackInfo, ClipInfo, TimeRange};

    pub(super) fn editor() -> EditorState {
        let mut ed = EditorState::new(Err("no preview".into()));
        let track = |stream_index: usize| AudioTrackInfo {
            stream_index,
            name: format!("T{stream_index}"),
            channels: 2,
            sample_rate: 48_000,
            preview_path: String::new(),
            waveform: Vec::new(),
            source: String::new(),
        };
        let info = ClipInfo { path: "a.mp4".into(), duration: 10.0, width: 4, height: 2, video_fps: 60.0, source_bytes: 1, source_modified_ms: 1, audio: vec![track(1), track(2)], keyframes: Vec::new(), ..Default::default() };
        ed.adopt(info.clone(), None, None);
        ed.info = Some(info);
        ed.plan.ranges = vec![TimeRange::new(0.0, 10.0)];
        ed
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::editor;
    use super::*;

    #[test]
    fn listening_changes_only_the_preview_never_the_plan_undo_or_save() {
        let mut ed = editor();
        let plan = ed.plan.clone();
        ed.toggle_listen_mute(0);
        ed.set_listen_gain(1, -12.0);
        assert_eq!(ed.plan, plan, "the export plan is untouched");
        assert!(ed.saved && !ed.can_undo(), "no edit: nothing to save, no undo step");
        assert_eq!(ed.preview_audio(1), (0.0, true), "lane 1 is silent in the preview only");
        assert_eq!(ed.preview_audio(2), (-12.0, false));
        assert!(ed.plan.audio.iter().all(|s| !s.muted && s.gain_db == 0.0), "the export still has both tracks at full level");
    }

    #[test]
    fn the_preview_plays_export_gain_plus_listening_gain() {
        let mut ed = editor();
        ed.set_gain(0, -6.0); // right page: the export's gain
        ed.set_listen_gain(0, 3.0); // left of the timeline: listening only
        assert_eq!(ed.setting(1).map(|s| s.gain_db), Some(-6.0), "export gain is in the plan");
        assert_eq!(ed.preview_audio(1), (-3.0, false), "preview = export -6 dB + listening +3 dB");
        ed.set_listen_gain(0, 0.0);
        assert_eq!(ed.preview_audio(1), (-6.0, false), "listening at 0 dB: the preview is the export");
        ed.toggle_mute(0);
        assert!(ed.preview_audio(1).1, "an export mute is heard too");
        assert!(ed.setting(1).is_some_and(|s| s.muted));
    }

    #[test]
    fn listening_levels_reset_when_another_clip_opens() {
        let mut ed = editor();
        ed.toggle_listen_mute(0);
        let info = ed.info.clone().unwrap();
        ed.adopt(info, None, None);
        assert_eq!(ed.listen_level(0), Level::default());
    }
}

#[cfg(test)]
mod export_tests {
    use super::tests_support::editor;
    use crate::worker::Job;

    #[test]
    fn an_export_made_while_every_lane_is_silenced_still_carries_every_track_at_full_level() {
        let mut ed = editor();
        ed.toggle_listen_mute(0);
        ed.toggle_listen_mute(1);
        assert!(ed.preview_audio(1).1 && ed.preview_audio(2).1, "nothing is heard in the preview");
        let Job::Export(_, plan, _, _) = ed.export_job(ed.info.clone().unwrap()) else { panic!("an export job") };
        assert_eq!(plan.audio.len(), 2);
        assert!(plan.audio.iter().all(|s| !s.muted && s.gain_db == 0.0), "the export keeps both tracks: {:?}", plan.audio);
    }
}

#[cfg(test)]
mod mix_down_tests {
    use super::tests_support::editor;
    use crate::edit_model::AudioOutput;
    use crate::worker::Job;

    #[test]
    fn the_clip_switch_overrides_settings_without_changing_them_and_undoes() {
        let dir = std::env::temp_dir().join(format!("recall-mixdown-{}", std::process::id()));
        crate::services::editor_prefs::use_test_file(Some(dir.join("editor.json")));
        let mut ed = editor();
        assert!(ed.merge_audio && ed.plan.mix_down.is_none() && ed.mix_down(), "a new install merges; the clip follows Settings");
        let Job::Export(_, plan, _, _) = ed.export_job(ed.info.clone().unwrap()) else { panic!("an export job") };
        assert_eq!(plan.mix_down, Some(true), "the engine gets the Settings default filled in");
        assert_eq!(plan.audio_outputs(&[1, 2]), vec![AudioOutput::Mix(vec![1, 2])]);
        // The clip's own switch: one undo step, Settings untouched.
        ed.set_clip_merge(false);
        assert!(ed.can_undo() && !ed.mix_down());
        assert!(ed.merge_audio && crate::services::editor_prefs::EditorPrefs::load().mix_down, "Settings > Audio is not changed");
        let Job::Export(_, plan, _, _) = ed.export_job(ed.info.clone().unwrap()) else { panic!("an export job") };
        assert_eq!(plan.audio_outputs(&[1, 2]), vec![AudioOutput::Track(1), AudioOutput::Track(2)], "off: both tracks apart");
        assert!(editor().mix_down(), "the next clip still follows Settings");
        ed.undo();
        assert!(ed.plan.mix_down.is_none() && ed.mix_down(), "undo: follows Settings again");
        ed.redo();
        assert_eq!(ed.plan.mix_down, Some(false), "redo: Separate again");
        // Settings off: a clip with no choice of its own follows; one with its own keeps it.
        ed.set_merge_audio(false);
        assert!(!crate::services::editor_prefs::EditorPrefs::load().mix_down, "saved in editor.json");
        let next = editor();
        assert!(!next.mix_down() && next.plan.mix_down.is_none());
        let mut own = editor();
        own.set_clip_merge(true);
        let Job::Export(_, plan, _, _) = own.export_job(own.info.clone().unwrap()) else { panic!("an export job") };
        assert_eq!(plan.audio_outputs(&[1, 2]), vec![AudioOutput::Mix(vec![1, 2])], "the clip's own choice wins");
        // Old edits keep the values they saved.
        own.plan.mix_down = Some(false);
        own.plan.audio.iter_mut().for_each(|s| s.mix = false);
        let Job::Export(_, plan, _, _) = own.export_job(own.info.clone().unwrap()) else { panic!("an export job") };
        assert_eq!(plan.mix_down, Some(false));
        assert!(own.plan.audio.iter().all(|s| !s.mix), "not rewritten");
        crate::services::editor_prefs::use_test_file(None);
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod meter_tests {
    use super::tests_support::editor;
    use crate::services::editor_prefs::MixBox;
    use crate::worker::Job;

    /// Both tracks playing at -20 dBFS RMS, -17 dBFS peak (before gain).
    fn playing() -> Vec<crate::audio::TrackLevel> {
        vec![(1, 0.1, 0.1413), (2, 0.1, 0.1413)]
    }

    #[test]
    fn the_export_meter_follows_export_gain_and_never_the_listening_level() {
        let mut ed = editor();
        let (rms, _) = ed.export_meter_with(0, &playing()).unwrap();
        assert!((rms + 20.0).abs() < 0.01, "{rms}");
        ed.set_listen_gain(0, -12.0);
        ed.toggle_listen_mute(1);
        let plan = ed.plan.clone();
        assert_eq!(ed.export_meter_with(0, &playing()).unwrap().0, rms, "listening moves nothing");
        assert!(ed.export_meter_with(1, &playing()).is_some(), "a listen mute keeps the export meter");
        assert_eq!(ed.plan, plan, "nor the plan");
        ed.set_gain(0, 6.0);
        assert!((ed.export_meter_with(0, &playing()).unwrap().0 - (rms + 6.0)).abs() < 0.01, "export gain scales the meter");
        ed.toggle_mute(1);
        assert_eq!(ed.export_meter_with(1, &playing()), None, "export-muted: empty");
        assert_eq!(ed.export_meter_with(0, &[]), None, "nothing playing: empty");
        assert_eq!(ed.export_meter(0), None, "no player: empty");
    }

    #[test]
    fn mix_level_reaches_the_export_but_not_the_saved_edit() {
        let dir = std::env::temp_dir().join(format!("recall-mixbox-{}", std::process::id()));
        crate::services::editor_prefs::use_test_file(Some(dir.join("editor.json")));
        let mut ed = editor();
        assert_eq!(ed.mix_box, MixBox::default());
        ed.set_mix_box(MixBox { offset_on: true, offset_db: 3.5, limiter: false });
        let Job::Export(_, plan, _, _) = ed.export_job(ed.info.clone().unwrap()) else { panic!("an export job") };
        assert_eq!(plan.mix_level, crate::edit_model::MixLevel { offset_db: 3.5, limiter: false });
        assert!(ed.plan.mix_level.is_default() && !ed.can_undo(), "Settings, not an edit of this clip");
        assert_eq!(editor().mix_box, ed.mix_box, "remembered for the next clip");
        ed.set_mix_box(MixBox { offset_on: false, ..ed.mix_box });
        let Job::Export(_, plan, _, _) = ed.export_job(ed.info.clone().unwrap()) else { panic!("an export job") };
        assert_eq!(plan.mix_level.offset_db, 0.0, "offset off: 0 dB, the value kept");
        assert_eq!(editor().mix_box.offset_db, 3.5);
        crate::services::editor_prefs::use_test_file(None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
