//! Where a captured window's client area sits inside its WGC frames, in physical pixels.
//!
//! WGC captures a window's extended frame bounds (`DWMWA_EXTENDED_FRAME_BOUNDS`): the title
//! bar and borders included, the invisible resize margins excluded. The client area's offset
//! in that frame is `ClientToScreen(0,0) - bounds.origin`, its size `GetClientRect`. The
//! thread is switched to per-monitor-v2 DPI awareness for the three calls (winit already makes
//! the process per-monitor-v2 aware; this keeps the numbers physical whoever calls), so every
//! value is in the same physical pixels as the captured texture.
//!
//! Measured on the capture thread when the frame size changes and at most every
//! [`REPROBE`] otherwise (a few microseconds of user32/dwmapi calls, never per frame).

use std::ffi::c_void;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{ClientToScreen, GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::HiDpi::{SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, IsIconic};

use crate::framing::{Rect, MAX_JITTER_PIXELS, MIN_ENCODE};

/// Longest a measurement is reused while the frame size stays the same.
pub const REPROBE: Duration = Duration::from_millis(500);

/// How long a titled window that grew must hold before the canvas moves, so a window being
/// dragged never reopens the encoder.
pub const SETTLE: Duration = Duration::from_secs(1);

/// A borderless / fullscreen fill (a display-mode or resolution change) moves the canvas after
/// [`FILL_FRAMES`] frames in a row at one size, or once it has held this long: the move lands
/// in the stall Windows takes to apply the change, not a second after the picture is back.
pub const FILL_SETTLE: Duration = Duration::from_millis(250);

/// Frames in a row at one size that settle a fill ([`FILL_SETTLE`]).
pub const FILL_FRAMES: u32 = 2;

/// Shortest time between two canvas moves, so a flip back and forth (an accidental hotkey, a
/// game cycling modes) reopens NVENC at most once per this gap. Matches the long-recording
/// part gap (`longrec::machine_parts::PART_GAP`).
pub const MOVE_GAP: Duration = Duration::from_secs(10);

/// A canvas move the window is holding still for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Settling {
    /// The canvas it would set (None = off the canvas, the frame at its own size).
    target: Option<(u32, u32)>,
    /// The frame size it was seen at (a fill counts frames at one size).
    frame: (u32, u32),
    since: Instant,
    /// Frames seen in a row at `frame` with this target.
    frames: u32,
}

/// A window's client area relative to its captured frame bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Client {
    /// Inside the bounds, smaller than them: the window has a title bar or borders.
    Inset(Rect),
    /// The client area is the whole frame (borderless / fullscreen).
    Fills,
    /// Not measurable now (minimized, closing, or a call failed).
    Unknown,
}

/// Classify a client area of `size` at screen `origin` against the frame `bounds`.
pub fn client_in_bounds(bounds: RECT, origin: POINT, size: (i32, i32)) -> Client {
    let (bw, bh) = (bounds.right - bounds.left, bounds.bottom - bounds.top);
    if bw <= 0 || bh <= 0 || size.0 <= 0 || size.1 <= 0 {
        return Client::Unknown;
    }
    let (x, y) = (origin.x - bounds.left, origin.y - bounds.top);
    let (x0, y0) = (x.max(0), y.max(0));
    let (x1, y1) = (x.saturating_add(size.0).min(bw), y.saturating_add(size.1).min(bh));
    if x1 <= x0 || y1 <= y0 {
        return Client::Unknown;
    }
    if (x0, y0, x1, y1) == (0, 0, bw, bh) {
        return Client::Fills;
    }
    Client::Inset(Rect { x: x0 as u32, y: y0 as u32, w: (x1 - x0) as u32, h: (y1 - y0) as u32 })
}

/// Measure `hwnd` now.
pub fn probe(hwnd: usize) -> Client {
    let hwnd = HWND(hwnd as *mut c_void);
    // SAFETY: plain user32/dwmapi queries on a window handle; a stale handle makes them fail.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            return Client::Unknown;
        }
        let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let mut bounds = RECT::default();
        let mut client = RECT::default();
        let mut origin = POINT::default();
        let ok = DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, (&mut bounds as *mut RECT).cast(), std::mem::size_of::<RECT>() as u32)
            .is_ok()
            && GetClientRect(hwnd, &mut client).is_ok()
            && ClientToScreen(hwnd, &mut origin).as_bool();
        if !previous.0.is_null() {
            SetThreadDpiAwarenessContext(previous);
        }
        if !ok {
            return Client::Unknown;
        }
        client_in_bounds(bounds, origin, (client.right - client.left, client.bottom - client.top))
    }
}

