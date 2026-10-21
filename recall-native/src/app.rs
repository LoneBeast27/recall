//! Real GPUI edit surface. Expensive preparation, project I/O, and exports stay on the worker.
use std::{
    borrow::Cow,
    collections::HashSet,
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use gpui::{
    App, Application, Bounds, Context, ElementId, Entity, FocusHandle, KeyDownEvent, MouseButton,
    MouseMoveEvent, ObjectFit, Render, Timer, Window, WindowBounds, WindowOptions, canvas, div,
    img, prelude::*, px, relative, size,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::{
    backend::{self, ClipEntry, ExportSummary, PreparedClip, VisualAssets},
    edit_model::{AudioSetting, ClipInfo, EditPlan, TimeRange},
    icons::{Icon, icon},
    media::{NativePreview, PreviewTrack},
    theme,
};

enum Job {
    List,
    Prepare(u64, String),
    Save(ClipInfo, EditPlan, u32),
    Export(ClipInfo, EditPlan, u32),
}
enum ResultEvent {
    Clips(Result<Vec<ClipEntry>, String>),
    Prepared(u64, Result<PreparedClip, String>),
    Saved(Result<String, String>),
    Exported(Result<ExportSummary, String>),
}

struct ThumbnailResult {
    source: String,
    result: Result<String, String>,
}

struct Editor {
    jobs: Sender<Job>,
    results: Receiver<ResultEvent>,
    thumbnail_jobs: mpsc::SyncSender<String>,
    thumbnail_results: Receiver<ThumbnailResult>,
    thumbnail_queued: HashSet<String>,
    thumbnail_failed: HashSet<String>,
    clips: Vec<ClipEntry>,
    clip_limit: usize,
    info: Option<ClipInfo>,
    visuals: VisualAssets,
    plan: EditPlan,
    undo: Vec<EditPlan>,
    redo: Vec<EditPlan>,
    selected: usize,
    preview: Option<NativePreview>,
    playing: bool,
    fps: u32,
    fps_input: String,
    fps_focus: FocusHandle,
    status: String,
    saved: Option<String>,
    exported: Option<String>,
    preview_bounds: (i32, i32, i32, i32),
    canvas_bounds: Arc<Mutex<Option<(i32, i32, i32, i32)>>>,
    timeline_bounds: Arc<Mutex<Option<(f32, f32, f32)>>>,
    dirty: bool,
    prepare_generation: u64,
    play_started: Option<Instant>,
    saw_native_playing: bool,
    diagnostics_written: Instant,
}

pub fn run() -> Result<(), String> {
    Application::new().run(|cx: &mut App| {
        if let Err(error) = cx.text_system().add_fonts(vec![
            Cow::Borrowed(include_bytes!("../assets/fonts/Outfit-Medium.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/Lilex-Regular.ttf")),
        ]) {
            eprintln!("load Recall fonts: {error}");
        }
        let (width, height) = native_window_size();
        let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
        if let Err(error) = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                focus: std::env::var_os("RECALL_NATIVE_BACKGROUND").is_none(),
                ..Default::default()
            },
            |window, cx| {
                let editor = cx.new(|cx| Editor::new(window, cx));
                Editor::poll_loop(editor.clone(), window, cx);
                editor
            },
        ) {
            eprintln!("open Recall editor: {error}");
            cx.quit();
            return;
        }
        if std::env::var_os("RECALL_NATIVE_BACKGROUND").is_none() {
            cx.activate(true);
        }
    });
    Ok(())
}

