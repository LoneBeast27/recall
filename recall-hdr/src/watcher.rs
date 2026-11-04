//! Foreground watcher: SetWinEventHook(EVENT_SYSTEM_FOREGROUND) marshaling
//! (pid, exe stem, title, fullscreen?) to the main thread via a channel.
//!
//! This is the *event-driven* half of the reconciler's input. Instead of polling
//! the foreground window on a timer, we install a low-overhead accessibility hook
//! that the OS fires exactly when the system foreground window changes. Each event
//! is turned into a cheap [`WatchEvent`] and pushed down an `mpsc` channel; the
//! reconciler thread (drained elsewhere) decides what to do with it.
//!
//! # Threading contract (read this before wiring it up)
//! `SetWinEventHook` with `WINEVENT_OUTOFCONTEXT` delivers callbacks **on the
//! thread that installed the hook, via that thread's message pump**. Therefore:
//!
//! - [`Watcher::install`] MUST be called from the thread that runs the Win32
//!   message loop (`GetMessageW` / `DispatchMessageW`). If that thread never pumps
//!   messages, no events are ever delivered -- the hook is silent, not an error.
//! - The callback runs on that same thread. It must stay cheap and must never
//!   panic across the FFI boundary (we wrap it in `catch_unwind`) and never mutate
//!   the display or hold a lock across other calls.
//! - [`Watcher::follow`] adds a second, scoped hook on one window's location
//!   changes (`watcher_follow.rs`); same thread rules.
//! - The watcher is **single-instance**: only one hook may be installed process-
//!   wide at a time (the callback reads a single global sender). A second
//!   [`Watcher::install`] before the first is dropped returns `Err`.
//!
//! # Relationship to `rules.rs`
//! A sibling module (`rules.rs`) owns the `Foreground { exe_stem, title,
//! fullscreen_on_target, pid }` rule-engine input. To avoid a build-ordering race
//! between the two Wave-2 agents we do **not** import from `rules` here. Instead we
//! emit our own [`WatchEvent`] with the same load-bearing fields (plus the raw
//! `hwnd`); `app.rs` (the wiring layer) converts `WatchEvent -> Foreground` once
//! both modules exist.

use std::panic::catch_unwind;
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{
    HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsWindow, IsZoomed, EVENT_SYSTEM_FOREGROUND, GWL_STYLE, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS, WS_CAPTION,
};

use crate::log;

/// Half-tolerance (px) when deciding a window "covers" its monitor. Borderless
/// fullscreen windows sometimes overhang the monitor bounds by ~1px, so an exact
/// rect match is too strict -- we allow each edge to differ by up to this much.
const FULLSCREEN_TOL: i32 = 2;

/// A foreground-change event, marshaled off the hook callback onto the reconciler
/// thread. Deliberately owns plain data (`String`, `u32`, `bool`, `isize`) so it
/// crosses the channel `Send`-safely with no live Win32 handles.
///
/// `app.rs` converts this into the rule engine's `Foreground { exe_stem, title,
/// fullscreen_on_target, pid }` (see the module docs) -- the field names match on
/// purpose. `hwnd` is carried as a raw `isize` for the wiring layer that may want
/// to re-query the window; it is not a live handle and must be re-wrapped
/// (`HWND(hwnd as *mut _)`) before any Win32 call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchEvent {
    /// PID owning the new foreground window.
    pub pid: u32,
    /// Executable file stem (lower-case not forced -- callers denylist-compare).
    pub exe_stem: String,
    /// Executable file name with extension (e.g. `VALORANT-Win64-Shipping.exe`).
    pub exe_name: String,
    /// The window's title text (may be empty).
    pub title: String,
    /// True iff the window is fullscreen/borderless on ITS OWN monitor (any
    /// display) -- the record mode's "is this a game?" geometry signal.
    pub fullscreen: bool,
    /// True iff `fullscreen` AND that monitor is the managed one (HDR rules).
    pub fullscreen_on_target: bool,
    /// True iff the executable lives in a Steam library (`steamapps\common`):
    /// the record mode's "is this a game?" signal for windowed games.
    pub steam_game: bool,
    /// Raw `HWND` as an `isize` (not a live handle; re-wrap before use).
    pub hwnd: isize,
}

/// Whether an executable path is a game in a Steam library. Steamworks Shared
/// (the redistributable installers Steam runs before a first launch) is not.
pub(crate) fn is_steam_library_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase().replace('/', "\\");
    path.contains("\\steamapps\\common\\") && !path.contains("\\steamapps\\common\\steamworks shared\\")
}

