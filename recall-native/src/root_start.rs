//! `Root::new`: start-up order (the cache guard before any worker or sweep), the first state of
//! every field, and the layout-check routes.
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{Context, Window};

use super::{Root, Route, ToastKind, parent_hwnd};
use crate::{
    editor_state::EditorState,
    library_state::LibraryState,
    media::NativePreview,
    services::{
        daemon::{DaemonClient, DaemonSnapshot},
        library::folders::Folders,
    },
    theme,
    ui::TextField,
    worker::{Job, Workers},
};

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
        let folders = span("root: folders load", crate::services::library::folders::Folders::load);
        // Edit saves place files by the UI's list, not by a fresh read per save.
        crate::backend::share_folders(&folders);
        // Before any worker or sweep: a clip folder inside the cache (or the cache inside one) blocks it.
        let cache_overlap = span("root: cache guard", || crate::services::library::folders::check_cache_overlap(&folders));
        let workers = span("root: workers", Workers::spawn);
        workers.job(Job::List);
        span("root: sweep_kept", || crate::services::storage::sweep_kept(crate::backend::library_dir));
        // Nothing is open yet: a list a crash left behind would keep its clip from the storage cap.
        crate::services::library::open_clips::publish(None);
        span("root: pre_encode clear_dir", crate::services::pre_encode::clear_dir);
        span("root: pre_encode sweep_deliveries", crate::services::pre_encode::sweep_deliveries);
        let daemon = span("root: daemon client", DaemonClient::spawn);
        let mut root = Self::initial(window, cx, workers, daemon, folders, preview);
        if let Some(message) = cache_overlap {
            root.show_toast_for(ToastKind::Bad, "Cache and clip folders overlap", Some(message), 30);
        }
        if let Some(detail) = root.folders.damaged.take() {
            root.show_toast_for(ToastKind::Bad, "Your clip folder list was damaged", Some(detail), 30);
        }
        if let Some(path) = std::env::var_os("RECALL_NATIVE_CLIP") {
            root.open_clip(path.to_string_lossy().into_owned());
        }
        root.open_env_route();
        // Alt+Tab mid-drag: the release goes to another window and no later move may come.
        // Save the dragged gain now, as a release would; a move after refocus then finds no drag.
        cx.observe_window_activation(window, |this, window, cx| {
            if !window.is_window_active() && this.settings_ui.audio.fader.is_some() {
                this.commit_dragged_fader();
                cx.notify();
            }
        })
        .detach();
        crate::views::window_frame::solid_backdrop(window, theme::WINDOW);
        window.focus(&root.focus);
        root
    }

    /// Every field as the window opens.
    fn initial(window: &mut Window, cx: &mut Context<Self>, workers: Workers, daemon: DaemonClient, folders: Folders, preview: Result<NativePreview, String>) -> Self {
        Self {
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
            cache_field: TextField::new(cx, "GB", 6).decimal(),
            fps_custom: false,
            meters: None,
            midi: None,
            midi_result: None,
            midi_sending: false,
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
            pending_folder: None,
            controller_check_at: std::env::var_os("RECALL_NO_CONTROLLER").is_none().then(|| Instant::now() + Duration::from_millis(1500)),
            frame_at: Instant::now(),
            hwnd: parent_hwnd(window).ok(),
            status_stale: false,
            recorder_unresponsive: false,
            recorder_reach: Default::default(),
            reach_since: None,
            reach_checked: None,
            drain_drawn: None,
            first_render_marked: false,
            storage_asking: None,
            storage_confirm: None,
            stop_confirm: false,
        }
    }

    /// Layout checks: RECALL_NATIVE_ROUTE=hdr|audio|settings[-clip|-media|-games|-shortcuts] opens that page.
    fn open_env_route(&mut self) {
        match std::env::var("RECALL_NATIVE_ROUTE").as_deref() {
            Ok("hdr") => self.go(Route::Hdr),
            Ok("audio") => self.go(Route::Audio),
            Ok("settings") => self.go(Route::Settings),
            Ok("settings-clip") | Ok("settings-media") | Ok("settings-games") | Ok("settings-shortcuts") => {
                use crate::views::settings::SettingsTab;
                self.settings_tab = match std::env::var("RECALL_NATIVE_ROUTE").as_deref() {
                    Ok("settings-clip") => SettingsTab::Clip,
                    Ok("settings-media") => SettingsTab::Media,
                    Ok("settings-games") => SettingsTab::Games,
                    _ => SettingsTab::Shortcuts,
                };
                self.go(Route::Settings);
            }
            _ => {}
        }
    }
}
