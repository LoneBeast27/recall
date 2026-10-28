//! The running overlay: controller feed → reducer → island content → layout →
//! Direct2D frame → layered window, and the 4 Hz stats poller (alive only
//! while the stats toggle is on).

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use serde_json::Value;
use windows::Win32::Foundation::{HANDLE, HWND, RECT, WAIT_OBJECT_0};
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindow, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible, KillTimer, SetTimer, SetWindowPos, ShowWindow, GW_HWNDPREV, HWND_TOPMOST,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE,
};

use crate::fonts::Fonts;
use crate::island::{self, stats_content, Content, Island};
use crate::layout::{notice_scene, stats_scene, Scene};
use crate::placement::{self, position, Rect, Spot, Spots, EDGE};
use crate::reducer::Reducer;
use crate::render::Renderer;
use crate::smoke::Smoke;
use crate::stats::{StatsPoller, StatsSnapshot};
use crate::window::target_monitor;

pub const BASE_TIMER: usize = 1;
pub const BASE_MS: u32 = 250;
pub const ANIM_TIMER: usize = 2;
const ANIM_MS: u32 = 15;
const POLL_EVERY: Duration = Duration::from_millis(240);

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
    pub spot: Spot,
    pub scale: f32,
}

pub struct App {
    pub reducer: Reducer,
    pub rx: Receiver<Feed>,
    pub parent_handle: Option<HANDLE>,
    pub parent_pid: Option<u32>,
    pub corner: String,
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
    poller: Option<StatsPoller>,
    last_poll: Option<Instant>,
    pub stats: StatsSnapshot,
    scene: Scene,
    drawn: Option<(Scene, [i32; 4], u8, u8)>,
    pub bounds: [i32; 4],
    pub render_error: Option<String>,
    anim_running: bool,
    window_visible: bool,
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
            poller: None,
            last_poll: None,
            stats: StatsSnapshot::default(),
            scene: Scene::default(),
            drawn: None,
            bounds: [0; 4],
            render_error: None,
            anim_running: false,
            window_visible: false,
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
                Feed::Json(value) => self.reducer.apply_envelope(&value, now),
            };
        }
        if let Some(smoke) = self.smoke.as_mut() {
            changed |= smoke.apply_due(&mut self.reducer, elapsed, now);
        }
        if !self.smoke.as_ref().is_some_and(|s| s.hold) {
            changed |= self.reducer.tick(now);
        }
        changed |= self.poll_stats(now);
        let was_shown = self.island.shown();
        self.refresh(hwnd, now);
        (false, changed || was_shown != self.island.shown())
    }

    /// The animation timer: next frame only.
    pub fn animate(&mut self, hwnd: HWND) -> bool {
        let was_shown = self.island.shown();
        self.render(hwnd, Instant::now());
        was_shown != self.island.shown()
    }

    fn poll_stats(&mut self, now: Instant) -> bool {
        if !self.reducer.session().overlay_stats {
            self.last_poll = None;
            let stopped = self.poller.take().is_some();
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
        let pid = self.reducer.session().target_pid;
        let next = self.poller.get_or_insert_with(StatsPoller::start).poll(pid);
        self.last_poll = Some(now);
        let changed = next != self.stats;
        self.stats = next;
        changed
    }

    /// The current target's spot, monitor and scale (looked up on target changes
    /// and again each time the island appears).
    fn place(&mut self) -> f32 {
        let session = self.reducer.session();
        let key = session.game_key();
        let (pid, hwnd) = (session.target_pid, session.target_hwnd);
        let fresh = self.placed.as_ref().is_some_and(|p| p.key == key && p.pid == pid && p.hwnd == hwnd);
        if !fresh {
            let spot = self.spots.lookup(&key);
            let monitor = target_monitor(hwnd, pid);
            let scale = placement::scale(monitor.height(), &spot);
            self.placed = Some(Placed { key, pid, hwnd, monitor, spot, scale });
        }
        self.placed.as_ref().map_or(1.0, |p| p.scale)
    }

    /// Lays out `content` at the current placement's scale.
    fn scene_for(&mut self, content: &Content) -> (Scene, f32) {
        let scale = self.place();
        let scene = match content {
            Content::Notice(notice) => notice_scene(notice, scale, &self.fonts),
            Content::Stats => stats_scene(&stats_content(&self.reducer, &self.stats), scale, &self.fonts),
        };
        (scene, scale)
    }

    /// `--smoke-render`: the fixture's current content, settled, to a BMP.
    pub fn snapshot(&mut self, path: &std::path::Path) -> Result<(), String> {
        self.poll_stats(Instant::now());
        let content = island::content(self.reducer.view(), self.reducer.session(), true)
            .ok_or("the fixture shows nothing (the island is hidden)")?;
        let (scene, scale) = self.scene_for(&content);
        crate::preview::snapshot(path, &mut self.renderer, &self.fonts, &scene, scale)
    }

    fn refresh(&mut self, hwnd: HWND, now: Instant) {
        let session = self.reducer.session();
        let in_game = self.smoke.is_some() || game_in_front(session.target_pid.filter(|_| !session.desktop));
        match island::content(self.reducer.view(), session, in_game) {
            Some(content) => {
                let (scene, _) = self.scene_for(&content);
                self.island.update(Some((content, scene.w, scene.h)), now);
                self.scene = scene;
            }
            None => self.island.update(None, now),
        }
        self.render(hwnd, now);
    }

    fn render(&mut self, hwnd: HWND, now: Instant) {
        if self.island.tick(now) {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            self.window_visible = false;
            self.drawn = None;
            self.placed = None;
        }
        if !self.island.shown() {
            self.set_anim(hwnd, false);
            return;
        }
        let Some(placed) = self.placed.as_ref() else { return };
        let frame = self.island.frame(now);
        let (w, h) = (frame.w.ceil().max(1.0) as i32, frame.h.ceil().max(1.0) as i32);
        let edge = (EDGE * placed.scale).round() as i32;
        let (x, y) = position(placed.spot.anchor, &placed.monitor, w, edge);
        let alpha = (frame.alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
        let content_alpha = self.island.content_alpha(now);
        let key = (self.scene.clone(), [x, y, w, h], alpha, (content_alpha.clamp(0.0, 1.0) * 255.0).round() as u8);
        if self.drawn.as_ref() != Some(&key) {
            let drawn = self.renderer.draw(&self.fonts, &self.scene, frame, placed.scale, content_alpha);
            let shown = drawn.and_then(|(w, h)| self.renderer.present(hwnd, x, y, w, h, frame.alpha));
            match shown {
                Ok(()) => {
                    self.render_error = None;
                    self.bounds = [x, y, x + w, y + h];
                    self.drawn = Some(key);
                }
                Err(error) => self.render_error = Some(error.to_string()),
            }
        }
        if !self.window_visible && self.render_error.is_none() {
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags);
            }
            self.window_visible = true;
        } else if self.window_visible {
            keep_on_top(hwnd);
        }
        let animating = self.island.animating(now);
        self.set_anim(hwnd, animating);
    }

}

/// The stats are for the game only: true while the foreground window belongs
/// to the target game's process.
fn game_in_front(target_pid: Option<u32>) -> bool {
    let Some(pid) = target_pid else { return false };
    let mut owner = 0u32;
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut owner)) };
    owner == pid
}

/// Games that make their own window topmost (Rocket League does) cover the
/// island once they are activated. Re-raise it, without focus, only when a
/// visible window above it overlaps it (Explorer keeps 1x1 helper windows on
/// top that never cover anything).
fn keep_on_top(hwnd: HWND) {
    let mut mine = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut mine) }.is_err() {
        return;
    }
    let mut above = unsafe { GetWindow(hwnd, GW_HWNDPREV) }.unwrap_or_default();
    while !above.is_invalid() {
        let mut r = RECT::default();
        let overlaps = unsafe { GetWindowRect(above, &mut r) }.is_ok()
            && r.left < mine.right && mine.left < r.right && r.top < mine.bottom && mine.top < r.bottom;
        if overlaps && unsafe { IsWindowVisible(above) }.as_bool() {
            let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
            let _ = unsafe { SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags) };
            return;
        }
        above = unsafe { GetWindow(above, GW_HWNDPREV) }.unwrap_or_default();
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
