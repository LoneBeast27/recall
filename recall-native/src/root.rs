//! The single top-level view: routes between Library, Editor, HDR, Audio and Settings,
//! owns services and workers, and runs the 30 ms poll loop.
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{FocusHandle, Window};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::{
    editor_state::EditorState,
    library_state::LibraryState,
    services::{
        daemon::{DaemonClient, DaemonCommand, DaemonSnapshot},
        meters::Meters,
        midi::Midi,
    },
    ui::TextField,
    worker::Workers,
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
    CacheSize,
}

pub struct Root {
    pub route: Route,
    pub(crate) setup: Option<crate::views::setup_runner::SetupRunner>,
    setup_resume_pending: bool,
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
    /// Settings → Storage: the typed SSD cache size in GB.
    pub cache_field: TextField,
    pub fps_custom: bool,
    pub meters: Option<Meters>,
    pub midi: Option<Midi>,
    pub midi_result: Option<Result<String, String>>,
    /// A test note is being sent: a second click waits for it (`audio_midi.rs`).
    pub midi_sending: bool,
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
    /// A Change-button folder change waiting for the recorder to report it (`folders_sync`).
    pub pending_folder: Option<crate::views::folders_sync::PendingFolder>,
    /// When to check whether the recorder is running and start it (once per launch).
    controller_check_at: Option<Instant>,
    pub frame_at: Instant,
    /// The top-level window, for the background check (`root_poll.rs`).
    hwnd: Option<isize>,
    /// A recorder status change arrived in the background and is not drawn yet.
    status_stale: bool,
    /// The pill's clock (`root_poll::recorder_clock_tick`): no status replies, drain seconds drawn.
    pub(crate) recorder_unresponsive: bool,
    /// Disconnected: is a recorder running that never answers (`root_reach`)?
    pub(crate) recorder_reach: crate::services::daemon::RecorderReach,
    reach_since: Option<Instant>,
    reach_checked: Option<Instant>,
    drain_drawn: Option<u64>,
    /// The startup timeline marks the first render once.
    first_render_marked: bool,
    /// Settings → Storage limit: a picked limit waiting for the recorder's dry run.
    pub storage_asking: Option<(u32, Instant)>,
    /// That dry run would move clips now: the confirm bar under the limit row.
    pub storage_confirm: Option<crate::views::storage_limit::Preview>,
    pub update: crate::update::UpdateUi,
    /// Settings → "Stop now (lose the clip)" was clicked: its confirm shows.
    pub stop_confirm: bool,
}

#[path = "root_events.rs"]
mod events;
#[path = "root_exits.rs"]
mod exits;
#[path = "root_keys.rs"]
mod keys;
#[path = "root_poll.rs"]
mod poll;
#[path = "root_reach.rs"]
pub(crate) mod reach;
#[path = "root_render.rs"]
mod render;
#[path = "root_start.rs"]
mod start;
#[path = "root_done.rs"]
mod done;

impl Root {
    /// The polling task holds this view for a tick after the window goes, and the app can quit
    /// first, so `drop` alone may never run.
    pub fn flush_on_close(&mut self) {
        if crate::TESTING {
            return;
        }
        // Queued behind the edit writes already sent; app.rs waits for them before exiting.
        self.ed.flush_on_close(|job| self.workers.job(job));
        // A fader still held is saved, as a release would.
        self.commit_dragged_fader();
        // A cache size still being typed is applied, as a click elsewhere would.
        if self.cache.editing {
            self.apply_typed_cache();
        }
        crate::services::pre_encode::discard();
        crate::services::library::open_clips::publish(None);
    }

    pub fn show_toast(&mut self, kind: ToastKind, text: impl Into<String>, detail: Option<String>) {
        let secs = if kind == ToastKind::Bad { 6 } else { 3 };
        self.show_toast_for(kind, text, detail, secs);
    }

    pub fn show_toast_for(&mut self, kind: ToastKind, text: impl Into<String>, detail: Option<String>, secs: u64) {
        self.toast = Some(Toast { text: text.into(), detail, kind, until: Instant::now() + Duration::from_secs(secs) });
    }

    pub fn send(&mut self, command: DaemonCommand) {
        if !self.daemon.send(command) {
            self.show_toast(ToastKind::Bad, "Recorder is off", Some("Start it from the status pill".into()));
        } else if crate::TESTING {
            self.snap = self.daemon.snapshot();
        }
    }

    pub fn go(&mut self, route: Route) {
        if crate::TESTING {
            if route != Route::Audio {
                self.commit_dragged_fader();
                self.settings_ui.audio.closed();
            }
            self.route = route;
            return;
        }
        let route = reachable(route, hdr_page_enabled());
        self.lib.finish_rename(&self.rename.value, crate::library_state::RenameEnd::Away);
        self.lib.confirm_delete = false;
        // A storage-limit question belongs to the page it was asked on.
        if route != Route::Settings {
            self.storage_asking = None;
            self.storage_confirm = None;
        }
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
            // A fader still held when the page goes is saved, as a release would.
            self.commit_dragged_fader();
            self.settings_ui.audio.closed();
            self.midi = None;
        }
        if route == Route::Settings {
            self.cache.refresh();
            // A MIDI device plugged in since Settings last showed is listed again.
            if self.route != Route::Settings {
                self.settings_ui.devices.stale();
            }
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
        if crate::TESTING {
            return;
        }
        crate::services::storage::record_interaction(crate::services::storage::Interaction::Opened(path.clone()));
        self.lib.mark_played(&path);
        // The recorder's storage cap never recycles the clip open here.
        crate::services::library::open_clips::publish(Some(&path));
        if !self.ed.is_open(&path) {
            self.ed.open(path, &self.workers);
        }
        self.fps_custom = !crate::editor_state::FPS_PRESETS.contains(&self.ed.fps);
        self.go(Route::Editor);
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
