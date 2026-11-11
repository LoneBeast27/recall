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
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::HiDpi::{SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, IsIconic};

use crate::framing::{Rect, MAX_JITTER_PIXELS, MIN_ENCODE};

/// Longest a measurement is reused while the frame size stays the same.
pub const REPROBE: Duration = Duration::from_millis(500);

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
/// canvas, and at what size ([`canvas_for`]). That decision is sticky: once the window has
/// shown a title bar or borders, the session stays on that canvas (a later borderless /
/// fullscreen switch or resize keeps the encoder size; the picture is fitted into it). Monitor captures (no window) never use it.
pub struct ClientArea {
    hwnd: usize,
    client: Client,
    frame: (u32, u32),
    probed: Option<Instant>,
    canvas: Option<(u32, u32)>,
    measure: fn(usize) -> Client,
}

impl ClientArea {
    /// `hwnd` 0 = a monitor capture.
    pub fn new(hwnd: usize) -> Self {
        Self::with_probe(hwnd, probe)
    }

    pub(crate) fn with_probe(hwnd: usize, measure: fn(usize) -> Client) -> Self {
        Self { hwnd, client: Client::Unknown, frame: (0, 0), probed: None, canvas: None, measure }
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
            match self.client {
                // Border jitter, not chrome: a native session keeps its encoder size.
                Client::Inset(r) if self.canvas.is_none() && within_jitter(r, frame) => self.client = Client::Fills,
                Client::Inset(r) if self.canvas.is_none() => self.canvas = Some(canvas_for(seen, r, frame)),
                _ => {}
            }
        }
        let crop = match self.client {
            Client::Inset(r) => r.clamp_to(frame),
            _ => None,
        };
        (crop, self.canvas)
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

/// A client area within the jitter tolerance of the frame on both axes.
fn within_jitter(client: Rect, frame: (u32, u32)) -> bool {
    client.w.abs_diff(frame.0) <= MAX_JITTER_PIXELS && client.h.abs_diff(frame.1) <= MAX_JITTER_PIXELS
}

#[cfg(test)]
#[path = "client_area_tests.rs"]
mod tests;
