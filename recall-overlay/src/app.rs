//! The running overlay: controller feed → reducer → island content → layout →
//! Direct2D frame → layered window, and the 4 Hz stats poller (alive only
//! while the stats pill can show, plus a short grace: `stats_gate`).

use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use windows::Win32::Foundation::{HANDLE, HWND, WAIT_OBJECT_0};
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::Win32::UI::WindowsAndMessaging::{
    IsWindowVisible, KillTimer, SetTimer, SetWindowPos, ShowWindow,
    HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE,
};

use crate::dynamic::{hud_scan, HudCache, Prober};
use crate::fonts::Fonts;
use crate::island::{self, stats_content, Content, Island};
use crate::hint::{choose_card, CardScene, GAP};
use crate::layout::{leader_scene, stats_scene, Scene};
use crate::placement::{self, look_origin, Rect, Spot, Spots, EDGE};
use crate::presence::{CardLook, Look};
use crate::badge::Badge;
use crate::reducer::Reducer;
use crate::render::{Layers, Outgoing, Renderer};
use crate::smoke::Smoke;
use crate::spot_edit::{Editor, LookBox, SavedSpot};
use crate::stats::gpu_watch::GpuBackoff;
use crate::stats::{StatsPoller, StatsSnapshot};
use crate::stats_gate::StatsGate;
use crate::trace::Trace;
use crate::zorder::{self, game_in_front, keep_on_top, Names, Raise};

#[path = "app_place.rs"]
mod place;
#[path = "app_editor.rs"]
pub(crate) mod editor;
#[path = "app_panel.rs"]
mod panel;

pub const BASE_TIMER: usize = 1;
pub const BASE_MS: u32 = 250;
pub const ANIM_TIMER: usize = 2;
const ANIM_MS: u32 = 15;
const POLL_EVERY: Duration = Duration::from_millis(240);

/// The pipe's write half while connected (the island's own commands: `hud_probe`).
pub type PipeOut = Arc<Mutex<Option<Box<dyn Write + Send>>>>;

#[derive(Debug)]
pub enum Feed {
    Connected,
    Disconnected,
    Json(Value),
}

/// Where the island sits for the current target.
pub struct Placed {
    pub key: String,
    pid: Option<u32>,
    hwnd: Option<isize>,
    pub monitor: Rect,
    /// Where the island may sit: the monitor, or its work area when the game
    /// does not cover it (the taskbar shows).
    pub area: Rect,
    /// The game picture whose shape keys the HUD scans (`window::Where`).
    picture: Rect,
    pub spot: Spot,
    pub scale: f32,
    /// Action notices lay out at `scale * grow` (the HUD-aware growth).
    pub grow: f32,
    /// The scan's free depth below the island's top (physical px; unbounded
    /// when unscanned): a spring's overshoot never reaches past it.
    max_h: f32,
    /// The scan's free rows per column (the hint card checks its own columns).
    hud: Option<Vec<u64>>,
    /// Dynamic: the spot the user placed (island editor); it sets the origin.
    custom: Option<SavedSpot>,
}

/// What the last pushed frame showed: scene, window origin, look, sweep
/// position (0..1000), save badge and the old content's opacity (0..1000).
/// Equal = no redraw.
type Drawn = (Scene, [i32; 2], Look, Option<u16>, Option<Badge>, Option<u16>);