/// The single global slot the callback reads from. `SetWinEventHook` has no
/// user-data pointer, so the callback cannot be handed a `&Watcher`; it must reach
/// a process-global instead. We store the channel sender plus the managed monitor
/// (as a raw `isize`, since `HMONITOR` is a non-`Send` pointer type and this static
/// is shared across threads). `None` = no watcher installed.
struct WatchState {
    tx: Sender<WatchEvent>,
    /// `HMONITOR` of the managed display, as a raw `isize` (`hmonitor.0 as isize`).
    target_monitor: isize,
}

/// Process-global watcher state. `OnceLock<Mutex<Option<..>>>`: the `OnceLock`
/// lazily creates the mutex; the `Option` toggles install/uninstall. Only one
/// watcher may be live at a time -- `install` fails if the slot is already `Some`.
static STATE: OnceLock<Mutex<Option<WatchState>>> = OnceLock::new();

fn state() -> &'static Mutex<Option<WatchState>> {
    STATE.get_or_init(|| Mutex::new(None))
}

/// Owns the installed `SetWinEventHook`. Unhooks on [`Watcher::uninstall`] or on
/// `Drop`, and clears the global [`STATE`] so a later `install` can succeed.
///
/// Not `Send`/`Sync`: it wraps the raw `HWINEVENTHOOK` and, per the hook's
/// threading contract, must be dropped on the same (message-pumping) thread that
/// installed it. `UnhookWinEvent` is only valid from that thread.
#[derive(Debug)]
pub struct Watcher {
    hook: HWINEVENTHOOK,
    target_monitor: isize,
    /// Where location-hook flips go ([`Watcher::follow_into`]).
    follow_tx: Option<Sender<WatchEvent>>,
    /// The window [`Watcher::follow`] was last asked for (installed or not).
    follow_asked: Option<isize>,
    follow: Option<follow::FollowHook>,
}

#[path = "watcher_follow.rs"]
mod follow;

impl Watcher {
    /// Install the `EVENT_SYSTEM_FOREGROUND` hook on the **calling thread** and
    /// deliver [`WatchEvent`]s into `tx`. `target_monitor` is the `HMONITOR` of the
    /// managed display (passed as an `isize` per the module's public signature);
    /// it decides the `fullscreen_on_target` flag.
    ///
    /// # Threading (mandatory)
    /// Call this from the thread that runs the Win32 message pump. Events are
    /// delivered on that thread; without a pump, no events arrive. See the module
    /// docs.
    ///
    /// # Errors
    /// - The watcher is single-instance: if one is already installed (the global
    ///   slot is `Some`), this returns `Err` rather than silently replacing it.
    /// - Returns `Err` if `SetWinEventHook` fails (null hook handle).
    pub fn install(tx: Sender<WatchEvent>, target_monitor: isize) -> Result<Watcher, String> {
        // Single-instance guard: refuse if a watcher is already registered. Taking
        // the lock first means install/uninstall can't race the callback's read.
        let mut slot = state()
            .lock()
            .map_err(|_| "watcher state mutex poisoned".to_string())?;
        if slot.is_some() {
            return Err("watcher already installed (single-instance)".to_string());
        }

        // Install the OUTOFCONTEXT hook. SKIPOWNPROCESS means our own foreground
        // changes never fire the callback (belt-and-suspenders with the pid check).
        // SAFETY: `foreground_hook` is a valid 'static `extern "system"` routine;
        // hmod/idprocess/idthread are 0 (all processes/threads, out-of-context).
        let hook = unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(foreground_hook),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };
        if hook.0.is_null() {
            return Err("SetWinEventHook returned a null hook handle".to_string());
        }

