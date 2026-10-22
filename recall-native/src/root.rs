//! The single top-level view: routes between Library, Editor, HDR, Audio and Settings,
//! owns services and workers, and runs the 30 ms poll loop.
use std::time::{Duration, Instant};

use gpui::{
    Bounds, Context, Entity, FocusHandle, Render, Timer, Window, canvas, div, fill, point,
    prelude::*, px, size,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::{
    editor_state::EditorState,
    library_state::LibraryState,
    media::NativePreview,
    services::{
        daemon::{DaemonClient, DaemonCommand, DaemonSnapshot},
        meters::Meters,
        midi::Midi,
    },
    theme::{self, c},
    ui::TextField,
    worker::{Done, Job, Workers},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Library,
    Editor,
    Hdr,
    Audio,
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Good,
    Info,
    Bad,
}

pub struct Toast {
    pub text: String,
    pub detail: Option<String>,
    pub kind: ToastKind,
    pub until: Instant,
}

/// Which text field currently receives keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Search,
    Rename,
    Fps,
}

pub struct Root {
    pub route: Route,
    pub focus: FocusHandle,
    pub workers: Workers,
    pub daemon: DaemonClient,
    daemon_rev: u64,
    pub snap: DaemonSnapshot,
    pub lib: LibraryState,
    pub ed: EditorState,
    pub search: TextField,
    pub rename: TextField,
    pub fps_field: TextField,
    pub fps_custom: bool,
    pub meters: Option<Meters>,
    pub midi: Option<Midi>,
    pub midi_result: Option<Result<String, String>>,
    pub ac_profile: Option<String>,
    pub toast: Option<Toast>,
    pub sort_open: bool,
    pub corner_open: bool,
    /// Displayed meter levels with ballistics: (rms dB, peak-hold dB, peak time).
    pub meter_display: Vec<(f32, f32, Instant)>,
    /// Tile press that may become a drag-out: (path, window x, y).
    pub drag_origin: Option<(String, f32, f32)>,
    /// Settings → Storage state (cache size slider, usage).
    pub cache: crate::views::storage_settings::CacheView,
    pub settings_tab: crate::views::settings::SettingsTab,
    /// Recordings folder (last reported) + imported folders of old clips.
    pub folders: crate::services::library::folders::Folders,
    /// When to check whether the recorder is running and start it (once per launch).
    controller_check_at: Option<Instant>,
    pub frame_at: Instant,
}

#[path = "root_events.rs"]
mod events;
#[path = "root_keys.rs"]
mod keys;