pub struct App {
    pub reducer: Reducer,
    pub rx: Receiver<Feed>,
    pub parent_handle: Option<HANDLE>,
    pub parent_pid: Option<u32>,
    /// `--corner`: `dynamic`, or a fixed corner (`Anchor::from_corner`).
    pub corner: String,
    pub out: PipeOut,
    hud_cache: HudCache,
    prober: Prober,
    pub diagnostics_path: Option<PathBuf>,
    pub capture_exclusion_requested: bool,
    pub capture_exclusion_result: String,
    pub smoke: Option<Smoke>,
    pub started: Instant,
    pub island: Island,
    pub renderer: Renderer,
    pub fonts: Fonts,
    spots: Spots,
    pub placed: Option<Placed>,
    /// This base tick's look at the target's monitor and area (`probe_place`).
    place_probe: Option<crate::window::Where>,
    poller: Option<StatsPoller>,
    /// The last poller's NVML back-off, kept across pill showings.
    gpu_backoff: Option<GpuBackoff>,
    stats_gate: StatsGate,
    last_poll: Option<Instant>,
    pub stats: StatsSnapshot,
    scene: Scene,
    /// The scale `scene` was laid out at (an action notice's is grown).
    scene_scale: f32,
    /// The scene (and its scale) an open pill is crossfading out of.
    outgoing: Option<(Scene, f32)>,
    /// The shortcut hint's card (kept while it folds away).
    card_scene: Option<CardScene>,
    drawn: Option<Drawn>,
    pub bounds: [i32; 4],
    pub render_error: Option<String>,
    anim_running: bool,
    window_visible: bool,
    /// The change-only log (`trace`); off in smoke runs.
    pub trace: Trace,
    names: Names,
    /// The island editor while it runs.
    editor: Option<Editor>,
    /// The editor's guide grid window (only while the editor runs).
    guide: Option<crate::guide_window::GuideWindow>,
    /// The editor's panel window (only while the editor runs).
    panel: Option<crate::panel_window::PanelWindow>,
}

impl App {
    pub fn new(reducer: Reducer, rx: Receiver<Feed>, renderer: Renderer, fonts: Fonts) -> Self {
        let now = Instant::now();
        Self {
            reducer,
            rx,
            parent_handle: None,
            parent_pid: None,
            corner: String::new(),
            out: Arc::new(Mutex::new(None)),
            hud_cache: HudCache::default(),
            prober: Prober::default(),
            diagnostics_path: None,
            capture_exclusion_requested: true,
            capture_exclusion_result: String::new(),
            smoke: None,
            started: now,
            island: Island::new(now),
            renderer,
            fonts,
            spots: Spots::embedded(),
            placed: None,
            place_probe: None,
            poller: None,
            gpu_backoff: None,
            stats_gate: StatsGate::default(),
            last_poll: None,
            stats: StatsSnapshot::default(),
            scene: Scene::default(),
            scene_scale: 1.0,
            outgoing: None,
            card_scene: None,
            drawn: None,
            bounds: [0; 4],
            render_error: None,
            anim_running: false,
            window_visible: false,
            trace: Trace::default(),
            names: Names::default(),
            editor: None,
            guide: None,
            panel: None,
        }
    }

    pub fn poller_active(&self) -> bool {
        self.poller.is_some()
    }

    pub fn nvml_loaded(&self) -> bool {
        self.poller.as_ref().is_some_and(StatsPoller::nvml_loaded)
    }

    /// The base 4 Hz tick (also run when the pipe thread posts a wake-up).
    /// Returns (close the overlay, diagnostics-worthy change).
    pub fn tick(&mut self, hwnd: HWND) -> (bool, bool) {
        let now = Instant::now();
        if let Some(handle) = self.parent_handle {
            if unsafe { WaitForSingleObject(handle, 0) } == WAIT_OBJECT_0 {
                return (true, false);
            }
        }
        let elapsed = now.saturating_duration_since(self.started);
        if self.smoke.as_ref().and_then(|s| s.exit_after).is_some_and(|at| elapsed >= at) {
            return (true, false);
        }
        let mut changed = false;
        while let Ok(feed) = self.rx.try_recv() {
            changed |= match feed {
                Feed::Connected => self.reducer.connected(),
                Feed::Disconnected => self.reducer.disconnected(),
                Feed::Json(value) if editor::is_toggle(&value) => self.answer_editor_toggle(hwnd, now),
                Feed::Json(value) => match hud_scan(&value) {
                    Some(scan) => self.on_hud_scan(scan, now),
                    None => self.reducer.apply_envelope(&value, now),
                },
            };
        }
        self.probe_hud(now);
        if let Some(smoke) = self.smoke.as_mut() {
            changed |= smoke.apply_due(&mut self.reducer, elapsed, now);
        }
        if !self.smoke.as_ref().is_some_and(|s| s.hold) {
            changed |= self.reducer.tick(now);
        }
        changed |= self.poll_stats(now);
        changed |= self.watch_editor(hwnd, now);
        self.probe_place();
        let was_shown = self.island.shown();
        self.refresh(hwnd, now);
        if self.trace.enabled() {
            let foreground = self.names.foreground();
            self.trace.note("foreground", foreground);
            self.trace.note("notification state", zorder::notification_state());
        }
        (false, changed || was_shown != self.island.shown())
    }

