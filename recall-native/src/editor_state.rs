//! Editor model: the open clip, its non-destructive plan, undo/redo, native preview
//! control, autosave and export state. Rendering lives in `views/editor*.rs`.
//! Save/flush/export settings live in `editor_state_persist.rs`; undo, cuts, trims,
//! drags and gain in `editor_state_edits.rs`; play/pause/seek in `editor_state_playback.rs`.
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

use crate::{
    backend::{ExportSummary, PreparedClip, VisualAssets},
    edit_model::{AudioSetting, ClipInfo, EditPlan, ExportSettings, TimeRange},
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
    /// Play was pressed while the preview was loading: start once it is ready.
    play_when_ready: bool,
    pub fps: u32,
    /// Lossless/re-encode, size cap and output height for the next export.
    pub export_settings: ExportSettings,
    /// Bookmarks (seconds from clip start) from `<clip>.recall-marks.json`.
    pub marks: Vec<f64>,
    /// The file the preview plays (an SSD copy when the video tier has one).
    pub playback_path: Option<String>,
    /// Seek to apply once a re-opened preview is ready (SSD swap keeps the position).
    pending_seek: Option<f64>,
    /// The open clip was changed since the sidecar was last written beside it.
    needs_flush: bool,
    /// A flush may rewrite or remove the sidecar beside the open clip.
    sidecar_ours: bool,
    pub status: String,
    /// One-shot error for a toast (taken by Root).
    pub last_error: Option<String>,
    /// The player reported an error for the current source (the video area is hidden).
    pub preview_failed: bool,
    /// Clip a preview copy was requested for (once per clip).
    pub proxy_for: Option<String>,
    pub warning: Option<String>,
    pub preview_rect: DeviceRect,
    applied_rect: (i32, i32, i32, i32),
    /// Where the native video window currently is (device px); Root leaves this unpainted.
    pub hole: DeviceRect,
    pub track_span: Span,
    pub gain_spans: Vec<Span>,
    pub drag: Option<Drag>,
    /// The plan a trim or gain drag started from.
    drag_from: Option<EditPlan>,
    generation: u64,
    play_started: Option<Instant>,
    saw_native_playing: bool,
    watch: crate::playback_watch::PlaybackWatch,
    audio_stalls_seen: u32,
    dirty_since: Option<Instant>,
    saving: bool,
    pub saved: bool,
    pub save_error: Option<String>,
    pub exporting: bool,
    /// Cancel was pressed; the engine has not stopped yet.
    pub cancelling: bool,
    /// The clip the running or last export is of: its result shows only on that clip.
    export_of: Option<String>,
    pub exported: Option<ExportSummary>,
    pub export_error: Option<String>,
    pub loading_path: Option<String>,
}