/// The capture thread's view of the target window: its client area, re-measured when the
/// frame size changes or [`REPROBE`] has passed, and whether this session records on the
/// canvas, and at what size ([`canvas_for`]). The clip follows the game's size until the game
/// first fills its monitor (fullscreen or borderless fullscreen): a window that settles
/// ([`SETTLE`]) larger than the canvas moves it to the new client size, a fill that settles
/// ([`FILL_SETTLE`]) leaves it (the frame records at its own size) -- [`canvas_move`], at most
/// once per [`MOVE_GAP`]. A splash or launcher window that does not cover its monitor never
/// settles it. Once the game has filled its monitor the canvas no longer moves: later size and
/// mode changes are fitted into the clip, nothing is reopened. Until a move settles the picture
/// is fitted into the canvas. A move reopens NVENC; the replay buffer is kept (`encoder/live.rs`).
/// Monitor captures (no window) never use it.
pub struct ClientArea {
    hwnd: usize,
    client: Client,
    frame: (u32, u32),
    probed: Option<Instant>,
    canvas: Option<(u32, u32)>,
    measure: fn(usize) -> Client,
    /// The window's monitor size in physical pixels.
    monitor: fn(usize) -> Option<(u32, u32)>,
    /// The window filled its monitor at its own size: the game is up, the canvas stays.
    settled: bool,
    /// The current fill covers its monitor (measured with the client area).
    covers: bool,
    /// The last canvas move ([`MOVE_GAP`]).
    moved: Option<Instant>,
    /// A canvas move waiting for the window to hold still.
    settling: Option<Settling>,
}

impl ClientArea {
    /// `hwnd` 0 = a monitor capture.
    pub fn new(hwnd: usize) -> Self {
        let mut area = Self::with_probe(hwnd, probe);
        area.monitor = monitor_size;
        area
    }

    /// Tests: a scripted monitor size.
    #[cfg(test)]
    pub(crate) fn with_monitor(mut self, monitor: fn(usize) -> Option<(u32, u32)>) -> Self {
        self.monitor = monitor;
        self
    }

    pub(crate) fn with_probe(hwnd: usize, measure: fn(usize) -> Client) -> Self {
        Self { hwnd, client: Client::Unknown, frame: (0, 0), probed: None, canvas: None, measure, monitor: |_| None, settled: false, covers: false, moved: None, settling: None }
    }

    /// For a `frame`-sized capture at `now`: the client area to crop to (None = the whole
    /// frame) and the canvas the session records on (None = the frame at its own size).
    pub fn update(&mut self, frame: (u32, u32), now: Instant) -> (Option<Rect>, Option<(u32, u32)>) {
        if self.hwnd == 0 {
            return (None, None);
        }
        if self.frame != frame || self.probed.is_none_or(|t| now.saturating_duration_since(t) >= REPROBE) {
            let seen = (self.frame != (0, 0)).then_some(self.frame);
            self.client = (self.measure)(self.hwnd);
            self.frame = frame;
            self.probed = Some(now);
            self.covers = self.client == Client::Fills && (self.monitor)(self.hwnd).is_some_and(|monitor| covering(frame, monitor));
            match self.client {
                // Border jitter, not chrome: a native session keeps its encoder size.
                Client::Inset(r) if self.canvas.is_none() && within_jitter(r, frame) => self.client = Client::Fills,
                Client::Inset(r) if self.canvas.is_none() => self.canvas = Some(canvas_for(seen, r, frame)),
                _ => {}
            }
        }
        if self.canvas.is_none() && self.covers {
            // Recording at its own size, filling its monitor: the game is up.
            self.settled = true;
        }
        if self.settled {
            self.settling = None;
        } else {
            self.settle(frame, now);
        }
        let crop = match self.client {
            Client::Inset(r) => r.clamp_to(frame),
            _ => None,
        };
        (crop, self.canvas)
    }