impl Editor {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (result_tx, result_rx) = mpsc::channel::<ResultEvent>();
        let (thumbnail_tx, thumbnail_rx) = mpsc::sync_channel::<String>(8);
        let (thumbnail_result_tx, thumbnail_result_rx) = mpsc::channel::<ThumbnailResult>();
        thread::Builder::new()
            .name("recall-editor-worker".into())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    let event = match job {
                        Job::List => ResultEvent::Clips(backend::list_clips()),
                        Job::Prepare(generation, path) => {
                            ResultEvent::Prepared(generation, backend::prepare_clip(&path))
                        }
                        Job::Save(info, plan, fps) => {
                            ResultEvent::Saved(backend::save_project(&info, &plan, fps))
                        }
                        Job::Export(info, plan, fps) => {
                            ResultEvent::Exported(backend::export_clip(&info, &plan, fps))
                        }
                    };
                    let _ = result_tx.send(event);
                }
            })
            .expect("start Recall editor worker");
        thread::Builder::new()
            .name("recall-thumbnail-worker".into())
            .spawn(move || {
                while let Ok(source) = thumbnail_rx.recv() {
                    let result = backend::thumbnail(&source);
                    let _ = thumbnail_result_tx.send(ThumbnailResult { source, result });
                }
            })
            .expect("start Recall thumbnail worker");
        let mut status = "Loading clip library…".to_owned();
        let preview = parent_hwnd(window)
            .and_then(|hwnd| NativePreview::new(hwnd))
            .map_err(|e| {
                status = e.clone();
                e
            })
            .ok();
        let editor = Self {
            jobs: job_tx,
            results: result_rx,
            thumbnail_jobs: thumbnail_tx,
            thumbnail_results: thumbnail_result_rx,
            thumbnail_queued: HashSet::new(),
            thumbnail_failed: HashSet::new(),
            clips: vec![],
            clip_limit: 60,
            info: None,
            visuals: VisualAssets::default(),
            plan: EditPlan::default(),
            undo: vec![],
            redo: vec![],
            selected: 0,
            preview,
            playing: false,
            fps: 60,
            fps_input: "60".into(),
            fps_focus: cx.focus_handle().tab_stop(true),
            status,
            saved: None,
            exported: None,
            preview_bounds: (0, 0, 0, 0),
            canvas_bounds: Arc::new(Mutex::new(None)),
            timeline_bounds: Arc::new(Mutex::new(None)),
            dirty: false,
            prepare_generation: 0,
            play_started: None,
            saw_native_playing: false,
            diagnostics_written: Instant::now() - Duration::from_secs(1),
        };
        let _ = editor.jobs.send(Job::List);
        editor
    }
    fn poll_loop(entity: Entity<Self>, window: &mut Window, cx: &mut App) {
        window
            .spawn(cx, async move |cx| {
                loop {
                    Timer::after(Duration::from_millis(30)).await;
                    if entity.update(cx, |this, cx| this.poll(cx)).is_err() {
                        break;
                    }
                }
            })
            .detach();
    }
    fn poll(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        while let Ok(event) = self.results.try_recv() {
            changed = true;
            match event {
                ResultEvent::Clips(Ok(clips)) => {
                    self.clips = clips;
                    self.clip_limit = 60;
                    self.thumbnail_queued.clear();
                    self.thumbnail_failed.clear();
                    self.status = if self.clips.is_empty() {
                        "No MP4 clips in RECALL_OUT_DIR".into()
                    } else {
                        "Choose a clip".into()
                    };
                    if let Some(path) = std::env::var_os("RECALL_NATIVE_CLIP")
                        .map(|v| v.to_string_lossy().into_owned())
                        .or_else(|| self.clips.first().map(|c| c.path.clone()))
                    {
                        self.prepare(path);
                    }
                }
                ResultEvent::Clips(Err(e)) | ResultEvent::Saved(Err(e)) => self.status = e,
                ResultEvent::Prepared(generation, result) => {
                    if generation == self.prepare_generation {
                        match result {
                            Ok(prepared) => self.apply_prepared(prepared),
                            Err(error) => self.status = error,
                        }
                    }
                }
                ResultEvent::Saved(Ok(path)) => {
                    self.saved = Some(path.clone());
                    self.status = format!("Saved {path}");
                    self.dirty = false;
                }
                ResultEvent::Exported(Ok(done)) => {
                    self.exported = Some(done.path.clone());
                    self.status = format!("Exported {} frames at {} fps", done.frames, done.fps);
                }
                ResultEvent::Exported(Err(e)) => self.status = e,
            }
        }
        while let Ok(result) = self.thumbnail_results.try_recv() {
            self.thumbnail_queued.remove(&result.source);
            match result.result {
                Ok(thumbnail) => {
                    if let Some(clip) = self
                        .clips
                        .iter_mut()
                        .find(|clip| clip.path == result.source)
                    {
                        clip.thumb = Some(thumbnail);
                        changed = true;
                    }
                }
                Err(error) => {
                    self.thumbnail_failed.insert(result.source);
                    if !self.status.contains("thumbnail") {
                        self.status = format!("Thumbnail unavailable: {error}");
                        changed = true;
                    }
                }
            }
        }
        self.queue_visible_thumbnails();
        if let Some(preview) = self.preview.as_mut() {
            match preview.refresh() {
                Ok(true) => {
                    changed = true;
                    if preview.is_ready() && self.status == "Preview loading…" {
                        self.status = "Ready to edit".into();
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    if self.status != error || self.playing {
                        self.status = error;
                        self.playing = false;
                        changed = true;
                    }
                }
            }
            if let Some(error) = preview.error() {
                if self.status != error {
                    self.status = error;
                    changed = true;
                }
                self.playing = false;
            }
            if self.playing {
                if let Ok(pos) = preview.position() {
                    let ended = self
                        .plan
                        .ranges
                        .last()
                        .is_some_and(|last| pos >= last.end - 0.001);
                    let native_playing = preview.is_playing();
                    self.saw_native_playing |= native_playing;
                    let settled = self
                        .play_started
                        .is_some_and(|started| started.elapsed() >= Duration::from_millis(250));
                    if ended || (settled && self.saw_native_playing && !native_playing) {
                        self.playing = false;
                        self.play_started = None;
                        let _ = preview.pause();
                    } else {
                        self.skip_removed(pos);
                    }
                    changed = true;
                }
            }
            self.apply_preview_rect();
        }
        if changed {
            cx.notify();
        }
        self.write_diagnostics();
    }
    fn write_diagnostics(&mut self) {
        if self.diagnostics_written.elapsed() < Duration::from_millis(500) {
            return;
        }
        let Some(path) = std::env::var_os("RECALL_NATIVE_DIAGNOSTICS") else {
            return;
        };
        self.diagnostics_written = Instant::now();
        let current = self
            .preview
            .as_ref()
            .and_then(|preview| preview.position().ok())
            .unwrap_or(0.);
        let preview = self
            .preview
            .as_ref()
            .map(|preview| preview.diagnostics())
            .unwrap_or(serde_json::Value::Null);
        let source_duration = self.info.as_ref().map_or(0., |info| info.duration);
        let tracks = self.info.as_ref().map(|info| info.audio.iter().map(|audio| serde_json::json!({"stream_index": audio.stream_index, "name": audio.name, "waveform_bins": audio.waveform.len(), "sample_rate": audio.sample_rate, "channels": audio.channels, "gain_db": self.plan.audio.iter().find(|setting| setting.stream_index == audio.stream_index).map_or(0., |setting| setting.gain_db), "muted": self.plan.audio.iter().find(|setting| setting.stream_index == audio.stream_index).is_some_and(|setting| setting.muted)})).collect::<Vec<_>>()).unwrap_or_default();
        let payload = serde_json::json!({
            "active_source": self.info.as_ref().map(|info| &info.path),
            "source": self.info.as_ref().map(|info| serde_json::json!({
                "duration": info.duration, "width": info.width, "height": info.height,
                "video_fps": info.video_fps, "bytes": info.source_bytes,
            })),
            "ready": self.preview.as_ref().is_some_and(|preview| preview.is_ready()),
            "playing": self.playing,
            "current_time": current,
            "preview": preview,
            "viewport": {"requested_width": native_window_size().0, "requested_height": native_window_size().1},
            "source_duration": source_duration,
            "retained_duration": self.plan.duration(),
            "ranges": self.plan.ranges,
            "selected_range": self.selected,
            "filmstrip": {"poster": self.visuals.poster, "frames": self.visuals.filmstrip},
            "audio": tracks,
            "export": {"active_fps": self.fps, "presets": [60,120,240,480,960]},
            "status": self.status,
            "loading": self.info.is_none(),
            "canvas_bounds": self.canvas_bounds.lock().ok().and_then(|bounds| *bounds),
            "timeline_bounds": self.timeline_bounds.lock().ok().and_then(|bounds| *bounds),
            "library": {
                "visible_clips": self.clip_limit.min(self.clips.len()),
                "clip_count": self.clips.len(),
                "thumbs_populated": self.clips.iter().take(self.clip_limit).filter(|clip| clip.thumb.is_some()).count(),
                "thumbnail_queue": self.thumbnail_queued.len(),
                "thumbnail_failed": self.thumbnail_failed.len(),
            }
        });
        let path = std::path::PathBuf::from(path);
        let temp = path.with_extension(format!("{}.tmp", std::process::id()));
        if std::fs::write(&temp, serde_json::to_vec(&payload).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(temp, path);
        }
    }
    fn prepare(&mut self, path: String) {
        self.status = "Preparing clip…".into();
        self.playing = false;
        self.play_started = None;
        if let Some(preview) = self.preview.as_mut() {
            let _ = preview.pause();
            let _ = preview.hide();
        }
        self.preview_bounds = (0, 0, 0, 0);
        self.visuals = VisualAssets::default();
        self.prepare_generation = self.prepare_generation.wrapping_add(1);
        let _ = self.jobs.send(Job::Prepare(self.prepare_generation, path));
    }
    fn apply_prepared(&mut self, prepared: PreparedClip) {
        let visuals = prepared.visuals;
        let info = prepared.info;
        self.plan = prepared
            .project
            .map(|p| {
                self.fps = p.export_fps;
                self.fps_input = self.fps.to_string();
                p.plan
            })
            .unwrap_or_else(|| EditPlan {
                ranges: vec![TimeRange {
                    start: 0.0,
                    end: info.duration,
                }],
                audio: info
                    .audio
                    .iter()
                    .map(|a| AudioSetting {
                        stream_index: a.stream_index,
                        gain_db: 0.0,
                        muted: false,
                    })
                    .collect(),
            });
        self.selected = 0;
        self.undo.clear();
        self.redo.clear();
        self.status = prepared
            .warning
            .unwrap_or_else(|| "Preview loading…".into());
        let tracks = self.preview_tracks(&info);
        if let Some(preview) = self.preview.as_mut() {
            if let Err(e) = preview.open(&info.path, &tracks) {
                self.status = e;
            }
        }
        self.info = Some(info);
        self.visuals = visuals;
        self.preview_bounds = (0, 0, 0, 0);
        self.dirty = false;
    }
    fn queue_visible_thumbnails(&mut self) {
        for clip in self.clips.iter().take(self.clip_limit) {
            if clip.thumb.is_some()
                || self.thumbnail_queued.contains(&clip.path)
                || self.thumbnail_failed.contains(&clip.path)
            {
                continue;
            }
            match self.thumbnail_jobs.try_send(clip.path.clone()) {
                Ok(()) => {
                    self.thumbnail_queued.insert(clip.path.clone());
                }
                Err(mpsc::TrySendError::Full(_)) => break,
                Err(mpsc::TrySendError::Disconnected(_)) => break,
            }
        }
    }
    fn preview_tracks(&self, info: &ClipInfo) -> Vec<PreviewTrack> {
        info.audio
            .iter()
            .map(|a| {
                let setting = self
                    .plan
                    .audio
                    .iter()
                    .find(|s| s.stream_index == a.stream_index);
                PreviewTrack {
                    stream_index: a.stream_index,
                    path: a.preview_path.clone(),
                    gain_db: setting.map_or(0.0, |s| s.gain_db),
                    muted: setting.is_some_and(|s| s.muted),
                }
            })
            .collect()
    }
    fn apply_preview_rect(&mut self) {
        let next = self
            .canvas_bounds
            .lock()
            .ok()
            .and_then(|v| *v)
            .unwrap_or((0, 0, 0, 0));
        if next != self.preview_bounds {
            self.preview_bounds = next;
            if let Some(p) = self.preview.as_mut() {
                if next.2 <= 0 || next.3 <= 0 || self.info.is_none() {
                    let _ = p.hide();
                    return;
                }
                if let Err(e) = p.rect(next.0, next.1, next.2, next.3) {
                    self.status = e;
                }
            }
        }
    }
    fn revise(&mut self) {
        self.undo.push(self.plan.clone());
        self.redo.clear();
        self.dirty = true;
    }
    fn toggle_play(&mut self) {
        let Some(p) = self.preview.as_mut() else {
            return;
        };
        let r = if self.playing { p.pause() } else { p.play() };
        match r {
            Ok(_) => {
                self.playing = !self.playing;
                self.play_started = self.playing.then(Instant::now);
                self.saw_native_playing = false;
                self.status = if self.playing {
                    "Playing".into()
                } else {
                    "Paused".into()
                }
            }
            Err(e) => self.status = e,
        }
    }
    fn seek(&mut self, seconds: f64) {
        if let Some(p) = self.preview.as_mut() {
            if let Err(e) = p.seek(seconds.max(0.)) {
                self.status = e;
            }
        }
    }
    fn scrub_from_x(&mut self, x: f32) {
        let Some(info) = self.info.as_ref() else {
            return;
        };
        let Some((left, width, _)) = self.timeline_bounds.lock().ok().and_then(|v| *v) else {
            return;
        };
        if width <= 1. {
            return;
        }
        self.seek(((x - left) / width).clamp(0., 1.) as f64 * info.duration);
    }
    fn skip_removed(&mut self, pos: f64) {
        if let Some(range) = self
            .plan
            .ranges
            .iter()
            .find(|r| pos >= r.start && pos < r.end)
        {
            let _ = range;
            return;
        }
        if let Some(next) = self.plan.ranges.iter().find(|r| r.start > pos) {
            self.seek(next.start)
        } else {
            self.playing = false;
            if let Some(p) = self.preview.as_mut() {
                let _ = p.pause();
            }
        }
    }
    fn trim_start(&mut self, delta: f64) {
        if self.selected >= self.plan.ranges.len() {
            return;
        }
        self.revise();
        let target = {
            let r = &mut self.plan.ranges[self.selected];
            r.start = (r.start + delta).min(r.end - 0.05).max(0.0);
            r.start
        };
        self.seek(target)
    }
    fn trim_end(&mut self, delta: f64) {
        if self.selected >= self.plan.ranges.len() {
            return;
        }
        let duration = self.info.as_ref().map_or(0., |i| i.duration);
        self.revise();
        let target = {
            let r = &mut self.plan.ranges[self.selected];
            r.end = (r.end + delta).max(r.start + 0.05).min(duration);
            r.end
        };
        self.seek(target)
    }
    fn set_in_at_playhead(&mut self) {
        let at = self.preview.as_ref().and_then(|p| p.position().ok());
        if let Some(at) = at {
            if self.selected < self.plan.ranges.len() {
                self.revise();
                let r = &mut self.plan.ranges[self.selected];
                r.start = at.min(r.end - 0.05).max(0.0);
            }
        }
    }
    fn set_out_at_playhead(&mut self) {
        let at = self.preview.as_ref().and_then(|p| p.position().ok());
        if let Some(at) = at {
            if self.selected < self.plan.ranges.len() {
                self.revise();
                let duration = self.info.as_ref().map_or(0.0, |i| i.duration);
                let r = &mut self.plan.ranges[self.selected];
                r.end = at.max(r.start + 0.05).min(duration);
            }
        }
    }
    fn split(&mut self) {
        let Some(p) = self.preview.as_mut() else {
            return;
        };
        let Ok(at) = p.position() else { return };
        if self.selected >= self.plan.ranges.len() {
            return;
        }
        let r = self.plan.ranges[self.selected].clone();
        if at - r.start < 0.05 || r.end - at < 0.05 {
            return;
        }
        self.revise();
        self.plan.ranges[self.selected] = TimeRange {
            start: r.start,
            end: at,
        };
        self.plan.ranges.insert(
            self.selected + 1,
            TimeRange {
                start: at,
                end: r.end,
            },
        );
        self.selected += 1;
    }
    fn delete_selected(&mut self) {
        if self.plan.ranges.len() <= 1 || self.selected >= self.plan.ranges.len() {
            return;
        }
        self.revise();
        self.plan.ranges.remove(self.selected);
        self.selected = self.selected.min(self.plan.ranges.len() - 1);
    }
    fn undo(&mut self) {
        if let Some(previous) = self.undo.pop() {
            self.redo.push(self.plan.clone());
            self.plan = previous;
            self.dirty = true;
        }
    }
    fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(self.plan.clone());
            self.plan = next;
            self.dirty = true;
        }
    }
    fn set_gain(&mut self, index: usize, delta: f64) {
        let Some(info) = self.info.as_ref() else {
            return;
        };
        if index >= info.audio.len() {
            return;
        }
        let stream = info.audio[index].stream_index;
        self.revise();
        if !self
            .plan
            .audio
            .iter()
            .any(|setting| setting.stream_index == stream)
        {
            self.plan.audio.push(AudioSetting {
                stream_index: stream,
                gain_db: 0.,
                muted: false,
            });
        }
        let setting = self
            .plan
            .audio
            .iter_mut()
            .find(|setting| setting.stream_index == stream)
            .expect("inserted audio setting");
        setting.gain_db = (setting.gain_db + delta).clamp(-60., 12.);
        if let Some(p) = self.preview.as_mut() {
            if let Err(e) = p.set_audio(stream, setting.gain_db, setting.muted) {
                self.status = e;
            }
        }
    }
    fn mute(&mut self, index: usize) {
        let Some(info) = self.info.as_ref() else {
            return;
        };
        if index >= info.audio.len() {
            return;
        }
        let stream = info.audio[index].stream_index;
        self.revise();
        if !self
            .plan
            .audio
            .iter()
            .any(|setting| setting.stream_index == stream)
        {
            self.plan.audio.push(AudioSetting {
                stream_index: stream,
                gain_db: 0.,
                muted: false,
            });
        }
        let setting = self
            .plan
            .audio
            .iter_mut()
            .find(|setting| setting.stream_index == stream)
            .expect("inserted audio setting");
        setting.muted = !setting.muted;
        if let Some(p) = self.preview.as_mut() {
            if let Err(e) = p.set_audio(stream, setting.gain_db, setting.muted) {
                self.status = e;
            }
        }
    }
    fn save(&mut self) {
        if let Some(info) = self.info.clone() {
            self.status = "Saving project…".into();
            let _ = self.jobs.send(Job::Save(info, self.plan.clone(), self.fps));
        }
    }
    fn export(&mut self) {
        if let Some(info) = self.info.clone() {
            self.status = format!("Exporting at {} fps…", self.fps);
            let _ = self
                .jobs
                .send(Job::Export(info, self.plan.clone(), self.fps));
        }
    }
    fn button(
        &self,
        id: impl Into<ElementId>,
        label: impl Into<String>,
        cx: &mut Context<Self>,
        f: impl Fn(&mut Self) + 'static,
    ) -> gpui::AnyElement {
        div()
            .id(id)
            .px_2()
            .py_1()
            .rounded_sm()
            .border_1()
            .border_color(theme::c(theme::BORDER_STRONG))
            .bg(theme::c(theme::FLOATING))
            .text_color(theme::c(theme::TEXT))
            .cursor_pointer()
            .active(|d| d.bg(theme::c(theme::TOP)))
            .child(label.into())
            .on_click(cx.listener(move |this, _, _, cx| {
                f(this);
                cx.notify();
            }))
            .into_any_element()
    }
    fn icon_button(
        &self,
        id: impl Into<ElementId>,
        kind: Icon,
        label: impl Into<String>,
        cx: &mut Context<Self>,
        f: impl Fn(&mut Self) + 'static,
    ) -> gpui::AnyElement {
        div()
            .id(id)
            .h(px(34.))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .rounded_sm()
            .border_1()
            .border_color(theme::c(theme::BORDER_STRONG))
            .bg(theme::c(theme::FLOATING))
            .text_color(theme::c(theme::TEXT))
            .cursor_pointer()
            .active(|element| element.bg(theme::c(theme::TOP)))
            .child(icon(kind, theme::c(theme::MUTED)))
            .child(label.into())
            .on_click(cx.listener(move |this, _, _, cx| {
                f(this);
                cx.notify();
            }))
            .into_any_element()
    }
}