#[path = "editor_state_persist.rs"]
mod persist;
#[path = "editor_state_edits.rs"]
mod edits;
#[path = "editor_state_playback.rs"]
mod playback;

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
            play_when_ready: false,
            fps: 60,
            export_settings: ExportSettings::default(),
            marks: Vec::new(),
            playback_path: None,
            pending_seek: None,
            needs_flush: false,
            sidecar_ours: false,
            status,
            last_error: None,
            preview_failed: false,
            proxy_for: None,
            warning: None,
            preview_rect: Rc::new(Cell::new(None)),
            applied_rect: (0, 0, 0, 0),
            hole: Rc::new(Cell::new(None)),
            track_span: Rc::new(Cell::new(None)),
            gain_spans: vec![],
            drag: None,
            drag_from: None,
            generation: 0,
            play_started: None,
            saw_native_playing: false,
            watch: Default::default(),
            audio_stalls_seen: 0,
            dirty_since: None,
            saving: false,
            saved: true,
            save_error: None,
            exporting: false,
            cancelling: false,
            export_of: None,
            exported: None,
            export_error: None,
            loading_path: None,
        }
    }

    pub fn duration(&self) -> f64 {
        self.info.as_ref().map_or(0.0, |info| info.duration.max(0.01))
    }

    /// `info.path` is canonical (`\\?\V:\...`); tiles pass plain paths.
    pub fn is_open(&self, path: &str) -> bool {
        self.info.as_ref().is_some_and(|info| same_path(&info.path, path)) || self.loading_path.as_deref().is_some_and(|p| same_path(p, path))
    }

    pub fn open(&mut self, path: String, workers: &Workers) {
        self.flush(workers);
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
                self.playing = false;
                self.play_when_ready = false;
                return;
            }
        };
        let info = prepared.info;
        self.sidecar_ours = prepared.sidecar_ours;
        self.plan = match prepared.project {
            Some(project) => {
                self.fps = project.export_fps.max(1);
                self.export_settings = project.export.clone();
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
        let source = prepared.playback.clone().unwrap_or_else(|| info.path.clone());
        self.playback_path = Some(source.clone());
        self.needs_flush = false;
        self.preview_failed = false;
        if let Some(preview) = self.preview.as_mut() {
            if let Err(error) = preview.open(&source, &tracks, info.duration) {
                self.status = error;
            }
        }
        self.marks = read_marks(&info.path);
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
        match self.preview.as_mut().map(|preview| (preview.refresh(), preview.is_ready())) {
            Some((Ok(true), ready)) => {
                changed = true;
                if ready && self.status == "Loading preview…" {
                    self.status.clear();
                }
            }
            Some((Err(error), _)) => {
                self.playing = false;
                self.play_when_ready = false;
                if !self.preview_failed {
                    self.preview_failed = true;
                    changed = true;
                    self.on_preview_failed(error, workers);
                }
            }
            Some((Ok(false), _)) | None => {}
        }
        if let Some(at) = self.pending_seek {
            if self.preview.as_ref().is_some_and(|p| p.is_ready()) {
                self.pending_seek = None;
                self.seek(at);
                changed = true;
            }
        }
        let settled = self.info.is_some() && self.loading_path.is_none() && self.pending_seek.is_none();
        if self.play_when_ready && settled && self.preview.as_ref().is_some_and(|p| p.is_ready()) {
            self.play();
            changed = true;
        }
        if self.playing && !self.play_when_ready {
            changed |= self.follow_playback();
        }
        // Moving the video means the backdrop's cut-out must be repainted too.
        changed |= self.apply_preview_rect(editor_visible);
        if let Some(since) = self.dirty_since {
            if !self.saving && since.elapsed() >= AUTOSAVE_AFTER && self.drag.is_none() {
                if let Some(info) = self.info.clone() {
                    self.saving = true;
                    self.dirty_since = None;
                    workers.job(Job::SaveProject(info, self.plan.clone(), self.fps, self.export_settings.clone()));
                }
            }
        }
        changed
    }

    /// Moves the native video to the layout's preview rect; true when it moved (repaint so the
    /// backdrop's cut-out follows).
    pub(crate) fn apply_preview_rect(&mut self, visible: bool) -> bool {
        let next = if visible && self.info.is_some() && !self.preview_failed { self.preview_rect.get().unwrap_or((0, 0, 0, 0)) } else { (0, 0, 0, 0) };
        if next == self.applied_rect {
            return false;
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
        true
    }

    pub fn hide_preview(&mut self) {
        self.hole.set(None);
        self.applied_rect = (0, 0, 0, 0);
        if let Some(preview) = self.preview.as_mut() {
            let _ = preview.hide();
        }
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

/// Reads bookmark times written by the recorder beside the clip:
/// `<stem>.recall-marks.json` = `{"version":1,"marks":[seconds,...]}`. Missing → none.
pub fn read_marks(clip: &str) -> Vec<f64> {
    let path = std::path::Path::new(clip);
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { return Vec::new() };
    let sidecar = path.with_file_name(format!("{stem}.recall-marks.json"));
    let Ok(text) = std::fs::read_to_string(sidecar) else { return Vec::new() };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else { return Vec::new() };
    let mut marks: Vec<f64> = value
        .get("marks")
        .and_then(|m| m.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_f64()).filter(|t| t.is_finite() && *t >= 0.0).collect())
        .unwrap_or_default();
    marks.sort_by(f64::total_cmp);
    marks
}

#[cfg(test)]
mod marks_tests {
    #[test]
    fn reads_marks_sidecar_and_ignores_garbage() {
        let dir = std::env::temp_dir().join(format!("recall-marks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("VALORANT__2026-10-22__01-02-03.mp4");
        std::fs::write(dir.join("VALORANT__2026-10-22__01-02-03.recall-marks.json"), r#"{"version":1,"marks":[42.5,3.0,"x",-1]}"#).unwrap();
        assert_eq!(super::read_marks(&clip.to_string_lossy()), vec![3.0, 42.5]);
        assert!(super::read_marks(&dir.join("none.mp4").to_string_lossy()).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Same file, ignoring the `\\?\` verbatim prefix and letter case.
fn same_path(a: &str, b: &str) -> bool {
    let plain = |p: &str| p.strip_prefix(r"\\?\").unwrap_or(p).replace('/', "\\");
    plain(a).eq_ignore_ascii_case(&plain(b))
}

#[cfg(test)]
mod same_path_tests {
    #[test]
    fn verbatim_and_plain_paths_match() {
        assert!(super::same_path(r"\\?\V:\ALL RECORDINGS\a.mp4", r"v:\all recordings\a.mp4"));
        assert!(!super::same_path(r"\\?\V:\ALL RECORDINGS\a.mp4", r"V:\ALL RECORDINGS\b.mp4"));
    }
}