    /// A [`canvas_move`] the window has held ([`SETTLE`]; a fill [`FILL_SETTLE`]) is applied,
    /// unless the last move
    /// was under [`MOVE_GAP`] ago (it stays pending until the gap has passed).
    fn settle(&mut self, frame: (u32, u32), now: Instant) {
        let Some(canvas) = self.canvas else {
            self.settling = None;
            return;
        };
        let Some(target) = canvas_move(canvas, self.client, frame, self.covers) else {
            self.settling = None;
            return;
        };
        let mut s = match self.settling {
            Some(s) if s.target == target && s.frame == frame => s,
            _ => Settling { target, frame, since: now, frames: 0 },
        };
        s.frames = s.frames.saturating_add(1);
        let waited = now.saturating_duration_since(s.since);
        let held = match target {
            None => s.frames >= FILL_FRAMES || waited >= FILL_SETTLE,
            Some(_) => waited >= SETTLE,
        };
        let gap_ok = self.moved.is_none_or(|t| now.saturating_duration_since(t) >= MOVE_GAP);
        if held && gap_ok {
            self.canvas = target;
            self.moved = Some(now);
            self.settling = None;
            self.settled = target.is_none() && self.covers;
        } else {
            self.settling = Some(s);
        }
    }
}

/// Where a canvas moves for a window now `client` in a `frame`-sized capture: off the
/// canvas (`Some(None)`) when the window fills its monitor (`covers`: fullscreen or borderless
/// fullscreen, at any size) or fills a frame at least the canvas's size, to the client size when
/// that is larger than the canvas on either side (past the jitter), else nowhere (None): a
/// smaller picture keeps the canvas and is centred on it.
pub(crate) fn canvas_move(canvas: (u32, u32), client: Client, frame: (u32, u32), covers: bool) -> Option<Option<(u32, u32)>> {
    match client {
        // The game's own fullscreen at whatever size it chose; a borderless window that does not
        // cover its monitor only when it is larger than the canvas.
        Client::Fills => (covers || (frame.0 + MAX_JITTER_PIXELS >= canvas.0 && frame.1 + MAX_JITTER_PIXELS >= canvas.1)).then_some(None),
        Client::Inset(r) => {
            let size = r.clamp_to(frame).unwrap_or(r).size();
            grew(size, canvas).then(|| Some(canvas_for(None, r, frame)))
        }
        Client::Unknown => None,
    }
}

/// The canvas a session moves to when the window first shows a title bar or borders: the
/// size the session already records at when it started borderless (`seen`, so the encoder
/// stays open), else the client area itself, so the picture fills the frame in its own shape.
/// Even (4:2:0 chroma) and never below what NVENC opens.
fn canvas_for(seen: Option<(u32, u32)>, client: Rect, frame: (u32, u32)) -> (u32, u32) {
    let (w, h) = seen.unwrap_or_else(|| client.clamp_to(frame).unwrap_or(client).size());
    ((w & !1).max(MIN_ENCODE.0), (h & !1).max(MIN_ENCODE.1))
}

/// A `frame` that covers a `monitor` (both in physical pixels), within the jitter tolerance.
fn covering(frame: (u32, u32), monitor: (u32, u32)) -> bool {
    frame.0 + MAX_JITTER_PIXELS >= monitor.0 && frame.1 + MAX_JITTER_PIXELS >= monitor.1
}

/// The size of the monitor `hwnd` is on (nearest), in physical pixels.
fn monitor_size(hwnd: usize) -> Option<(u32, u32)> {
    let hwnd = HWND(hwnd as *mut c_void);
    // SAFETY: plain user32 queries on a window handle; a stale handle gives the nearest monitor.
    unsafe {
        let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let ok = GetMonitorInfoW(monitor, &mut info).as_bool();
        if !previous.0.is_null() {
            SetThreadDpiAwarenessContext(previous);
        }
        let rect = info.rcMonitor;
        (ok && rect.right > rect.left && rect.bottom > rect.top).then(|| ((rect.right - rect.left) as u32, (rect.bottom - rect.top) as u32))
    }
}

/// `size` past the jitter tolerance beyond `canvas` on either side.
fn grew(size: (u32, u32), canvas: (u32, u32)) -> bool {
    size.0 > canvas.0 + MAX_JITTER_PIXELS || size.1 > canvas.1 + MAX_JITTER_PIXELS
}

/// A client area within the jitter tolerance of the frame on both axes.
fn within_jitter(client: Rect, frame: (u32, u32)) -> bool {
    client.w.abs_diff(frame.0) <= MAX_JITTER_PIXELS && client.h.abs_diff(frame.1) <= MAX_JITTER_PIXELS
}

#[cfg(test)]
#[path = "client_area_tests.rs"]
mod tests;
