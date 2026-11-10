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
/// The highest export rate: the largest preset. A typed rate above it is a typo (9999999 fps
/// started a background pre-encode of millions of duplicate frames).
pub const MAX_EXPORT_FPS: u32 = crate::backend::MAX_EXPORT_FPS;
const AUTOSAVE_AFTER: Duration = Duration::from_millis(700);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Drag {
    Scrub,
    TrimStart(usize),
    TrimEnd(usize),
    Gain(usize),
    /// A lane's listening level (left of the timeline): preview only, no undo step.
    Listen(usize),
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
    /// Playing outside the kept ranges (play pressed before In, in a cut, or after Out):
    /// no snap to In until the playhead reaches a kept range.
    free_play: bool,
    pub fps: u32,
    /// Lossless/re-encode, size cap and output height for the next export.
    pub export_settings: ExportSettings,
    /// Bookmarks (seconds from clip start) from `<clip>.recall-marks.json`.
    pub marks: Vec<f64>,
    /// The file the preview plays (an SSD copy when the video tier has one).
    pub playback_path: Option<String>,
    /// Where that file's picture ends.
    limits: crate::media::VideoLimits,
    /// The clip still arriving from a hard drive (`editor_state_stream.rs`).
    pub loading: stream::Loading,
    /// What an open that cannot stream is doing (the copy adds a percentage).
    open_step: Option<&'static str>,
    /// Seek to apply once a re-opened preview is ready (SSD swap keeps the position).
    pending_seek: Option<f64>,
    /// The open clip was changed since the sidecar was last written beside it.
    needs_flush: bool,
    /// A flush may rewrite or remove the sidecar beside the open clip.
    sidecar_ours: bool,
    /// A sidecar was there at open (else one found at flush is another writer's, unless this session edited).
    sidecar_at_open: bool,
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
    /// Listening levels beside the lanes (preview only; `editor_state_monitor.rs`).
    pub monitor: monitor::Monitor,
    pub listen_spans: Vec<Span>,
    /// The editor page's size in window pixels, written during paint (layout choice).
    pub area: Rc<Cell<Option<(f32, f32)>>>,
    /// Portrait layout (export panel below the timeline), chosen from `area`.
    pub portrait: bool,
    /// The remembered "Mix tracks" choice for clips whose edits never set one.
    pub mix_down_default: bool,
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
    /// Background re-encode of the current plan (`editor_state_pre_encode.rs`).
    pre: pre_encode::PreEncodeState,
}