impl Root {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        crate::shell::init_ole();
        let preview = parent_hwnd(window).and_then(NativePreview::new);
        let workers = Workers::spawn();
        workers.job(Job::List);
        let mut root = Self {
            route: Route::Library,
            focus: cx.focus_handle(),
            workers,
            daemon: DaemonClient::spawn(),
            daemon_rev: u64::MAX,
            snap: DaemonSnapshot::default(),
            lib: LibraryState::new(),
            ed: EditorState::new(preview),
            search: TextField::new(cx, "Search clips", 80),
            rename: TextField::new(cx, "Clip title", 80),
            fps_field: TextField::new(cx, "fps", 7),
            fps_custom: false,
            meters: None,
            midi: None,
            midi_result: None,
            ac_profile: None,
            toast: None,
            sort_open: false,
            corner_open: false,
            meter_display: Vec::new(),
            drag_origin: None,
            cache: crate::views::storage_settings::CacheView::new(),
            settings_tab: Default::default(),
            folders: crate::services::library::folders::Folders::load(),
            controller_check_at: std::env::var_os("RECALL_NO_CONTROLLER").is_none().then(|| Instant::now() + Duration::from_millis(1500)),
            frame_at: Instant::now(),
        };
        if let Some(path) = std::env::var_os("RECALL_NATIVE_CLIP") {
            root.open_clip(path.to_string_lossy().into_owned());
        }
        // Layout checks: RECALL_NATIVE_ROUTE=hdr|audio|settings opens that page.
        match std::env::var("RECALL_NATIVE_ROUTE").as_deref() {
            Ok("hdr") => root.go(Route::Hdr),
            Ok("audio") => root.go(Route::Audio),
            Ok("settings") => root.go(Route::Settings),
            Ok("settings-clip") | Ok("settings-media") | Ok("settings-shortcuts") => {
                use crate::views::settings::SettingsTab;
                root.settings_tab = match std::env::var("RECALL_NATIVE_ROUTE").as_deref() {
                    Ok("settings-clip") => SettingsTab::Clip,
                    Ok("settings-media") => SettingsTab::Media,
                    _ => SettingsTab::Shortcuts,
                };
                root.go(Route::Settings);
            }
            _ => {}
        }
        window.focus(&root.focus);
        root
    }

    pub fn start_polling(entity: Entity<Self>, window: &mut Window, cx: &mut gpui::App) {
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
        while let Ok(done) = self.workers.done.try_recv() {
            changed = true;
            self.on_done(done);
        }
        if self.daemon.revision() != self.daemon_rev {
            self.daemon_rev = self.daemon.revision();
            self.snap = self.daemon.snapshot();
            // Background cache filling pauses while a game is being recorded.
            let busy = self.snap.session.as_ref().is_some_and(|s| s.state == "recording" || s.state == "starting");
            crate::services::storage::set_recorder_busy(busy);
            self.sync_recordings_folder();
            for event in self.daemon.drain_events() {
                self.on_daemon_event(event);
            }
            changed = true;
        }
        changed |= self.ed.tick(self.route == Route::Editor, &self.workers);
        if let Some(error) = self.ed.last_error.take() {
            self.show_toast(ToastKind::Bad, "Preview", Some(error));
            changed = true;
        }
        changed |= self.lib.fit_columns();
        if self.route == Route::Library {
            self.lib.queue_visible(&self.workers);
            self.lib.note_scroll_depth();
        }
        self.lib.flush_meta_if_due();
        // Opening Recall means recording: start the recorder once if it isn't running.
        if self.controller_check_at.is_some_and(|at| Instant::now() >= at) {
            self.controller_check_at = None;
            if !self.snap.connected {
                if let Err(error) = crate::services::daemon::start_controller() {
                    self.show_toast(ToastKind::Bad, "Couldn't start the recorder", Some(error));
                    changed = true;
                }
            }
        }
        if self.toast.as_ref().is_some_and(|t| Instant::now() >= t.until) {
            self.toast = None;
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    fn on_done(&mut self, done: Done) {
        match done {
            Done::Clips(Ok(entries)) => self.lib.set_entries(entries),
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
                if let Err(error) = &result {
                    self.show_toast(ToastKind::Bad, "Export failed", Some(error.clone()));
                } else {
                    self.show_toast(ToastKind::Good, "Export finished", None);
                }
                self.ed.on_exported(result);
            }
            Done::Deleted(Ok(count)) => {
                let paths = self.lib.selected_paths();
                self.lib.forget(&paths);
                let noun = if count == 1 { "clip" } else { "clips" };
                self.show_toast(ToastKind::Info, format!("Moved {count} {noun} to the Recycle Bin"), None);
            }
            Done::Deleted(Err(error)) => self.show_toast(ToastKind::Bad, "Delete failed", Some(error)),
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
            Done::Flushed(Ok(sidecar)) => {
                if !sidecar.is_empty() {
                    self.lib.mark_edited_by_sidecar(&sidecar);
                }
            }
            Done::Flushed(Err(error)) => self.show_toast(ToastKind::Bad, "Couldn't save edits beside the clip", Some(error)),
            Done::Storage(report) => {
                if let Some(notice) = report.notice {
                    self.show_toast(ToastKind::Info, notice, None);
                }
                self.cache.refresh();
            }
        }
    }

    pub fn show_toast(&mut self, kind: ToastKind, text: impl Into<String>, detail: Option<String>) {
        let secs = if kind == ToastKind::Bad { 6 } else { 3 };
        self.toast = Some(Toast { text: text.into(), detail, kind, until: Instant::now() + Duration::from_secs(secs) });
    }

    pub fn send(&mut self, command: DaemonCommand) {
        if !self.daemon.send(command) {
            self.show_toast(ToastKind::Bad, "Recorder is off", Some("Start it from the status pill".into()));
        }
    }

    pub fn go(&mut self, route: Route) {
        if self.route == Route::Editor && route != Route::Editor {
            self.ed.flush(&self.workers);
            self.ed.pause();
            self.ed.hide_preview();
        }
        if route == Route::Audio {
            self.meters.get_or_insert_with(Meters::spawn);
            self.midi.get_or_insert_with(Midi::spawn);
        }
        if route == Route::Settings {
            self.cache.refresh();
        }
        if route == Route::Settings || route == Route::Hdr {
            self.daemon.send(DaemonCommand::GetConfig);
            self.daemon.send(DaemonCommand::Status);
        }
        self.sort_open = false;
        self.corner_open = false;
        self.route = route;
    }

    pub fn open_clip(&mut self, path: String) {
        crate::services::storage::record_interaction(crate::services::storage::Interaction::Opened(path.clone()));
        if !self.ed.is_open(&path) {
            self.ed.open(path, &self.workers);
        }
        self.fps_custom = !crate::editor_state::FPS_PRESETS.contains(&self.ed.fps);
        self.go(Route::Editor);
    }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Meters and playback animate at the display refresh rate, not the 30 ms poll.
        if self.route == Route::Audio || (self.route == Route::Editor && self.ed.playing) {
            window.request_animation_frame();
        }
        let body = match self.route {
            Route::Editor => self.render_editor(window, cx),
            _ => self.render_shell(window, cx),
        };
        let hole = self.ed.hole.clone();
        let backdrop = canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let color = c(theme::WINDOW);
                let Some((x, y, w, h)) = hole.get() else {
                    window.paint_quad(fill(bounds, color));
                    return;
                };
                // Paint around the native video window so it shows through.
                let s = window.scale_factor();
                let (hx, hy, hw, hh) = (x as f32 / s, y as f32 / s, w as f32 / s, h as f32 / s);
                let (bx, by) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
                let (bw, bh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                let rect = |x0: f32, y0: f32, x1: f32, y1: f32| {
                    Bounds::new(point(px(x0), px(y0)), size(px((x1 - x0).max(0.0)), px((y1 - y0).max(0.0))))
                };
                window.paint_quad(fill(rect(bx, by, bx + bw, hy), color));
                window.paint_quad(fill(rect(bx, hy + hh, bx + bw, by + bh), color));
                window.paint_quad(fill(rect(bx, hy, hx, hy + hh), color));
                window.paint_quad(fill(rect(hx + hw, hy, bx + bw, hy + hh), color));
            },
        )
        .absolute()
        .inset_0();
        let mut root = div();
        // Tabular figures everywhere so timecodes, sizes and counters don't jitter.
        root.text_style().get_or_insert_with(Default::default).font_features =
            Some(gpui::FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)])));
        root
            .id("root")
            .key_context("Recall")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .child(backdrop)
            .font_family(theme::sans())
            .text_size(px(13.))
            .line_height(px(18.))
            .text_color(c(theme::TEXT))
            .child(self.render_titlebar(window, cx))
            .child(div().flex_1().min_h_0().flex().child(body))
            .children(self.render_toast())
    }
}

pub fn parent_hwnd(window: &Window) -> Result<isize, String> {
    let handle = HasWindowHandle::window_handle(window).map_err(|e| e.to_string())?;
    match handle.as_raw() {
        RawWindowHandle::Win32(raw) => Ok(raw.hwnd.get() as isize),
        _ => Err("Recall needs a Win32 window".into()),
    }
}

impl Drop for Root {
    /// Closing the window writes pending edits beside the clip (the worker may not outlive it).
    fn drop(&mut self) {
        self.ed.flush_now();
    }
}
