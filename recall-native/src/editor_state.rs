//! Editor model: the open clip, its non-destructive plan, undo/redo, native preview
//! control, autosave and export state. Rendering lives in `views/editor*.rs`.
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

use crate::{
    backend::{ExportSummary, PreparedClip, VisualAssets},
    edit_model::{AudioSetting, ClipInfo, EditPlan, ExportMode, ExportSettings, TimeRange},
    media::{NativePreview, PreviewTrack},
    worker::{Job, Workers},
};

pub const MIN_RANGE: f64 = 0.05;
pub const FPS_PRESETS: [u32; 5] = [60, 120, 240, 480, 960];
const AUTOSAVE_AFTER: Duration = Duration::from_millis(700);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Drag {
    Scrub,
    TrimStart(usize),
    TrimEnd(usize),
    Gain(usize),
}

/// Device-pixel rect (x, y, w, h) of the preview canvas, written during paint.
pub type DeviceRect = Rc<Cell<Option<(i32, i32, i32, i32)>>>;
/// Window-pixel (left, width) span of an interactive strip, written during paint.
pub type Span = Rc<Cell<Option<(f32, f32)>>>;

pub struct EditorState {
    pub info: Option<ClipInfo>,
    pub visuals: VisualAssets,
    pub plan: EditPlan,
    undo: Vec<EditPlan>,
    redo: Vec<EditPlan>,
    pub selected: usize,
    pub preview: Option<NativePreview>,
    pub playing: bool,
    pub fps: u32,
    /// Lossless/re-encode, size cap and output height for the next export.
    pub export_settings: ExportSettings,
    pub status: String,
    /// One-shot error for a toast (taken by Root).
    pub last_error: Option<String>,
    pub warning: Option<String>,
    pub preview_rect: DeviceRect,
    applied_rect: (i32, i32, i32, i32),
    /// Where the native video window currently is (device px); Root leaves this unpainted.
    pub hole: DeviceRect,
    pub track_span: Span,
    pub gain_spans: Vec<Span>,
    pub drag: Option<Drag>,
    generation: u64,
    play_started: Option<Instant>,
    saw_native_playing: bool,
    dirty_since: Option<Instant>,
    saving: bool,
    pub saved: bool,
    pub save_error: Option<String>,
    pub exporting: bool,
    pub exported: Option<ExportSummary>,
    pub export_error: Option<String>,
    pub loading_path: Option<String>,
}

impl EditorState {
    pub fn new(preview: Result<NativePreview, String>) -> Self {
        let (preview, status) = match preview {
            Ok(preview) => (Some(preview), String::new()),
            Err(error) => (None, format!("Preview unavailable: {error}")),
        };
        Self {
            info: None,
            visuals: VisualAssets::default(),
            plan: EditPlan::default(),
            undo: vec![],
            redo: vec![],
            selected: 0,
            preview,
            playing: false,
            fps: 60,
            export_settings: ExportSettings::default(),
            status,
            last_error: None,
            warning: None,
            preview_rect: Rc::new(Cell::new(None)),
            applied_rect: (0, 0, 0, 0),
            hole: Rc::new(Cell::new(None)),
            track_span: Rc::new(Cell::new(None)),
            gain_spans: vec![],
            drag: None,
            generation: 0,
            play_started: None,
            saw_native_playing: false,
            dirty_since: None,
            saving: false,
            saved: true,
            save_error: None,
            exporting: false,
            exported: None,
            export_error: None,
            loading_path: None,
        }
    }

    pub fn duration(&self) -> f64 {
        self.info.as_ref().map_or(0.0, |info| info.duration.max(0.01))
    }

    pub fn is_open(&self, path: &str) -> bool {
        self.info.as_ref().is_some_and(|info| info.path == path) || self.loading_path.as_deref() == Some(path)
    }

    pub fn open(&mut self, path: String, workers: &Workers) {
        self.pause();
        self.hide_preview();
        self.info = None;
        self.visuals = VisualAssets::default();
        self.warning = None;
        self.exported = None;
        self.export_error = None;
        self.status = "Preparing clip…".into();
        self.generation = self.generation.wrapping_add(1);
        self.loading_path = Some(path.clone());
        workers.job(Job::Prepare(self.generation, path));
    }