fn native_window_size() -> (f32, f32) {
    let Some(value) = std::env::var_os("RECALL_NATIVE_WINDOW_SIZE") else {
        return (1280., 820.);
    };
    let value = value.to_string_lossy();
    let Some((width, height)) = value.split_once('x') else {
        return (1280., 820.);
    };
    let parsed = || Some((width.parse::<f32>().ok()?, height.parse::<f32>().ok()?));
    parsed()
        .filter(|(width, height)| *width >= 1000. && *height >= 680.)
        .unwrap_or((1280., 820.))
}

fn clip_title(value: &str) -> String {
    let stem = std::path::Path::new(value)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(value);
    let identity = stem.split("__").next().unwrap_or(stem);
    let readable = identity.replace(['_', '-'], " ");
    let mut chars = readable.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => "Untitled recording".into(),
    }
}

fn readable_date(epoch_seconds: u64) -> String {
    let days = (epoch_seconds / 86_400) as i64;
    let seconds = epoch_seconds % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let hour = seconds / 3600;
    let minute = (seconds % 3600) / 60;
    format!("{year:04}-{month:02}-{day:02}  {hour:02}:{minute:02} UTC")
}

fn timecode(seconds: f64) -> String {
    let seconds = seconds.max(0.0);
    let hours = (seconds / 3600.0).floor() as u64;
    let minutes = ((seconds % 3600.0) / 60.0).floor() as u64;
    let whole = (seconds % 60.0).floor() as u64;
    let centiseconds = ((seconds.fract() * 100.0).round() as u64).min(99);
    format!("{hours:02}:{minutes:02}:{whole:02}.{centiseconds:02}")
}