    /// The animation timer: next frame only.
    pub fn animate(&mut self, hwnd: HWND) -> bool {
        let was_shown = self.island.shown();
        self.render(hwnd, Instant::now());
        was_shown != self.island.shown()
    }

    fn poll_stats(&mut self, now: Instant) -> bool {
        let (view, session) = (self.reducer.view(), self.reducer.session());
        let pid = session.target_pid.filter(|_| !session.desktop);
        let in_game = self.smoke.is_some() || game_in_front(pid, session.target_hwnd.filter(|_| !session.desktop));
        let can_show = island::stats_shown(view, session, in_game);
        if !self.stats_gate.keep(session.overlay_stats, can_show, self.poller.is_some(), now) {
            self.last_poll = None;
            let stopped = self.poller.take().map(|p| self.gpu_backoff = Some(p.gpu_backoff())).is_some();
            let cleared = self.stats != StatsSnapshot::default();
            self.stats = StatsSnapshot::default();
            return stopped || cleared;
        }
        if let Some(fixed) = self.smoke.as_ref().and_then(|s| s.stats.clone()) {
            let changed = self.stats != fixed;
            self.stats = fixed;
            return changed;
        }
        if self.last_poll.is_some_and(|at| now.saturating_duration_since(at) < POLL_EVERY) {
            return false;
        }
        let gpu = self.gpu_backoff;
        let poller = self.poller.get_or_insert_with(|| StatsPoller::start(gpu));
        let next = poller.poll(pid);
        if let Some(note) = poller.take_note() {
            self.trace.note("frame timing", note);
        }
        self.last_poll = Some(now);
        let changed = next != self.stats;
        self.stats = next;
        changed
    }

    /// Lays out `content` at the current placement's scale: the scene, the
    /// scale it was laid out at and the island's size. An action notice lays
    /// out grown and sizes over the stats pill under it (`stats_under`).
    fn scene_for(&mut self, content: &Content, stats_under: bool) -> (Scene, f32, (f32, f32)) {
        let scale = self.place();
        let stats = |app: &Self| stats_scene(&stats_content(&app.reducer, &app.stats), scale, &app.fonts);
        let laid = match content {
            Content::Notice(notice) => {
                let grow = self.placed.as_ref().map_or(1.0, |p| p.grow);
                let under = stats_under.then(|| stats(self).w);
                // A horizontal pill stops at its area's width less the edge margins.
                let max = self.placed.as_ref().map_or(f32::INFINITY, |p| ((p.area.right - p.area.left) as f32 - 2.0 * EDGE * p.scale).max(0.0));
                crate::fit_width::fit_notice(notice, max, |n| island::notice_layout(n, scale, grow, || under, &self.fonts))
            }
            Content::Stats => {
                let scene = stats(self);
                let size = (scene.w, scene.h);
                (scene, scale, size)
            }
            Content::Leader(leader) => {
                let scene = leader_scene(leader, scale, &self.fonts);
                let size = (scene.w, scene.h);
                (scene, scale, size)
            }
        };
        // A side spot: the same content, re-flowed tall with upright glyphs.
        if !self.placed.as_ref().is_some_and(|p| p.spot.anchor.turn() != crate::presence::Turn::None) {
            return laid;
        }
        let under = match content {
            Content::Notice(notice) if notice.emphasis && stats_under => Some(crate::stack::stack(&stats(self), scale, &self.fonts).w),
            _ => None,
        };
        // The column may use the side's height less the edge margins, never more.
        let max = self.placed.as_ref().map_or(f32::INFINITY, |p| ((p.area.bottom - p.area.top) as f32 - 2.0 * EDGE * p.scale).max(0.0));
        island::stacked_layout(laid, under, scale, &self.fonts, max)
    }

