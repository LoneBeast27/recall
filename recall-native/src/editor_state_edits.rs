//! Editor edit commands: undo/redo, in/out, split/delete, trim and scrub drags, gain.
use std::time::Instant;

use super::{Drag, EditorState, GAIN_MAX, GAIN_MIN, MIN_RANGE};
use crate::edit_model::{AudioSetting, EditPlan, TimeRange};

impl EditorState {
    pub(super) fn touch(&mut self) {
        self.needs_flush = true;
        self.saved = false;
        self.dirty_since = Some(Instant::now());
    }

    pub(super) fn revise(&mut self) {
        self.revise_from(self.plan.clone());
    }

    fn revise_from(&mut self, before: EditPlan) {
        self.undo.push(before);
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.touch();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) {
        if let Some(previous) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.plan, previous));
            self.after_plan_swap();
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.plan, next));
            self.after_plan_swap();
        }
    }

    fn after_plan_swap(&mut self) {
        self.selected = self.selected.min(self.plan.ranges.len().saturating_sub(1));
        self.touch();
        self.sync_audio();
    }

    fn sync_audio(&mut self) {
        let streams: Vec<usize> = self.plan.audio.iter().map(|s| s.stream_index).collect();
        for stream in streams {
            self.push_preview_audio(stream);
        }
    }

    /// Selects the kept range under `source` time, if any.
    pub fn select_at(&mut self, source: f64) {
        if let Some(index) = self.plan.ranges.iter().position(|r| source >= r.start && source <= r.end) {
            self.selected = index;
        }
    }

    /// I trims the part under the playhead (in a cut: the part after it), never the selected
    /// part elsewhere: keys and playback move the playhead without changing the selection, and
    /// pulling a far part's edge out to its neighbour brought cut footage back (E3).
    pub fn set_in(&mut self) {
        let at = self.position();
        let ranges = &self.plan.ranges;
        let Some(index) = ranges.iter().rposition(|r| r.start <= at && at <= r.end).or_else(|| ranges.iter().position(|r| r.start > at)) else { return };
        let start = at.max(self.prev_end(index));
        // At a shared edge nothing moves: no undo step, no save.
        if at < ranges[index].end - MIN_RANGE && start != ranges[index].start {
            self.revise();
            self.plan.ranges[index].start = start;
            self.selected = index;
        }
    }

    /// O trims the part under the playhead (in a cut: the part before it).
    pub fn set_out(&mut self) {
        let at = self.position();
        let ranges = &self.plan.ranges;
        let Some(index) = ranges.iter().position(|r| r.start <= at && at <= r.end).or_else(|| ranges.iter().rposition(|r| r.end < at)) else { return };
        let end = at.min(self.next_start(index));
        if at > ranges[index].start + MIN_RANGE && end != ranges[index].end {
            self.revise();
            self.plan.ranges[index].end = end;
            self.selected = index;
        }
    }

    fn prev_end(&self, index: usize) -> f64 {
        if index == 0 { 0.0 } else { self.plan.ranges[index - 1].end }
    }

    fn next_start(&self, index: usize) -> f64 {
        self.plan.ranges.get(index + 1).map_or(self.duration(), |r| r.start)
    }

    pub fn split(&mut self) {
        let at = self.position();
        let Some(index) = self.plan.ranges.iter().position(|r| at > r.start + MIN_RANGE && at < r.end - MIN_RANGE) else { return };
        self.revise();
        let range = self.plan.ranges[index].clone();
        self.plan.ranges[index].end = at;
        self.plan.ranges.insert(index + 1, TimeRange { start: at, end: range.end, speed: range.speed });
        self.selected = index + 1;
    }

    pub fn delete_selected(&mut self) {
        if self.plan.ranges.len() <= 1 || self.selected >= self.plan.ranges.len() {
            return;
        }
        self.revise();
        self.plan.ranges.remove(self.selected);
        self.selected = self.selected.min(self.plan.ranges.len() - 1);
    }

    /// Starts a drag; a trim or gain drag makes one undo step, when it changes something.
    pub fn begin_drag(&mut self, drag: Drag) {
        if matches!(drag, Drag::TrimStart(_) | Drag::TrimEnd(_) | Drag::Gain(_)) {
            self.drag_from = Some(self.plan.clone());
        }
        if let Drag::TrimStart(i) | Drag::TrimEnd(i) = drag {
            self.selected = i;
        }
        self.drag = Some(drag);
    }

    pub fn end_drag(&mut self) {
        self.drag = None;
        if let Some(before) = self.drag_from.take().filter(|before| *before != self.plan) {
            self.revise_from(before);
        }
    }

    /// Escape during a drag: the plan goes back to where the drag started.
    pub fn cancel_drag(&mut self) {
        self.drag = None;
        if let Some(before) = self.drag_from.take() {
            self.plan = before;
            self.sync_audio();
        }
    }

    /// Applies the active drag at a window x coordinate.
    pub fn drag_to(&mut self, window_x: f32) {
        let Some(drag) = self.drag else { return };
        match drag {
            Drag::Scrub => {
                if let Some(f) = self.span_fraction(window_x) {
                    let t = f * self.duration();
                    self.select_at(t);
                    self.seek(t);
                }
            }
            Drag::TrimStart(i) => {
                if let (Some(f), Some(range)) = (self.span_fraction(window_x), self.plan.ranges.get(i).cloned()) {
                    // A part already shorter than MIN_RANGE (a loaded plan) stays as it is.
                    let (lo, hi) = (self.prev_end(i), range.end - MIN_RANGE);
                    if lo <= hi {
                        let t = (f * self.duration()).clamp(lo, hi);
                        self.plan.ranges[i].start = t;
                        self.seek(t);
                    }
                }
            }
            Drag::TrimEnd(i) => {
                if let (Some(f), Some(range)) = (self.span_fraction(window_x), self.plan.ranges.get(i).cloned()) {
                    let (lo, hi) = (range.start + MIN_RANGE, self.next_start(i));
                    if lo <= hi {
                        let t = (f * self.duration()).clamp(lo, hi);
                        self.plan.ranges[i].end = t;
                        self.seek(t);
                    }
                }
            }
            Drag::Gain(track) => {
                let Some(span) = self.gain_spans.get(track).and_then(|s| s.get()) else { return };
                let f = ((window_x - span.0) / span.1.max(1.0)).clamp(0.0, 1.0) as f64;
                self.set_gain(track, GAIN_MIN + f * (GAIN_MAX - GAIN_MIN));
            }
            Drag::Listen(track) => {
                use super::monitor::{LISTEN_MAX, LISTEN_MIN};
                let Some(span) = self.listen_spans.get(track).and_then(|s| s.get()) else { return };
                let f = ((window_x - span.0) / span.1.max(1.0)).clamp(0.0, 1.0) as f64;
                self.set_listen_gain(track, LISTEN_MIN + f * (LISTEN_MAX - LISTEN_MIN));
            }
        }
    }

    fn setting_mut(&mut self, track: usize) -> Option<&mut AudioSetting> {
        let info = self.info.as_ref()?.audio.get(track)?;
        let stream = info.stream_index;
        if !self.plan.audio.iter().any(|s| s.stream_index == stream) {
            let default = info.default_setting();
            self.plan.audio.push(default);
        }
        self.plan.audio.iter_mut().find(|s| s.stream_index == stream)
    }

    pub fn set_gain(&mut self, track: usize, db: f64) {
        let db = (db * 2.0).round() / 2.0; // 0.5 dB steps
        let Some(setting) = self.setting_mut(track) else { return };
        setting.gain_db = db.clamp(GAIN_MIN, GAIN_MAX);
        let stream = setting.stream_index;
        self.push_preview_audio(stream);
    }

    pub fn toggle_mute(&mut self, track: usize) {
        self.revise();
        let Some(setting) = self.setting_mut(track) else { return };
        setting.muted = !setting.muted;
        let stream = setting.stream_index;
        self.push_preview_audio(stream);
    }

    /// Mix the track into the export's first track, or keep it as its own track. Export only:
    /// the preview already plays every unmuted track together.
    pub fn toggle_mix(&mut self, track: usize) {
        self.revise();
        if let Some(setting) = self.setting_mut(track) {
            setting.mix = !setting.mix;
        }
    }

    /// The line under track `track`'s mix switch, when mixing it would play it twice.
    pub fn mix_hint(&self, track: usize) -> Option<&'static str> {
        let info = self.info.as_ref()?;
        let stream = info.audio.get(track)?.stream_index;
        let mix = self.setting(stream).map_or_else(|| info.audio[track].default_setting().mix, |s| s.mix);
        info.mix_hint(stream, mix && self.mix_down())
    }

    pub fn reset_gain(&mut self, track: usize) {
        let Some(stream) = self.info.as_ref().and_then(|i| i.audio.get(track)).map(|t| t.stream_index) else { return };
        if self.setting(stream).is_some_and(|s| s.gain_db != 0.0) {
            self.revise();
            self.set_gain(track, 0.0);
        }
    }
}

