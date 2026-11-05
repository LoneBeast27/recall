//! The single top-level view: routes between Library, Editor, HDR, Audio and Settings,
//! owns services and workers, and runs the 30 ms poll loop.
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{
    Bounds, Context, FocusHandle, Render, Window, canvas, div, fill, point,
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

/// The Display & HDR page belongs to the shelved experimental HDR feature (2026-11-04):
/// it is hidden unless the app starts with `RECALL_EXPERIMENTAL_HDR=1`, the variable that
/// also re-enables HDR switching in the recorder (`recall-hdr/src/hdr_gate.rs`).
pub fn hdr_page_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| hdr_flag(std::env::var("RECALL_EXPERIMENTAL_HDR").ok().as_deref()))
}

fn hdr_flag(value: Option<&str>) -> bool {
    matches!(value.map(str::trim), Some("1" | "true" | "on" | "yes"))
}

/// Where a request for `route` lands: the shelved HDR page falls back to the library.
pub fn reachable(route: Route, hdr_enabled: bool) -> Route {
    if route == Route::Hdr && !hdr_enabled { Route::Library } else { route }
}

impl Route {
    pub fn name(self) -> &'static str {
        match self {
            Route::Library => "library",
            Route::Editor => "editor",
            Route::Hdr => "hdr",
            Route::Audio => "audio",
            Route::Settings => "settings",
        }
    }
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
    /// Right-click menu on a library clip tile.
    pub tile_menu: Option<crate::views::tile_menu::TileMenu>,
    /// Displayed meter levels with ballistics: (rms dB, peak-hold dB, peak time).
    pub meter_display: Vec<(f32, f32, Instant)>,
    /// Tile press that may become a drag-out: (path, window x, y).
    pub drag_origin: Option<(String, f32, f32)>,
    /// The rename field was on screen in the last frame.
    pub rename_painted: Rc<Cell<bool>>,
    /// This press ended a rename or closed a menu: it does not also act on the tile under it.
    pub press_dismisses: bool,
    /// The sort menu was open at this press (its button then closes it, not reopens it).
    pub press_saw_sort: bool,
    /// The last click opened a clip from its tile.
    pub opened_by_click: bool,
    /// Frame rate before the fps field took the keyboard (Escape goes back to it).
    pub fps_before: u32,
    /// Settings → Storage state (cache size slider, usage).
    pub cache: crate::views::storage_settings::CacheView,
    pub settings_tab: crate::views::settings::SettingsTab,
    /// Settings device lists (loaded on first view) and open menus.
    pub settings_ui: crate::views::settings_audio::SettingsUi,
    /// Where a press on the title bar started (a drag begins once the pointer moves).
    pub title_drag: Option<gpui::Point<gpui::Pixels>>,
    /// Recordings folder (last reported) + imported folders of old clips.
    pub folders: crate::services::library::folders::Folders,
    /// When to check whether the recorder is running and start it (once per launch).
    controller_check_at: Option<Instant>,
    pub frame_at: Instant,
    /// The top-level window, for the background check (`root_poll.rs`).
    hwnd: Option<isize>,
    /// A recorder status change arrived in the background and is not drawn yet.
    status_stale: bool,
    /// The startup timeline marks the first render once.
    first_render_marked: bool,
}

#[path = "root_events.rs"]
mod events;
#[path = "root_exits.rs"]
mod exits;
#[path = "root_keys.rs"]
mod keys;
#[path = "root_poll.rs"]
mod poll;