    pub fn apply_prepared(&mut self, generation: u64, result: Result<PreparedClip, String>) {
        if generation != self.generation {
            return;
        }
        self.loading_path = None;
        let prepared = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        let info = prepared.info;
        self.plan = match prepared.project {
            Some(project) => {
                self.fps = project.export_fps.max(1);
                project.plan
            }
            None => EditPlan {
                ranges: vec![TimeRange::new(0.0, info.duration)],
                audio: info
                    .audio
                    .iter()
                    .map(|a| AudioSetting { stream_index: a.stream_index, gain_db: 0.0, muted: false })
                    .collect(),
            },
        };
        self.selected = 0;
        self.undo.clear();
        self.redo.clear();
        self.saved = true;
        self.dirty_since = None;
        self.warning = prepared.warning;
        self.gain_spans = (0..info.audio.len()).map(|_| Rc::new(Cell::new(None))).collect();
        let tracks = self.preview_tracks(&info);
        self.status = "Loading preview…".into();
        if let Some(preview) = self.preview.as_mut() {
            if let Err(error) = preview.open(&info.path, &tracks) {
                self.status = error;
            }
        }
        self.info = Some(info);
        self.visuals = prepared.visuals;
        self.applied_rect = (0, 0, 0, 0);
    }

    fn preview_tracks(&self, info: &ClipInfo) -> Vec<PreviewTrack> {
        info.audio
            .iter()
            .map(|track| {
                let setting = self.setting(track.stream_index);
                PreviewTrack {
                    stream_index: track.stream_index,
                    path: track.preview_path.clone(),
                    gain_db: setting.map_or(0.0, |s| s.gain_db),
                    muted: setting.is_some_and(|s| s.muted),
                }
            })
            .collect()
    }

    pub fn setting(&self, stream_index: usize) -> Option<&AudioSetting> {
        self.plan.audio.iter().find(|s| s.stream_index == stream_index)
    }

    /// Per-frame upkeep. Returns true when something visible changed.
    pub fn tick(&mut self, editor_visible: bool, workers: &Workers) -> bool {
        let mut changed = false;
        if let Some(preview) = self.preview.as_mut() {
            match preview.refresh() {
                Ok(true) => {
                    changed = true;
                    if preview.is_ready() && self.status == "Loading preview…" {
                        self.status.clear();
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    if self.status != error {
                        self.status = error;
                        changed = true;
                    }
                    self.playing = false;
                }
            }
        }
        if self.playing {
            changed |= self.follow_playback();
        }
        self.apply_preview_rect(editor_visible);
        if let Some(since) = self.dirty_since {
            if !self.saving && since.elapsed() >= AUTOSAVE_AFTER && self.drag.is_none() {
                if let Some(info) = self.info.clone() {
                    self.saving = true;
                    self.dirty_since = None;
                    workers.job(Job::SaveProject(info, self.plan.clone(), self.fps));
                }
            }
        }
        changed
    }

    fn follow_playback(&mut self) -> bool {
        let Some(preview) = self.preview.as_mut() else { return false };
        let Ok(pos) = preview.position() else { return false };
        let native_playing = preview.is_playing();
        self.saw_native_playing |= native_playing;
        let settled = self.play_started.is_some_and(|t| t.elapsed() >= Duration::from_millis(250));
        let ended = self.plan.ranges.last().is_some_and(|last| pos >= last.end - 0.001);
        if ended || (settled && self.saw_native_playing && !native_playing) {
            self.pause();
        } else if !self.plan.ranges.iter().any(|r| pos >= r.start && pos < r.end) {
            match self.plan.ranges.iter().find(|r| r.start > pos).map(|r| r.start) {
                Some(next) => self.seek(next),
                None => self.pause(),
            }
        }
        true
    }

    fn apply_preview_rect(&mut self, visible: bool) {
        let next = if visible && self.info.is_some() { self.preview_rect.get().unwrap_or((0, 0, 0, 0)) } else { (0, 0, 0, 0) };
        if next == self.applied_rect {
            return;
        }
        self.applied_rect = next;
        if let Some(preview) = self.preview.as_mut() {
            let shown = next.2 > 0 && next.3 > 0;
            let result = if shown { preview.rect(next.0, next.1, next.2, next.3) } else { preview.hide() };
            match result {
                Ok(()) => self.hole.set(shown.then_some(next)),
                Err(error) => {
                    self.hole.set(None);
                    self.status = error;
                }
            }
        }
    }

    pub fn hide_preview(&mut self) {
        self.hole.set(None);
        self.applied_rect = (0, 0, 0, 0);
        if let Some(preview) = self.preview.as_mut() {
            let _ = preview.hide();
        }
    }

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
        match result {
            Ok(summary) => {
                self.exported = Some(summary);
                self.export_error = None;
            }
            Err(error) => self.export_error = Some(error),
        }
    }