#[cfg(test)]
mod drag_tests {
    use super::*;

    fn editor() -> EditorState {
        let mut ed = EditorState::new(Err("no preview".into()));
        ed.plan.ranges = vec![TimeRange::new(0.0, 10.0)];
        ed
    }

    #[test]
    fn a_press_on_a_handle_without_moving_changes_nothing() {
        let mut ed = editor();
        ed.begin_drag(Drag::TrimEnd(0));
        ed.end_drag();
        assert!(!ed.can_undo() && ed.saved, "no undo step, nothing to save");
    }

    #[test]
    fn a_press_without_moving_keeps_redo() {
        let mut ed = editor();
        ed.plan.ranges[0].end = 9.0;
        ed.revise();
        ed.plan.ranges[0].end = 8.0;
        ed.undo();
        ed.begin_drag(Drag::TrimStart(0));
        ed.end_drag();
        assert!(ed.can_redo());
    }

    #[test]
    fn a_trim_is_one_undo_step() {
        let mut ed = editor();
        ed.begin_drag(Drag::TrimEnd(0));
        ed.plan.ranges[0].end = 9.0;
        ed.plan.ranges[0].end = 8.0;
        ed.end_drag();
        assert!(!ed.saved);
        ed.undo();
        assert!(!ed.can_undo());
        assert_eq!(ed.plan.ranges[0].end, 10.0);
    }