    /// The shortcut hint's card: laid out for the room the scan measured
    /// under the pill, unfolding while the chord window is open.
    fn refresh_hint(&mut self, now: Instant) {
        let hint = self.reducer.view().hint.clone().filter(|_| self.reducer.session().notices);
        let Some(hint) = hint else { return self.island.update_hint(false, now) };
        let scale = self.place();
        let pill_h = self.island.frame(now).h;
        let cover = |w: f32, h: f32| {
            let p = self.placed.as_ref()?;
            placement::card_cover(&p.spot, p.hud.as_deref(), &p.monitor, p.scale, (w, h), pill_h + GAP * p.scale)
        };
        self.card_scene = Some(choose_card(&hint, scale, &self.fonts, cover));
        self.island.update_hint(true, now);
    }

    /// `--smoke-render`: the fixture's current content, settled, to a BMP.
    pub fn snapshot(&mut self, path: &std::path::Path) -> Result<(), String> {
        self.poll_stats(Instant::now());
        let view = self.reducer.view();
        let content = island::content(view, self.reducer.session(), true)
            .ok_or("the fixture shows nothing (the island is hidden)")?;
        let under = island::stats_shown(view, self.reducer.session(), true);
        let (scene, scale, size) = self.scene_for(&content, under);
        crate::preview::snapshot(path, &mut self.renderer, &self.fonts, &scene, scale, size)
    }

    fn refresh(&mut self, hwnd: HWND, now: Instant) {
        let (view, session) = (self.reducer.view(), self.reducer.session());
        let in_game = self.smoke.is_some() || game_in_front(session.target_pid.filter(|_| !session.desktop), session.target_hwnd.filter(|_| !session.desktop));
        let under = island::stats_shown(view, session, in_game);
        let content = self.editor_content().or_else(|| island::content(view, session, in_game));
        if self.trace.enabled() {
            self.trace.note("island", island::state_line(content.as_ref(), view, session, in_game));
        }
        crate::act::state(content.as_ref(), view, session, in_game);
        match content {
            Some(content) => {
                let (scene, scale, (w, h)) = self.scene_for(&content, under);
                let morphing = self.island.update(Some((content, w, h)), now);
                let old = (std::mem::replace(&mut self.scene, scene), std::mem::replace(&mut self.scene_scale, scale));
                if morphing {
                    self.outgoing = Some(old);
                }
            }
            None => {
                self.island.update(None, now);
            }
        }
        self.refresh_hint(now);
        self.render(hwnd, now);
    }