#[path = "editor_state_persist.rs"]
mod persist;
#[path = "editor_state_edits.rs"]
mod edits;
#[path = "editor_state_playback.rs"]
mod playback;
#[path = "editor_state_stream.rs"]
pub mod stream;
#[path = "editor_state_pre_encode.rs"]
mod pre_encode;
#[path = "editor_state_monitor.rs"]
pub mod monitor;

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
            free_play: false,
            fps: 60,
            export_settings: ExportSettings::default(),
            marks: Vec::new(),
            playback_path: None,
            limits: (None, None),
            loading: Default::default(),
            open_step: None,
            pending_seek: None,
            needs_flush: false,
            sidecar_ours: false,
            sidecar_at_open: false,
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
            monitor: monitor::Monitor::default(),
            listen_spans: vec![],
            area: Rc::new(Cell::new(None)),
            portrait: false,
            mix_down_default: true,
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
            pre: Default::default(),
        }
    }

    pub fn duration(&self) -> f64 {
        self.info.as_ref().map_or(0.0, |info| info.duration.max(0.01))
    }

    /// `info.path` is canonical (`\\?\E:\...`); tiles pass plain paths.
    pub fn is_open(&self, path: &str) -> bool {
        let open: Vec<&str> = self.info.iter().map(|info| info.path.as_str()).chain(self.loading_path.as_deref()).collect();
        if open.iter().any(|p| same_path(p, path)) {
            return true;
        }
        // The editor holds the canonical path; a tile names the clip through the folder as
        // configured. Through a junction or a linked folder only the resolved path matches.
        let Some(real) = (!open.is_empty()).then(|| std::fs::canonicalize(path).ok()).flatten() else { return false };
        let real = real.to_string_lossy();
        open.iter().any(|p| same_path(p, &real))
    }

    pub fn open(&mut self, path: String, workers: &Workers) {
        self.flush(workers);
        self.pause();
        self.hide_preview();
        if let Some(Err(error)) = self.preview.as_mut().map(|preview| preview.close_stream()) {
            self.status = error;
        }
        self.info = None;
        self.visuals = VisualAssets::default();
        self.warning = None;
        self.exported = None;
        self.export_error = None;
        self.drop_pre_encode();
        self.status = "Opening clip…".into();
        self.generation = self.generation.wrapping_add(1);
        self.loading_path = Some(path.clone());
        self.loading = Default::default();
        self.pending_seek = None;
        self.open_step = None;
        workers.open_clip(self.generation, path);
    }

    /// An open that cannot stream says what it is doing.
    pub fn open_step(&mut self, generation: u64, text: &'static str) {
        if generation == self.generation && self.loading_path.is_some() {
            self.status = text.into();
            self.open_step = Some(text);
        }
    }

    pub fn apply_prepared(&mut self, generation: u64, result: Result<PreparedClip, String>) {
        if generation != self.generation {
            return;
        }
        self.loading_path = None;
        self.open_step = None;
        let streaming = self.loading.generation == Some(generation);
        let prepared = match result {
            Ok(prepared) if streaming => return self.upgrade(prepared),
            Ok(prepared) => prepared,
            Err(error) if streaming => {
                // The clip keeps playing what has loaded; the rest stays grey.
                self.loading.prepared = true;
                self.last_error = Some(error);
                return;
            }
            Err(error) => {
                self.status = error;
                self.playing = false;
                self.play_when_ready = false;
                return;
            }
        };
        let info = prepared.info;
        self.sidecar_ours = prepared.sidecar_ours;
        self.sidecar_at_open = prepared.sidecar_at_open;
        self.adopt(info.clone(), prepared.project, prepared.warning);
        let tracks = self.preview_tracks(&info);
        let source = prepared.playback.clone().unwrap_or_else(|| info.path.clone());
        self.playback_path = Some(source.clone());
        self.limits = prepared.limits;
        self.needs_flush = false;
        self.preview_failed = false;
        if let Some(preview) = self.preview.as_mut() {
            if let Err(error) = preview.open(&source, &tracks, info.duration, prepared.limits) {
                self.status = error;
            }
        }
        self.marks = prepared.marks;
        self.info = Some(info);
        self.visuals = prepared.visuals;
        self.applied_rect = (0, 0, 0, 0);
    }

    /// The new clip's plan and edit state, from its saved project or untouched.
    fn adopt(&mut self, info: ClipInfo, project: Option<crate::backend::Project>, warning: Option<String>) {
        self.plan = match project {
            Some(project) => {
                self.fps = project.export_fps.clamp(1, MAX_EXPORT_FPS);
                self.export_settings = project.export.clone();
                project.plan
            }
            None => EditPlan { mix_down: None,
                ranges: vec![TimeRange::new(0.0, info.duration)],
                // App tracks start out of the mix: Desktop audio or the game already holds them.
                audio: info.default_audio(),
            },
        };
        self.mix_down_default = crate::services::editor_prefs::EditorPrefs::load().mix_down;
        self.selected = 0;
        self.undo.clear();
        self.redo.clear();
        self.saved = true;
        self.dirty_since = None;
        self.warning = warning;
        self.gain_spans = (0..info.audio.len()).map(|_| Rc::new(Cell::new(None))).collect();
        self.listen_spans = (0..info.audio.len()).map(|_| Rc::new(Cell::new(None))).collect();
        self.monitor = monitor::Monitor::default();
        self.needs_flush = false;
        self.status = "Loading preview…".into();
        // The playhead opens on the first kept part, landing there once the preview is ready.
        self.pending_seek = start_at(&self.plan.ranges);
    }

    fn preview_tracks(&self, info: &ClipInfo) -> Vec<PreviewTrack> {
        info.audio
            .iter()
            .map(|track| {
                // Export level plus listening level (`editor_state_monitor.rs`).
                let (gain_db, muted) = self.preview_audio(track.stream_index);
                PreviewTrack { stream_index: track.stream_index, path: track.preview_path.clone(), gain_db, muted }
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
        if let Some(notice) = self.preview.as_mut().and_then(NativePreview::take_sound_notice) {
            changed |= self.on_sound_notice(notice);
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
        changed |= self.tick_loading(editor_visible);
        // The background re-encode of an edited clip (`editor_state_pre_encode.rs`): starts once
        // the edit settles, the clip has loaded and the recorder is idle.
        changed |= self.tick_pre_encode();
        if self.playing && !self.play_when_ready && !self.loading.buffering {
            changed |= self.follow_playback();
        }
        if let Some(step) = self.open_step {
            let text = match crate::services::storage::copy_progress() {
                Some(share) => format!("{} {}%", step, (share * 100.0).floor() as u32),
                None => step.to_owned(),
            };
            if text != self.status {
                self.status = text;
                changed = true;
            }
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

/// Same file, ignoring the `\\?\` verbatim prefix and letter case.
pub(super) fn same_path(a: &str, b: &str) -> bool {
    let plain = |p: &str| p.strip_prefix(r"\\?\").unwrap_or(p).replace('/', "\\");
    plain(a).eq_ignore_ascii_case(&plain(b))
}

/// Where a freshly opened clip's playhead starts: the first kept part's In, or None for 0:00.
fn start_at(ranges: &[TimeRange]) -> Option<f64> {
    ranges.first().map(|range| range.start).filter(|start| *start > 0.0)
}

#[cfg(test)]
#[path = "editor_state_misc_tests.rs"]
mod misc_tests;