    #[test]
    fn escape_puts_the_plan_back_with_no_undo_step() {
        let mut ed = editor();
        ed.begin_drag(Drag::TrimEnd(0));
        ed.plan.ranges[0].end = 8.0;
        ed.cancel_drag();
        ed.end_drag(); // the release that follows
        assert_eq!(ed.plan.ranges[0].end, 10.0);
        assert!(ed.drag.is_none() && !ed.can_undo() && ed.saved);
    }

    #[test]
    fn a_sidecar_that_appeared_without_an_edit_here_is_left_alone() {
        let mut ed = editor();
        ed.sidecar_ours = true;
        assert!(!ed.may_touch_sidecar(), "none at open, no edit: not ours");
        ed.sidecar_at_open = true;
        assert!(ed.may_touch_sidecar(), "it loaded at open");
        ed.sidecar_at_open = false;
        ed.plan.ranges[0].end = 9.0;
        ed.revise();
        assert!(ed.may_touch_sidecar(), "an edit here wrote it");
        ed.undo();
        assert!(ed.may_touch_sidecar(), "undoing that edit may remove it");
        ed.sidecar_ours = false;
        ed.sidecar_at_open = true;
        assert!(!ed.may_touch_sidecar(), "an unreadable sidecar stays");
    }

    #[test]
    fn a_scrub_is_no_edit() {
        let mut ed = editor();
        ed.begin_drag(Drag::Scrub);
        ed.end_drag();
        assert!(ed.saved && !ed.can_undo());
    }
}