fn channel_label(channels: u16) -> &'static str {
    match channels {
        1 => "Mono",
        2 => "Stereo",
        _ => "Multichannel",
    }
}

impl Editor {
    fn cycle_fps(&mut self) {
        const RATES: [u32; 5] = [60, 120, 240, 480, 960];
        let next = RATES
            .iter()
            .position(|rate| *rate == self.fps)
            .map_or(0, |index| (index + 1) % RATES.len());
        self.fps = RATES[next];
        self.fps_input = self.fps.to_string();
        self.status = format!("Export rate set to {} fps", self.fps);
    }

    fn apply_fps_input(&mut self) {
        match self
            .fps_input
            .parse::<u32>()
            .ok()
            .filter(|value| *value > 0 && *value <= i32::MAX as u32)
        {
            Some(value) => {
                self.fps = value;
                self.status = format!("Export rate set to {value} fps");
            }
            None => {
                self.status = "Export rate must be a positive integer up to 2147483647".into();
            }
        }
    }

    fn fps_key_down(&mut self, event: &KeyDownEvent) {
        match event.keystroke.key.as_str() {
            "backspace" => {
                self.fps_input.pop();
                self.apply_fps_input();
            }
            "enter" => self.apply_fps_input(),
            "escape" => {
                self.fps_input = self.fps.to_string();
                self.status = format!("Export rate remains {} fps", self.fps);
            }
            _ => {
                let Some(characters) = event.keystroke.key_char.as_ref() else {
                    return;
                };
                if characters
                    .chars()
                    .all(|character| character.is_ascii_digit())
                    && self.fps_input.len() + characters.len() <= 10
                {
                    self.fps_input.push_str(characters);
                    self.apply_fps_input();
                }
            }
        }
    }