    pub fn export(&mut self, workers: &Workers) {
        if self.exporting {
            return;
        }
        if let Some(info) = self.info.clone() {
            self.exporting = true;
            self.exported = None;
            self.export_error = None;
            workers.job(Job::Export(info, self.plan.clone(), self.fps));
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
        self.export_settings.max_bytes = max_bytes;
        self.touch();
    }

    pub fn set_height(&mut self, height: Option<u32>) {
        self.export_settings.height = height;
        self.touch();
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

    // ---- playback -------------------------------------------------------

    pub fn position(&self) -> f64 {
        self.preview.as_ref().and_then(|p| p.position().ok()).unwrap_or(0.0)
    }

    pub fn toggle_play(&mut self) {
        if self.playing {
            self.pause();
            return;
        }
        let Some(preview) = self.preview.as_mut() else { return };
        let pos = preview.position().unwrap_or(0.0);
        let at_end = self.plan.ranges.last().is_some_and(|last| pos >= last.end - 0.01);
        let start = self.plan.ranges.first().map_or(0.0, |first| first.start);
        if at_end || pos < start {
            let _ = preview.seek(start);
        }
        match preview.play() {
            Ok(()) => {
                self.playing = true;
                self.play_started = Some(Instant::now());
                self.saw_native_playing = false;
            }
            Err(error) => {
                self.status = error.clone();
                self.last_error = Some(error);
            }
        }
    }

    pub fn pause(&mut self) {
        self.playing = false;
        self.play_started = None;
        if let Some(preview) = self.preview.as_mut() {
            let _ = preview.pause();
        }
    }

    pub fn seek(&mut self, seconds: f64) {
        let target = seconds.clamp(0.0, self.duration());
        if let Some(preview) = self.preview.as_mut() {
            if let Err(error) = preview.seek(target) {
                self.status = error;
            }
        }
    }

    pub fn nudge(&mut self, delta: f64) {
        let pos = self.position();
        self.seek(pos + delta);
    }

    /// Source time → time in the edited output (slow/fast ranges count at their speed).
    pub fn edited_time(&self, source: f64) -> f64 {
        let mut total = 0.0;
        for range in &self.plan.ranges {
            let speed = if range.speed > 0.0 { range.speed } else { 1.0 };
            if source >= range.end {
                total += (range.end - range.start) / speed;
            } else if source > range.start {
                total += (source - range.start) / speed;
            }
        }
        total
    }

    pub fn span_fraction(&self, window_x: f32) -> Option<f64> {
        let (left, width) = self.track_span.get()?;
        (width > 1.0).then(|| ((window_x - left) / width).clamp(0.0, 1.0) as f64)
    }

    // ---- edits ----------------------------------------------------------

    fn touch(&mut self) {
        self.saved = false;
        self.dirty_since = Some(Instant::now());
    }

    fn revise(&mut self) {
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

pub const GAIN_MIN: f64 = -24.0;
pub const GAIN_MAX: f64 = 12.0;

/// "00:34.50"
pub fn timecode(seconds: f64) -> String {
    // `> 0.0` also folds -0.0 and NaN to zero (they printed as "00:-0.00").
    let s = if seconds > 0.0 { seconds } else { 0.0 };
    let minutes = (s / 60.0).floor() as u64;
    let rest = (s - minutes as f64 * 60.0).max(0.0);
    format!("{minutes:02}:{rest:05.2}")
}

/// "0:52.30" for ruler tags.
pub fn short_timecode(seconds: f64) -> String {
    let s = if seconds > 0.0 { seconds } else { 0.0 };
    let minutes = (s / 60.0).floor() as u64;
    format!("{minutes}:{:05.2}", (s - minutes as f64 * 60.0).max(0.0))
}