#[cfg(test)]
mod mix_tests {
    use super::*;
    use crate::edit_model::{AudioOutput, AudioTrackInfo, ClipInfo};

    fn track(stream_index: usize, name: &str) -> AudioTrackInfo {
        AudioTrackInfo { stream_index, name: name.into(), channels: 2, sample_rate: 48_000, preview_path: String::new(), waveform: Vec::new(), source: String::new() }
    }

    #[test]
    fn the_mix_switch_changes_the_export_tracks_and_is_one_undo_step() {
        let mut ed = EditorState::new(Err("no preview".into()));
        ed.plan.ranges = vec![TimeRange::new(0.0, 10.0)];
        ed.info = Some(ClipInfo {
            path: "a.mp4".into(), duration: 10.0, width: 4, height: 2, video_fps: 60.0, source_bytes: 1, source_modified_ms: 1,
            audio: vec![track(1, "Source A"), track(2, "Source B")], keyframes: Vec::new(),
        });
        assert_eq!(ed.plan.audio_outputs(&[1, 2]), vec![AudioOutput::Track(1), AudioOutput::Track(2)], "no settings yet: untouched");
        ed.toggle_mix(0);
        assert!(ed.setting(1).is_some_and(|s| !s.mix), "a new setting starts in the mix, the switch takes it out");
        ed.toggle_mix(1);
        ed.toggle_mix(1);
        assert!(ed.setting(2).is_some_and(|s| s.mix));
        assert_eq!(ed.plan.audio_outputs(&[1, 2]), vec![AudioOutput::Track(2), AudioOutput::Track(1)]);
        ed.toggle_mix(0);
        assert_eq!(ed.plan.audio_outputs(&[1, 2]), vec![AudioOutput::Mix(vec![1, 2])]);
        ed.undo();
        assert!(ed.setting(1).is_some_and(|s| !s.mix), "undo puts the switch back");
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;
    use crate::edit_model::{AudioTrackInfo, ClipInfo};

    fn clip(sources: &[&str]) -> EditorState {
        let mut ed = EditorState::new(Err("no preview".into()));
        let audio = sources
            .iter()
            .enumerate()
            .map(|(i, s)| AudioTrackInfo { stream_index: i + 1, name: format!("T{i}"), channels: 2, sample_rate: 48_000, preview_path: String::new(), waveform: Vec::new(), source: (*s).into() })
            .collect();
        let info = ClipInfo { path: "a.mp4".into(), duration: 10.0, width: 4, height: 2, video_fps: 60.0, source_bytes: 1, source_modified_ms: 1, audio, keyframes: Vec::new() };
        ed.adopt(info.clone(), None, None); // the clip-open path (both prepared and streamed)
        ed.info = Some(info);
        ed
    }

    #[test]
    fn an_app_track_opens_out_of_the_mix_and_switching_it_on_shows_the_hint() {
        let mut ed = clip(&["desktop-loopback", "mic", "app:a.exe"]);
        let mix: Vec<bool> = ed.plan.audio.iter().map(|s| s.mix).collect();
        assert_eq!(mix, [true, true, false], "the app track starts out of the mix");
        assert_eq!(ed.mix_hint(2), None, "out of the mix: nothing doubles");
        ed.toggle_mix(2);
        assert_eq!(ed.mix_hint(2), Some("This app is also in Desktop audio, so mixing it doubles it."));
        assert!(ed.setting(3).is_some_and(|s| s.mix), "the choice is not blocked");
        assert_eq!(ed.mix_hint(0), None);
    }

    #[test]
    fn a_game_clip_gives_no_hint_and_an_untagged_clip_keeps_everything_in_the_mix() {
        let mut game = clip(&["game-process", "mic", "app:a.exe"]);
        game.toggle_mix(2);
        assert_eq!(game.mix_hint(2), None, "a game's own track holds no other app");
        let old = clip(&["", ""]);
        assert!(old.plan.audio.iter().all(|s| s.mix), "older clips: today's behaviour");
    }
}

#[cfg(test)]
#[path = "editor_state_edits_tests.rs"]
mod tests;