        // Publish the sender + target only after a successful hook, so a failed
        // install leaves the global untouched.
        *slot = Some(WatchState { tx, target_monitor });
        drop(slot);
        log!("watcher: EVENT_SYSTEM_FOREGROUND hook installed (target monitor {target_monitor:#x})");
        Ok(Watcher { hook, target_monitor, follow_tx: None, follow_asked: None, follow: None })
    }

    /// Send [`Watcher::follow`]'s fullscreen flips into `tx` (a channel apart
    /// from foreground changes, so the caller can treat them like a probe).
    pub fn follow_into(&mut self, tx: Sender<WatchEvent>) {
        self.follow_tx = Some(tx);
    }

    /// Follow `hwnd`'s location changes (a scoped hook on its thread), or none.
    /// Replaces the previous hook; asking for the same window again is free,
    /// and a window that is gone is not retried until another is asked for.
    /// Pump thread only.
    pub fn follow(&mut self, hwnd: Option<isize>) {
        if self.follow_asked == hwnd {
            return;
        }
        self.follow_asked = hwnd;
        if let Some(f) = self.follow.take() {
            follow::stop(f);
        }
        if let (Some(h), Some(tx)) = (hwnd, self.follow_tx.as_ref()) {
            self.follow = follow::start(h, tx.clone(), self.target_monitor);
        }
    }

    /// The window a scoped hook is installed on.
    pub fn followed(&self) -> Option<isize> {
        self.follow.as_ref().and(self.follow_asked)
    }

    /// Explicitly uninstall the hook and clear the global state. Equivalent to
    /// dropping the `Watcher`; provided for symmetry with the spec's API. Consumes
    /// `self` so the `Drop` impl below does not double-unhook.
    pub fn uninstall(self) {
        // `self` drops at end of scope -> `Drop` does the unhook + clear.
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        if let Some(f) = self.follow.take() {
            follow::stop(f);
        }
        // SAFETY: `hook` is a handle we obtained from `SetWinEventHook` on this
        // thread and have not previously unhooked. Must run on the install thread.
        let ok = unsafe { UnhookWinEvent(self.hook) };
        if !ok.as_bool() {
            log!("watcher: UnhookWinEvent returned FALSE (hook may already be gone)");
        }
        // Clear the global so a subsequent `install` can succeed. Recover from a
        // poisoned lock -- we still want the slot emptied.
        let mut slot = state().lock().unwrap_or_else(|p| p.into_inner());
        *slot = None;
        log!("watcher: hook uninstalled");
    }
}

/// The Win32 `WINEVENTPROC`. Runs on the installing (message-pump) thread when the
/// system foreground window changes.
///
/// Contract enforced here: keep it CHEAP, never panic across FFI, never mutate the
/// display, never hold a lock across an unrelated Win32 call. We do the minimal
/// work -- identity + geometry probe -- then `tx.send` and return. Non-candidates
/// (null hwnd, empty stem, our own pid) are dropped cheaply without sending.
unsafe extern "system" fn foreground_hook(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _id_event_thread: u32,
    _ms_event_time: u32,
) {
    // A panic across an `extern "system"` boundary is UB. Wrap the whole body so a
    // surprise (e.g. an allocator failure in String) can never unwind into Win32.
    let _ = catch_unwind(|| {
        handle_foreground(hwnd);
    });
}

/// Callback body, split out so `catch_unwind` wraps a plain Rust `fn` (keeps the
/// `unsafe extern` shim tiny). Prefer the event's `hwnd` -- it is exactly the
/// window that just came to the foreground, so we skip a `GetForegroundWindow`.
fn handle_foreground(hwnd: HWND) {
    // Read the managed monitor + sender under one lock, then release it before
    // probing/sending (never hold the lock across Win32 calls or `tx.send`).
    let (target_monitor, tx) = {
        let guard = match state().lock() {
            Ok(g) => g,
            Err(_) => return, // poisoned: drop this event rather than panic
        };
        let Some(st) = guard.as_ref() else {
            return; // uninstalled between the OS event and now
        };
        (st.target_monitor, st.tx.clone())
    };
    if let Some(event) = build_event(hwnd, target_monitor) {
        // A closed receiver just means the reconciler shut down; drop the event.
        let _ = tx.send(event);
    }
}

/// Probe the CURRENT foreground window once (startup + the record mode's 1 s
/// re-probe, which catches games that go fullscreen after taking focus -- no
/// `EVENT_SYSTEM_FOREGROUND` fires for that). Same filtering as the hook.
pub fn probe_foreground(target_monitor: isize) -> Option<WatchEvent> {
    // SAFETY: plain query; a null result is handled by `build_event`.
    let hwnd = unsafe { GetForegroundWindow() };
    build_event(hwnd, target_monitor)
}

/// Turn a window into a [`WatchEvent`], or `None` for non-candidates (null hwnd,
/// no resolvable process / stem, our own process).
fn build_event(hwnd: HWND, target_monitor: isize) -> Option<WatchEvent> {
    // Cheap reject: no window handle.
    if hwnd.0.is_null() {
        return None;
    }

    // PID + exe identity. Skip windows with no resolvable owning process.
    let mut pid = 0u32;
    // SAFETY: `hwnd` is a live top-level window from the event / probe.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    // Our own pid: WINEVENT_SKIPOWNPROCESS already filters these at the OS level,
    // but re-check defensively so a self-event never reaches the reconciler.
    if pid == 0 || pid == std::process::id() {
        return None;
    }
    let (exe_stem, exe_name, steam_game) = exe_identity(pid)?;
    if exe_stem.trim().is_empty() {
        return None; // no stem -> not a real app window we can classify
    }

    let (fullscreen, monitor) = fullscreen_on_own_monitor(hwnd);
    Some(WatchEvent {
        pid,
        exe_stem,
        exe_name,
        title: window_title(hwnd),
        fullscreen,
        fullscreen_on_target: fullscreen && monitor == target_monitor,
        steam_game,
        hwnd: hwnd.0 as isize,
    })
}