    fn render(&mut self, hwnd: HWND, now: Instant) {
        if self.island.tick(now) {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            let still = unsafe { IsWindowVisible(hwnd) }.as_bool();
            crate::act::window(false, !still, None);
            self.trace.note("window", if still { "hide failed: still visible after ShowWindow(SW_HIDE)" } else { "hidden (fade done)" });
            self.trace.note("raise", "idle (window hidden)");
            self.window_visible = false;
            self.drawn = None;
            self.placed = None;
        }
        if !self.island.shown() {
            self.set_anim(hwnd, false);
            return;
        }
        let Some(placed) = self.placed.as_ref() else { return };
        let mut look = self.island.look(now, placed.max_h.max(self.scene.h));
        let anchor = placed.spot.anchor;
        look.align = placement::align(anchor);
        look.up = anchor.side() == placement::Side::Bottom;
        look.turn = anchor.turn();
        let card = self.island.card(now).zip(self.card_scene.as_ref());
        // A side spot's card stands upright beside the pill: `c.h` along the edge, `c.w` across.
        look.card = card.map(|(progress, c)| CardLook::placed(look.turn, (c.w, c.h), GAP * placed.scale, progress));
        let edge = (EDGE * placed.scale).round() as i32;
        let (x, y) = match placed.custom {
            Some(spot) => {
                let b = LookBox { along: look.span_w(), depth: look.extent(), pill: look.box_h, margin: look.margin };
                crate::spot_edit::origin(&spot, &placed.monitor, &placed.area, b, edge)
            }
            None => look_origin(anchor, &placed.area, look.span_w(), look.extent(), look.margin, edge),
        };
        let (sheen, badge) = (self.island.sheen(now), self.island.badge(now));
        let fade = self.island.crossfade(now);
        let key = (self.scene.clone(), [x, y], look, sheen.map(|s| (s.progress * 1000.0) as u16), badge, fade.map(|f| (f.alpha * 1000.0) as u16));
        if self.drawn.as_ref() != Some(&key) {
            let outgoing = fade.zip(self.outgoing.as_ref()).map(|(f, (scene, scale))| Outgoing { scene, scale: *scale, alpha: f.alpha, icon: f.icon });
            let layers = Layers { sheen, badge, card: card.map(|(_, c)| c), outgoing };
            let drawn = self.renderer.draw(&self.fonts, &self.scene, look, self.scene_scale, layers);
            let shown = drawn.and_then(|(w, h)| self.renderer.present(hwnd, x, y, w, h, look.alpha).map(|()| (w, h)));
            match shown {
                Ok((w, h)) => {
                    self.render_error = None;
                    self.trace.note("present", "ok");
                    crate::act::present(None);
                    self.bounds = [x, y, x + w, y + h];
                    self.drawn = Some(key);
                }
                Err(error) => {
                    self.trace.note("present", format!("UpdateLayeredWindow failed: {}", zorder::win_error(&error)));
                    crate::act::present(Some(&error));
                    self.render_error = Some(error.to_string());
                }
            }
        }
        if !self.window_visible && self.render_error.is_none() {
            let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
            let placed = unsafe {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags)
            };
            let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
            crate::act::window(true, visible && placed.is_ok(), placed.as_ref().err());
            let line = match placed {
                Ok(()) if visible => "shown, topmost set".to_string(),
                Ok(()) => "show failed: not visible after ShowWindow(SW_SHOWNOACTIVATE)".to_string(),
                Err(error) => format!("shown (visible {visible}), SetWindowPos topmost failed: {}", zorder::win_error(&error)),
            };
            self.trace.note("window", line);
            self.window_visible = true;
        } else if self.window_visible {
            match keep_on_top(hwnd, &mut self.names) {
                Raise::Clear => {}
                Raise::Raised { above, result: Ok(()) } => {
                    self.trace.note("raise", format!("re-raised over {above}"));
                    crate::act::raised(true);
                }
                Raise::Raised { above, result: Err(error) } => {
                    self.trace.note("raise", format!("re-raise failed: {error}, over {above}"));
                    crate::act::raised(false);
                }
            }
        }
        if self.guide.is_some() {
            self.update_guide(hwnd);
        }
        if self.panel.is_some() {
            self.update_panel();
        }
        let animating = self.island.animating(now);
        self.set_anim(hwnd, animating);
    }

}

impl App {
    fn set_anim(&mut self, hwnd: HWND, on: bool) {
        if on == self.anim_running {
            return;
        }
        unsafe {
            if on {
                SetTimer(Some(hwnd), ANIM_TIMER, ANIM_MS, None);
            } else {
                let _ = KillTimer(Some(hwnd), ANIM_TIMER);
            }
        }
        self.anim_running = on;
    }
}