impl Root {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        use crate::startup::span;
        span("root: ole + drop target", || {
            crate::shell::init_ole();
            if let Ok(hwnd) = parent_hwnd(window) {
                crate::shell::refuse_drops(hwnd);
            }
        });
        let preview = span("root: native preview", || parent_hwnd(window).and_then(NativePreview::new));
        let workers = span("root: workers", Workers::spawn);
        workers.job(Job::List);
        span("root: sweep_kept", || crate::services::storage::sweep_kept(crate::backend::library_dir));
        span("root: pre_encode clear_dir", crate::services::pre_encode::clear_dir);
        span("root: pre_encode sweep_deliveries", crate::services::pre_encode::sweep_deliveries);
        let daemon = span("root: daemon client", DaemonClient::spawn);
        let folders = span("root: folders load", crate::services::library::folders::Folders::load);
        let mut root = Self {
            route: Route::Library,
            focus: cx.focus_handle(),
            workers,
            daemon,
            daemon_rev: u64::MAX,
            snap: DaemonSnapshot::default(),
            lib: LibraryState::new(),
            ed: EditorState::new(preview),
            search: TextField::new(cx, "Search clips", 80),
            rename: TextField::new(cx, "Clip title", 80),
            fps_field: TextField::new(cx, "fps", 7).digits_only(),
            fps_custom: false,
            meters: None,
            midi: None,
            midi_result: None,
            ac_profile: None,
            toast: None,
            sort_open: false,
            corner_open: false,
            tile_menu: None,
            meter_display: Vec::new(),
            drag_origin: None,
            rename_painted: Rc::new(Cell::new(false)),
            press_dismisses: false,
            press_saw_sort: false,
            opened_by_click: false,
            fps_before: 60,
            cache: crate::views::storage_settings::CacheView::new(),
            settings_tab: Default::default(),
            settings_ui: Default::default(),
            title_drag: None,
            folders,
            controller_check_at: std::env::var_os("RECALL_NO_CONTROLLER").is_none().then(|| Instant::now() + Duration::from_millis(1500)),
            frame_at: Instant::now(),
            hwnd: parent_hwnd(window).ok(),
            status_stale: false,
            first_render_marked: false,
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
        crate::views::window_frame::solid_backdrop(window, theme::WINDOW);
        window.focus(&root.focus);
        root
    }