/// Whether `hwnd` is fullscreen/borderless on the monitor it mostly occupies,
/// plus that monitor (as `isize`) for the managed-display attribution.
///
/// Uses `MONITOR_DEFAULTTONEAREST` so a window straddling monitors is attributed
/// to the one it mostly occupies -- matching how the OS treats "which display is
/// this fullscreen game on". A maximized window that still has a caption is a
/// normal app window, not a game: on a monitor without a taskbar its rect can
/// equal the monitor (custom-frame apps), so it is excluded explicitly.
fn fullscreen_on_own_monitor(hwnd: HWND) -> (bool, isize) {
    // SAFETY: `hwnd` is live; MonitorFromWindow falls back to the nearest monitor.
    let mon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `mon` is a valid HMONITOR just returned above; `mi.cbSize` is set.
    if !unsafe { GetMonitorInfoW(mon, &mut mi) }.as_bool() {
        return (false, mon.0 as isize);
    }
    let mut wr = RECT::default();
    // SAFETY: `hwnd` is live.
    if unsafe { GetWindowRect(hwnd, &mut wr) }.is_err() {
        return (false, mon.0 as isize);
    }
    // SAFETY: style/zoom reads on a live window.
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
    let zoomed = unsafe { IsZoomed(hwnd) }.as_bool();
    let captioned = style & WS_CAPTION.0 == WS_CAPTION.0;
    (is_fullscreen_geometry(&mi.rcMonitor, &wr, zoomed, captioned), mon.0 as isize)
}

/// Pure fullscreen decision: the window covers its monitor (within
/// [`FULLSCREEN_TOL`]) and is not a maximized captioned app window.
pub(crate) fn is_fullscreen_geometry(mon: &RECT, win: &RECT, zoomed: bool, captioned: bool) -> bool {
    rect_covers(mon, win, FULLSCREEN_TOL) && !(zoomed && captioned)
}

/// Whether `win` covers `mon` on all four edges within `tol` px. The window is
/// allowed to overhang (borderless windows do by ~1px) or fall short by `tol`.
fn rect_covers(mon: &RECT, win: &RECT, tol: i32) -> bool {
    (win.left - mon.left).abs() <= tol
        && (win.top - mon.top).abs() <= tol
        && (win.right - mon.right).abs() <= tol
        && (win.bottom - mon.bottom).abs() <= tol
}

// ---------------------------------------------------------------------------
// Vendored Win32 helpers
// ---------------------------------------------------------------------------

// vendored from recall/src/source.rs (exe_stem): OpenProcess with
// PROCESS_QUERY_LIMITED_INFORMATION + QueryFullProcessImageNameW, then take the
// file stem. Kept local to avoid a cross-crate dependency for two small helpers.
fn exe_stem(pid: u32) -> Option<String> {
    exe_identity(pid).map(|(stem, _, _)| stem)
}

/// `(file stem, file name, in a Steam library)` of `pid`'s executable image.
fn exe_identity(pid: u32) -> Option<(String, String, bool)> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let res =
            QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(handle);
        res.ok()?;
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        let path = std::path::Path::new(&full);
        Some((
            path.file_stem()?.to_string_lossy().into_owned(),
            path.file_name()?.to_string_lossy().into_owned(),
            is_steam_library_path(&full),
        ))
    }
}

/// Whether `hwnd` still names a live window.
pub fn window_alive(hwnd: isize) -> bool {
    // SAFETY: IsWindow tolerates any handle value.
    unsafe { IsWindow(Some(HWND(hwnd as *mut _))) }.as_bool()
}

/// Public identity lookup for env-pinned capture targets (`Start-Recall.ps1`).
pub fn window_identity(hwnd: isize) -> Option<(u32, String, String, String)> {
    let hwnd = HWND(hwnd as *mut _);
    let mut pid = 0u32;
    // SAFETY: GetWindowThreadProcessId tolerates a stale handle (pid stays 0).
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return None;
    }
    let (stem, name, _) = exe_identity(pid)?;
    Some((pid, stem, name, window_title(hwnd)))
}

// vendored from recall/src/source.rs (window_title): GetWindowTextW into a fixed
// buffer, UTF-16-lossy decode. Empty string on no title / error.
fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if len <= 0 {
        String::new()
    } else {
        String::from_utf16_lossy(&buf[..len as usize])
    }
}

#[cfg(test)]
#[path = "watcher_tests.rs"]
mod tests;