    fn render_header(&self, title: String, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .h(px(56.))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .px_4()
            .bg(theme::c(theme::BASE))
            .border_b_1()
            .border_color(theme::c(theme::BORDER))
            .child(
                div()
                    .min_w_0()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::c(theme::MUTED))
                            .child("Recall editor"),
                    )
                    .child(
                        div()
                            .text_lg()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(title),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(self.icon_button("save", Icon::Save, "Save edits", cx, Self::save))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .id("fps-input")
                                    .track_focus(&self.fps_focus)
                                    .w(px(76.))
                                    .h(px(34.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .justify_end()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(theme::c(theme::BORDER_STRONG))
                                    .bg(theme::c(theme::OVERLAY))
                                    .font_family("Lilex")
                                    .text_sm()
                                    .focus(|element| element.border_color(theme::c(theme::FOCUS)))
                                    .on_click(cx.listener(|editor, _, window, cx| {
                                        window.focus(&editor.fps_focus);
                                        cx.notify();
                                    }))
                                    .on_key_down(cx.listener(
                                        |editor, event: &KeyDownEvent, _, cx| {
                                            editor.fps_key_down(event);
                                            cx.notify();
                                        },
                                    ))
                                    .child(self.fps_input.clone()),
                            )
                            .child(
                                div()
                                    .font_family("Lilex")
                                    .text_xs()
                                    .text_color(theme::c(theme::MUTED))
                                    .child("fps"),
                            )
                            .child(
                                self.button("fps-cycle", "Preset", cx, |editor| editor.cycle_fps()),
                            ),
                    )
                    .child(self.icon_button("export", Icon::Export, "Export", cx, Self::export)),
            )
            .into_any_element()
    }

    fn render_library(
        &self,
        selected_path: Option<&str>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let cards = self
            .clips
            .iter()
            .take(self.clip_limit)
            .map(|clip| {
                let path = clip.path.clone();
                let selected = selected_path.is_some_and(|selected| selected == path);
                let image = clip.thumb.clone().map_or_else(
                    || {
                        div()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(theme::c(theme::OVERLAY))
                            .child(icon(Icon::Filmstrip, theme::c(theme::PLACEHOLDER)))
                            .into_any_element()
                    },
                    |thumbnail| {
                        img(PathBuf::from(thumbnail))
                            .size_full()
                            .object_fit(ObjectFit::Cover)
                            .into_any_element()
                    },
                );
                div()
                    .id(("clip", clip.mtime))
                    .h(px(68.))
                    .flex_none()
                    .p_2()
                    .flex()
                    .gap_2()
                    .items_center()
                    .rounded_sm()
                    .cursor_pointer()
                    .border_1()
                    .border_color(if selected {
                        theme::c(theme::FOCUS)
                    } else {
                        theme::c(theme::BORDER_SUBTLE)
                    })
                    .bg(if selected {
                        theme::c(theme::TOP)
                    } else {
                        theme::c(theme::BASE)
                    })
                    .child(
                        div()
                            .w(px(88.))
                            .h(px(50.))
                            .flex_none()
                            .rounded_sm()
                            .overflow_hidden()
                            .child(image),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .child(
                                div()
                                    .text_sm()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(clip_title(&clip.name)),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .font_family("Lilex")
                                    .text_xs()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_color(theme::c(theme::MUTED))
                                    .child(readable_date(clip.mtime)),
                            )
                            .child(
                                div()
                                    .font_family("Lilex")
                                    .text_xs()
                                    .text_color(theme::c(theme::PLACEHOLDER))
                                    .child(format!("{} MB", clip.bytes / 1_048_576)),
                            ),
                    )
                    .on_click(cx.listener(move |editor, _, _, cx| {
                        editor.prepare(path.clone());
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        div()
            .w(px(248.))
            .flex_none()
            .p_3()
            .bg(theme::c(theme::BASE))
            .border_r_1()
            .border_color(theme::c(theme::BORDER))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child("Local recordings")
                    .child(
                        div()
                            .font_family("Lilex")
                            .text_xs()
                            .text_color(theme::c(theme::MUTED))
                            .child(format!("{} clips", self.clips.len())),
                    ),
            )
            .child(
                div()
                    .id("clip-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(cards),
            )
            .child(if self.clips.len() > self.clip_limit {
                self.button(
                    "more-clips",
                    format!("Load more  {} / {}", self.clip_limit, self.clips.len()),
                    cx,
                    |editor| {
                        editor.clip_limit = (editor.clip_limit + 60).min(editor.clips.len());
                        editor.queue_visible_thumbnails();
                    },
                )
            } else {
                div()
                    .font_family("Lilex")
                    .text_xs()
                    .text_color(theme::c(theme::MUTED))
                    .child("End of library")
                    .into_any_element()
            })
            .into_any_element()
    }

    fn render_preview(
        &self,
        current: f64,
        duration: f64,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let preview_canvas_bounds = Arc::clone(&self.canvas_bounds);
        let ready = self.preview.as_ref().is_some_and(NativePreview::is_ready);
        div()
            .flex_1()
            .min_w_0()
            .p_3()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                div()
                    .w(px(562.))
                    .h(px(316.))
                    .flex_none()
                    .relative()
                    .overflow_hidden()
                    .rounded_md()
                    .bg(theme::c(theme::BASE))
                    .border_1()
                    .border_color(theme::c(theme::BORDER_STRONG))
                    .child(
                        canvas(
                            move |bounds, window, _| {
                                let scale = window.scale_factor();
                                let rect = (
                                    (f32::from(bounds.origin.x) * scale) as i32,
                                    (f32::from(bounds.origin.y) * scale) as i32,
                                    (f32::from(bounds.size.width) * scale) as i32,
                                    (f32::from(bounds.size.height) * scale) as i32,
                                );
                                if let Ok(mut slot) = preview_canvas_bounds.lock() {
                                    *slot = Some(rect);
                                }
                            },
                            |_, _, _, _| {},
                        )
                        .size_full(),
                    )
                    .child(
                        div()
                            .absolute()
                            .left_2()
                            .bottom_2()
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .bg(theme::c(theme::OVERLAY))
                            .font_family("Lilex")
                            .text_xs()
                            .text_color(theme::c(if ready {
                                theme::TEXT
                            } else {
                                theme::PLACEHOLDER
                            }))
                            .child(if ready {
                                format!("{}  /  {}", timecode(current), timecode(duration))
                            } else {
                                self.status.clone()
                            }),
                    ),
            )
            .child(
                div()
                    .h(px(48.))
                    .w(px(562.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_2()
                    .rounded_md()
                    .bg(theme::c(theme::RAISED))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(self.icon_button(
                                "back",
                                Icon::SkipBack,
                                "5 s",
                                cx,
                                move |editor| editor.seek((current - 5.0).max(0.0)),
                            ))
                            .child(self.icon_button(
                                "play",
                                if self.playing {
                                    Icon::Pause
                                } else {
                                    Icon::Play
                                },
                                if self.playing { "Pause" } else { "Play" },
                                cx,
                                Self::toggle_play,
                            ))
                            .child(self.icon_button(
                                "forward",
                                Icon::SkipForward,
                                "5 s",
                                cx,
                                move |editor| editor.seek((current + 5.0).min(duration)),
                            )),
                    )
                    .child(
                        div()
                            .font_family("Lilex")
                            .text_sm()
                            .text_color(theme::c(theme::TEXT))
                            .child(timecode(current)),
                    ),
            )
            .into_any_element()
    }

    fn render_audio(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let tracks = self
            .info
            .as_ref()
            .map(|info| {
                info.audio
                    .iter()
                    .enumerate()
                    .map(|(index, track)| {
                        let setting = self
                            .plan
                            .audio
                            .iter()
                            .find(|setting| setting.stream_index == track.stream_index)
                            .cloned()
                            .unwrap_or(AudioSetting {
                                stream_index: track.stream_index,
                                gain_db: 0.0,
                                muted: false,
                            });
                        let identity_icon = if track.name.to_ascii_lowercase().contains("mic") {
                            Icon::Mic
                        } else {
                            Icon::Volume
                        };
                        div()
                            .p_2()
                            .rounded_sm()
                            .border_1()
                            .border_color(theme::c(theme::BORDER_SUBTLE))
                            .bg(theme::c(theme::RAISED))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(icon(identity_icon, theme::c(theme::MUTED)))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex_1()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .whitespace_nowrap()
                                                    .text_ellipsis()
                                                    .child(track.name.clone()),
                                            )
                                            .child(
                                                div()
                                                    .font_family("Lilex")
                                                    .text_xs()
                                                    .text_color(theme::c(theme::MUTED))
                                                    .child(format!(
                                                        "{}  {} Hz  stream {}",
                                                        channel_label(track.channels),
                                                        track.sample_rate,
                                                        track.stream_index
                                                    )),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .mt_2()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .font_family("Lilex")
                                            .text_sm()
                                            .child(format!("{:+.1} dB", setting.gain_db)),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .gap_1()
                                            .child(self.button(
                                                ("gain-minus", index),
                                                "−",
                                                cx,
                                                move |editor| editor.set_gain(index, -1.0),
                                            ))
                                            .child(self.button(
                                                ("gain-plus", index),
                                                "+",
                                                cx,
                                                move |editor| editor.set_gain(index, 1.0),
                                            )),
                                    ),
                            )
                            .child(self.icon_button(
                                ("mute", index),
                                identity_icon,
                                if setting.muted { "Unmute" } else { "Mute" },
                                cx,
                                move |editor| editor.mute(index),
                            ))
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        div()
            .w(px(244.))
            .flex_none()
            .p_3()
            .bg(theme::c(theme::BASE))
            .border_l_1()
            .border_color(theme::c(theme::BORDER))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div().child("Audio inspector").child(
                    div()
                        .mt_1()
                        .text_xs()
                        .text_color(theme::c(theme::MUTED))
                        .child("Source tracks and export mix"),
                ),
            )
            .children(tracks)
            .into_any_element()
    }

    fn render_range_segments(
        &self,
        duration: f64,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let mut elements = Vec::new();
        let mut cursor = 0.0;
        for (index, range) in self.plan.ranges.iter().enumerate() {
            if range.start > cursor {
                elements.push(
                    div()
                        .w(relative(((range.start - cursor) / duration) as f32))
                        .h_full()
                        .flex_none()
                        .bg(theme::c(theme::BASE))
                        .into_any_element(),
                );
            }
            let start = range.start;
            elements.push(
                div()
                    .id(("range", index))
                    .w(relative(((range.end - range.start) / duration) as f32))
                    .h_full()
                    .flex_none()
                    .px_1()
                    .cursor_pointer()
                    .border_l_1()
                    .border_r_1()
                    .border_color(if index == self.selected {
                        theme::c(theme::FOCUS)
                    } else {
                        theme::c(theme::BORDER_STRONG)
                    })
                    .bg(if index == self.selected {
                        theme::c(theme::TOP)
                    } else {
                        theme::c(theme::FLOATING)
                    })
                    .font_family("Lilex")
                    .text_xs()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(format!("{}–{}", timecode(range.start), timecode(range.end)))
                    .on_click(cx.listener(move |editor, _, _, cx| {
                        editor.selected = index;
                        editor.seek(start);
                        cx.notify();
                    }))
                    .into_any_element(),
            );
            cursor = range.end;
        }
        if cursor < duration {
            elements.push(
                div()
                    .w(relative(((duration - cursor) / duration) as f32))
                    .h_full()
                    .flex_none()
                    .bg(theme::c(theme::BASE))
                    .into_any_element(),
            );
        }
        elements
    }

    fn render_waveform_lane(
        &self,
        _index: usize,
        name: String,
        waveform: &[f32],
        playhead: f32,
    ) -> gpui::AnyElement {
        let step = (waveform.len() / 128).max(1);
        let bars = waveform
            .iter()
            .step_by(step)
            .take(128)
            .map(|peak| {
                div()
                    .flex_1()
                    .h(px(2.0 + peak.clamp(0.0, 1.0) * 22.0))
                    .bg(theme::c(theme::MUTED))
            })
            .collect::<Vec<_>>();
        div()
            .h(px(34.))
            .flex_none()
            .flex()
            .items_center()
            .child(
                div()
                    .w(px(112.))
                    .flex_none()
                    .pr_2()
                    .text_xs()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_color(theme::c(theme::MUTED))
                    .child(name),
            )
            .child(
                div()
                    .relative()
                    .h_full()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(1.))
                    .overflow_hidden()
                    .bg(theme::c(theme::OVERLAY))
                    .children(bars)
                    .child(
                        div()
                            .absolute()
                            .left(relative(playhead))
                            .top_0()
                            .bottom_0()
                            .w(px(1.))
                            .bg(theme::c(theme::FOCUS)),
                    ),
            )
            .into_any_element()
    }

    fn render_timeline(
        &self,
        current: f64,
        duration: f64,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let playhead = (current / duration).clamp(0.0, 1.0) as f32;
        let timeline_bounds = Arc::clone(&self.timeline_bounds);
        let filmstrip = if self.visuals.filmstrip.is_empty() {
            vec![
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(theme::c(theme::PLACEHOLDER))
                    .child("Preparing source filmstrip")
                    .into_any_element(),
            ]
        } else {
            self.visuals
                .filmstrip
                .iter()
                .map(|path| {
                    img(PathBuf::from(path))
                        .flex_1()
                        .h_full()
                        .object_fit(ObjectFit::Cover)
                        .into_any_element()
                })
                .collect::<Vec<_>>()
        };
        let waveform_lanes = self
            .info
            .as_ref()
            .map(|info| {
                info.audio
                    .iter()
                    .enumerate()
                    .map(|(index, track)| {
                        self.render_waveform_lane(
                            index,
                            track.name.clone(),
                            &track.waveform,
                            playhead,
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        div()
            .h(px(220.))
            .flex_none()
            .px_3()
            .py_2()
            .bg(theme::c(theme::RAISED))
            .border_t_1()
            .border_color(theme::c(theme::BORDER))
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .h(px(34.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(self.button("in-minus", "In −0.25", cx, |editor| {
                                editor.trim_start(-0.25)
                            }))
                            .child(self.button("in-plus", "In +0.25", cx, |editor| {
                                editor.trim_start(0.25)
                            }))
                            .child(self.button("out-minus", "Out −0.25", cx, |editor| {
                                editor.trim_end(-0.25)
                            }))
                            .child(self.button("out-plus", "Out +0.25", cx, |editor| {
                                editor.trim_end(0.25)
                            }))
                            .child(self.button("set-in", "Set In", cx, Self::set_in_at_playhead))
                            .child(self.button(
                                "set-out",
                                "Set Out",
                                cx,
                                Self::set_out_at_playhead,
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(self.icon_button(
                                "split",
                                Icon::Scissors,
                                "Split",
                                cx,
                                Self::split,
                            ))
                            .child(self.icon_button(
                                "delete",
                                Icon::Trash,
                                "Delete",
                                cx,
                                Self::delete_selected,
                            ))
                            .child(self.icon_button("undo", Icon::Undo, "Undo", cx, Self::undo))
                            .child(self.icon_button("redo", Icon::Redo, "Redo", cx, Self::redo)),
                    ),
            )
            .child(
                div()
                    .h(px(20.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .w(px(112.))
                            .flex_none()
                            .font_family("Lilex")
                            .text_xs()
                            .text_color(theme::c(theme::MUTED))
                            .child(format!("{:.1}s retained", self.plan.duration())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .justify_between()
                            .font_family("Lilex")
                            .text_xs()
                            .text_color(theme::c(theme::MUTED))
                            .child(timecode(0.0))
                            .child(timecode(duration * 0.25))
                            .child(timecode(duration * 0.5))
                            .child(timecode(duration * 0.75))
                            .child(timecode(duration)),
                    ),
            )
            .child(
                div()
                    .h(px(42.))
                    .flex_none()
                    .flex()
                    .child(
                        div()
                            .w(px(112.))
                            .flex_none()
                            .pr_2()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_xs()
                            .text_color(theme::c(theme::MUTED))
                            .child(icon(Icon::Filmstrip, theme::c(theme::MUTED)))
                            .child("Source"),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .rounded_sm()
                            .bg(theme::c(theme::OVERLAY))
                            .child(
                                canvas(
                                    move |bounds, _, _| {
                                        if let Ok(mut slot) = timeline_bounds.lock() {
                                            *slot = Some((
                                                f32::from(bounds.origin.x),
                                                f32::from(bounds.size.width),
                                                f32::from(bounds.origin.y),
                                            ));
                                        }
                                    },
                                    |_, _, _, _| {},
                                )
                                .size_full(),
                            )
                            .child(div().absolute().size_full().flex().children(filmstrip))
                            .child(
                                div()
                                    .absolute()
                                    .left(relative(playhead))
                                    .top_0()
                                    .bottom_0()
                                    .w(px(1.))
                                    .bg(theme::c(theme::FOCUS)),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |editor, event: &gpui::MouseDownEvent, _, cx| {
                                    editor.scrub_from_x(f32::from(event.position.x));
                                    cx.notify();
                                }),
                            )
                            .on_mouse_move(cx.listener(
                                move |editor, event: &MouseMoveEvent, _, cx| {
                                    if event.dragging() {
                                        editor.scrub_from_x(f32::from(event.position.x));
                                        cx.notify();
                                    }
                                },
                            )),
                    ),
            )
            .child(
                div()
                    .h(px(22.))
                    .flex_none()
                    .flex()
                    .child(
                        div()
                            .w(px(112.))
                            .flex_none()
                            .text_xs()
                            .child("Retained ranges"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .overflow_hidden()
                            .rounded_sm()
                            .children(self.render_range_segments(duration, cx)),
                    ),
            )
            .children(waveform_lanes)
            .into_any_element()
    }
}

impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let current = self
            .preview
            .as_ref()
            .and_then(|preview| preview.position().ok())
            .unwrap_or(0.0);
        let duration = self
            .info
            .as_ref()
            .map_or(1.0, |info| info.duration.max(0.01));
        let title = self.info.as_ref().map_or_else(
            || "Choose a recording".into(),
            |info| clip_title(&info.path),
        );
        let selected_path = self.info.as_ref().map(|info| info.path.as_str());
        div()
            .size_full()
            .font_family("Outfit")
            .bg(theme::c(theme::WINDOW))
            .text_color(theme::c(theme::TEXT))
            .flex()
            .flex_col()
            .child(self.render_header(title, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.render_library(selected_path, cx))
                    .child(self.render_preview(current, duration, cx))
                    .child(self.render_audio(cx)),
            )
            .child(self.render_timeline(current, duration, cx))
            .child(
                div()
                    .h(px(24.))
                    .flex_none()
                    .px_3()
                    .flex()
                    .items_center()
                    .font_family("Lilex")
                    .text_xs()
                    .bg(theme::c(theme::BASE))
                    .text_color(
                        if self.status.to_ascii_lowercase().contains("error")
                            || self.status.to_ascii_lowercase().contains("failed")
                        {
                            theme::c(theme::ERROR)
                        } else {
                            theme::c(theme::MUTED)
                        },
                    )
                    .child(self.status.clone()),
            )
    }
}

fn parent_hwnd(window: &Window) -> Result<isize, String> {
    let h = HasWindowHandle::window_handle(window).map_err(|e| e.to_string())?;
    match h.as_raw() {
        RawWindowHandle::Win32(raw) => Ok(raw.hwnd.get() as isize),
        _ => Err("Recall native preview requires a Win32 window".into()),
    }
}