    /// The polling task holds this view for a tick after the window goes, and the app can quit
    /// first, so `drop` alone may never run.
    pub fn flush_on_close(&mut self) {
        self.ed.flush_now();
        crate::services::pre_encode::discard();
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        let mut changed = self.back_in_front();
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
            crate::services::storage::set_recorder_connected(self.snap.connected);
            self.sync_recordings_folder();
            for event in self.daemon.drain_events() {
                self.on_daemon_event(event);
            }
            changed |= self.status_changed();
        }
        changed |= self.ed.tick(self.route == Route::Editor, &self.workers);
        crate::trace::record(self.route.name(), &self.ed, self.lib.records.len());
        crate::trace::dump_layout(self.route.name(), self.lib.records.len());
        if let Some(error) = self.ed.last_error.take() {
            self.show_toast(ToastKind::Bad, "Preview", Some(error));
            changed = true;
        }
        changed |= self.lib.fit_columns();
        if self.route == Route::Library {
            self.lib.queue_visible(&self.workers);
            self.lib.note_scroll_depth();
        }
        // A thumbnail rewritten larger in place: drop gpui's decode so the tile reloads the file.
        for thumb in self.lib.refreshed_images.drain(..) {
            gpui::ImageSource::from(std::path::PathBuf::from(thumb)).remove_asset(cx);
            changed = true;
        }
        self.lib.flush_meta_if_due();
        self.lib.ssd.poll(&self.workers, !self.lib.loading);
        if let Some(notice) = self.lib.notice.take() {
            self.show_toast(ToastKind::Bad, notice, None);
            changed = true;
        }
        // The tile with the rename field left the screen: draw again so `settle_focus` sees it.
        changed |= self.lib.renaming.is_some() && !self.rename_painted.get();
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
            Done::Clips(Ok(listing)) => {
                crate::startup::mark(&format!("library listed ({} clips)", listing.entries.len()));
                self.lib.set_entries(listing)
            }
            Done::Drive(drive) => self.lib.drive = drive,
            Done::Ssd(usage) => self.lib.ssd.set(usage),
            Done::Streaming(generation, clip) => self.ed.apply_streaming(generation, clip),
            Done::OpenStep(generation, text) => self.ed.open_step(generation, text),
            Done::Library(change) => self.on_library_change(change),
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
                match &result {
                    Err(error) if error == crate::backend::EXPORT_CANCELLED => self.show_toast(ToastKind::Info, "Export cancelled", None),
                    Err(error) => self.show_toast(ToastKind::Bad, "Export failed", Some(error.clone())),
                    Ok(_) => self.show_toast(ToastKind::Good, "Export finished", None),
                }
                self.ed.on_exported(result);
            }
            Done::Deleted(paths, Ok(count)) => {
                self.lib.deleting = 0;
                self.lib.forget(&paths);
                let noun = if count == 1 { "clip" } else { "clips" };
                self.show_toast(ToastKind::Info, format!("Moved {count} {noun} to the Recycle Bin"), None);
            }
            Done::Deleted(_, Err(error)) => {
                self.lib.deleting = 0;
                self.show_toast(ToastKind::Bad, "Delete failed", Some(error));
            }
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
            Done::Proxy(clip, result) => self.ed.on_proxy(&clip, result),
            Done::Flushed(Ok(crate::backend::Flushed::Current(sidecar))) => self.lib.mark_edited_by_sidecar(&sidecar, true),
            Done::Flushed(Ok(crate::backend::Flushed::Removed(sidecar))) => self.lib.mark_edited_by_sidecar(&sidecar, false),
            Done::Flushed(Ok(crate::backend::Flushed::Nothing)) => {}
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
        let route = reachable(route, hdr_page_enabled());
        self.lib.finish_rename(&self.rename.value, crate::library_state::RenameEnd::Away);
        self.lib.confirm_delete = false;
        self.lib.set_hover(None, &self.workers);
        self.ed.end_drag();
        if self.route == Route::Editor && route != Route::Editor {
            self.ed.flush(&self.workers);
            self.ed.pause();
            self.ed.hide_preview();
            self.ed.drop_pre_encode();
        }
        if self.route == Route::Editor && route == Route::Library {
            // Back on the clip just closed, wherever the grid was scrolled before.
            if let Some(path) = self.ed.info.as_ref().map(|i| i.path.clone()).or_else(|| self.ed.loading_path.clone()) {
                self.lib.reveal(&path);
            }
        }
        // The level meters and the MIDI monitor run only while the Audio page shows.
        if route == Route::Audio {
            self.sync_audio_monitors();
        } else {
            self.meters = None;
            self.midi = None;
        }
        if route == Route::Settings {
            self.cache.refresh();
        }
        if matches!(route, Route::Settings | Route::Hdr | Route::Audio) {
            self.daemon.send(DaemonCommand::GetConfig);
            self.daemon.send(DaemonCommand::Status);
        }
        self.sort_open = false;
        self.corner_open = false;
        self.tile_menu = None;
        self.route = route;
    }

    pub fn open_clip(&mut self, path: String) {
        crate::services::storage::record_interaction(crate::services::storage::Interaction::Opened(path.clone()));
        self.lib.mark_played(&path);
        if !self.ed.is_open(&path) {
            self.ed.open(path, &self.workers);
        }
        self.fps_custom = !crate::editor_state::FPS_PRESETS.contains(&self.ed.fps);
        self.go(Route::Editor);
    }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.first_render_marked {
            self.first_render_marked = true;
            crate::startup::mark("first render");
        }
        self.settle_focus(window);
        self.trace_ui(window);
        crate::trace::clear_targets(window);
        // Meters and playback animate at the display refresh rate, not the 30 ms poll.
        if self.route == Route::Audio || (self.route == Route::Editor && (self.ed.playing || (!self.ed.loading.grey.is_empty() && self.ed.loading.failed.is_none()))) {
            window.request_animation_frame();
        }
        // Right after a frame lays out the preview at a new size (resize, maximize), move the
        // native video there and repaint, so the cut-out never waits for the 30 ms poll.
        if self.route == Route::Editor {
            let root = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = root.update(cx, |this, cx| {
                    if this.ed.apply_preview_rect(true) {
                        cx.notify();
                    }
                });
            });
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
        // Focus tracking stays off the title bar: a focus-tracked element marks every mouse-down
        // under it as handled, and GPUI's Windows caption code then skips minimize / maximize /
        // close, title-bar drag and double-click maximize.
        let body = div()
            .id("body")
            .key_context("Recall")
            .track_focus(&self.focus)
            .capture_any_mouse_down(cx.listener(Self::on_press))
            .on_key_down(cx.listener(Self::on_key))
            .flex_1()
            .min_h_0()
            .flex()
            .child(body);
        root.id("root")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .child(backdrop)
            .child(Self::drag_watch(cx))
            .font_family(theme::sans())
            .text_size(px(13.))
            .line_height(px(18.))
            .text_color(c(theme::TEXT))
            .child(self.render_titlebar(window, cx))
            .child(body)
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
        self.flush_on_close();
    }
}

#[cfg(test)]
mod hdr_shelf_tests {
    use super::{Route, hdr_flag, reachable};

    #[test]
    fn hdr_page_is_hidden_unless_explicitly_enabled() {
        assert!(!hdr_flag(None) && !hdr_flag(Some("0")) && !hdr_flag(Some("")));
        assert!(hdr_flag(Some("1")));
        assert_eq!(reachable(Route::Hdr, false), Route::Library, "shelved HDR page falls back to the library");
        assert_eq!(reachable(Route::Hdr, true), Route::Hdr);
        assert_eq!(reachable(Route::Settings, false), Route::Settings);
    }
}
