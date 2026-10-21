//! Editor edit commands: undo/redo, in/out, split/delete, trim and scrub drags, gain.
use std::time::Instant;

use super::{Drag, EditorState, GAIN_MAX, GAIN_MIN, MIN_RANGE};
use crate::edit_model::{AudioSetting, TimeRange};

impl EditorState {
    pub(super) fn touch(&mut self) {
        self.needs_flush = true;
        self.saved = false;
        self.dirty_since = Some(Instant::now());
    }

    pub(super) fn revise(&mut self) {
        self.undo.push(self.plan.clone());
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
        let settings = self.plan.audio.clone();
        if let Some(preview) = self.preview.as_mut() {
            for s in settings {
                let _ = preview.set_audio(s.stream_index, s.gain_db, s.muted);
            }
        }
    }

    /// Selects the kept range under `source` time, if any.
    pub fn select_at(&mut self, source: f64) {
        if let Some(index) = self.plan.ranges.iter().position(|r| source >= r.start && source <= r.end) {
            self.selected = index;
        }
    }

    pub fn set_in(&mut self) {
        let at = self.position();
        if let Some(range) = self.plan.ranges.get(self.selected).cloned() {
            if at < range.end - MIN_RANGE {
                self.revise();
                self.plan.ranges[self.selected].start = at.max(self.prev_end(self.selected));
            }
        }
    }

    pub fn set_out(&mut self) {
        let at = self.position();
        if let Some(range) = self.plan.ranges.get(self.selected).cloned() {
            if at > range.start + MIN_RANGE {
                self.revise();
                self.plan.ranges[self.selected].end = at.min(self.next_start(self.selected));
            }
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

    /// Starts a drag; trims record one undo step for the whole gesture.
    pub fn begin_drag(&mut self, drag: Drag) {
        if matches!(drag, Drag::TrimStart(_) | Drag::TrimEnd(_) | Drag::Gain(_)) {
            self.revise();
        }
        if let Drag::TrimStart(i) | Drag::TrimEnd(i) = drag {
            self.selected = i;
        }
        self.drag = Some(drag);
    }

    pub fn end_drag(&mut self) {
        if self.drag.take().is_some() {
            self.touch();
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
                    let t = (f * self.duration()).clamp(self.prev_end(i), range.end - MIN_RANGE);
                    self.plan.ranges[i].start = t;
                    self.seek(t);
                }
            }
            Drag::TrimEnd(i) => {
                if let (Some(f), Some(range)) = (self.span_fraction(window_x), self.plan.ranges.get(i).cloned()) {
                    let t = (f * self.duration()).clamp(range.start + MIN_RANGE, self.next_start(i));
                    self.plan.ranges[i].end = t;
                    self.seek(t);
                }
            }
            Drag::Gain(track) => {
                let Some(span) = self.gain_spans.get(track).and_then(|s| s.get()) else { return };
                let f = ((window_x - span.0) / span.1.max(1.0)).clamp(0.0, 1.0) as f64;
                self.set_gain(track, GAIN_MIN + f * (GAIN_MAX - GAIN_MIN));
            }
        }
    }

    fn setting_mut(&mut self, track: usize) -> Option<&mut AudioSetting> {
        let stream = self.info.as_ref()?.audio.get(track)?.stream_index;
        if !self.plan.audio.iter().any(|s| s.stream_index == stream) {
            self.plan.audio.push(AudioSetting { stream_index: stream, gain_db: 0.0, muted: false });
        }
        self.plan.audio.iter_mut().find(|s| s.stream_index == stream)
    }

    pub fn set_gain(&mut self, track: usize, db: f64) {
        let db = (db * 2.0).round() / 2.0; // 0.5 dB steps
        let Some(setting) = self.setting_mut(track) else { return };
        setting.gain_db = db.clamp(GAIN_MIN, GAIN_MAX);
        let (stream, gain, muted) = (setting.stream_index, setting.gain_db, setting.muted);
        if let Some(preview) = self.preview.as_mut() {
            let _ = preview.set_audio(stream, gain, muted);
        }
    }

    pub fn toggle_mute(&mut self, track: usize) {
        self.revise();
        let Some(setting) = self.setting_mut(track) else { return };
        setting.muted = !setting.muted;
        let (stream, gain, muted) = (setting.stream_index, setting.gain_db, setting.muted);
        if let Some(preview) = self.preview.as_mut() {
            let _ = preview.set_audio(stream, gain, muted);
        }
    }

    pub fn reset_gain(&mut self, track: usize) {
        self.revise();
        self.set_gain(track, 0.0);
    }
}
