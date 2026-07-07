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
    GetMonitorInfoW, MonitorFromWindow, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{
    HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, EVENT_SYSTEM_FOREGROUND,
    WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
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
    /// The window's title text (may be empty).
    pub title: String,
    /// True iff the window is on the managed monitor AND covers it fully.
    pub fullscreen_on_target: bool,
    /// Raw `HWND` as an `isize` (not a live handle; re-wrap before use).
    pub hwnd: isize,
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
}

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
        Ok(Watcher { hook })
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
    // Cheap reject: no window handle.
    if hwnd.0.is_null() {
        return;
    }

    // PID + exe stem. Skip windows with no resolvable owning process.
    let mut pid = 0u32;
    // SAFETY: `hwnd` is the live foreground window from the event.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return;
    }
    let exe_stem = match exe_stem(pid) {
        Some(s) => s,
        None => return, // no stem -> not a real app window we can classify
    };
    if exe_stem.trim().is_empty() {
        return;
    }

    // Our own pid: WINEVENT_SKIPOWNPROCESS already filters these at the OS level,
    // but re-check defensively so a mis-flagged event never round-trips a self-
    // event to the reconciler.
    if pid == std::process::id() {
        return;
    }

    let title = window_title(hwnd);

    // Read the managed monitor + compute the fullscreen flag under one lock, then
    // release it before sending (never hold the lock across `tx.send`).
    let (fullscreen_on_target, tx) = {
        let guard = match state().lock() {
            Ok(g) => g,
            Err(_) => return, // poisoned: drop this event rather than panic
        };
        let Some(st) = guard.as_ref() else {
            return; // uninstalled between the OS event and now
        };
        let fs = is_fullscreen_on_target(hwnd, st.target_monitor);
        (fs, st.tx.clone())
    };

    let event = WatchEvent {
        pid,
        exe_stem,
        title,
        fullscreen_on_target,
        hwnd: hwnd.0 as isize,
    };
    // A closed receiver just means the reconciler shut down; drop the event.
    let _ = tx.send(event);
}

/// True iff `hwnd`'s nearest monitor is the managed one AND the window rect covers
/// that monitor's full bounds (within [`FULLSCREEN_TOL`] px per edge).
///
/// Uses `MONITOR_DEFAULTTONEAREST` so a window straddling monitors is attributed
/// to the one it mostly occupies -- matching how the OS treats "which display is
/// this fullscreen game on".
fn is_fullscreen_on_target(hwnd: HWND, target_monitor: isize) -> bool {
    // SAFETY: `hwnd` is the live foreground window; MonitorFromWindow can't fail
    // for a valid window (falls back to the nearest monitor).
    let mon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if mon.0 as isize != target_monitor {
        return false;
    }

    // Monitor bounds.
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `mon` is a valid HMONITOR just returned above; `mi.cbSize` is set.
    let got_mi = unsafe { GetMonitorInfoW(HMONITOR(target_monitor as *mut _), &mut mi) };
    if !got_mi.as_bool() {
        return false;
    }

    // Window bounds.
    let mut wr = RECT::default();
    // SAFETY: `hwnd` is the live foreground window.
    if unsafe { GetWindowRect(hwnd, &mut wr) }.is_err() {
        return false;
    }

    rect_covers(&mi.rcMonitor, &wr, FULLSCREEN_TOL)
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
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let res =
            QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(handle);
        res.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        Some(
            std::path::Path::new(&path)
                .file_stem()?
                .to_string_lossy()
                .into_owned(),
        )
    }
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
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    /// Clear the global slot so tests that assert on install-state start clean.
    /// Tests touching the singleton run under a shared serial lock (below) so this
    /// never races another test.
    fn reset_state() {
        let mut slot = state().lock().unwrap_or_else(|p| p.into_inner());
        *slot = None;
    }

    /// Serialize the tests that mutate the process-global watcher slot. Two
    /// installs in parallel would legitimately conflict (that's the feature), so we
    /// force them sequential to test each in isolation.
    static SERIAL: Mutex<()> = Mutex::new(());

    // --- rect_covers geometry (pure, no Win32) -------------------------------

    #[test]
    fn rect_covers_exact_match() {
        let mon = RECT { left: 0, top: 0, right: 3840, bottom: 2160 };
        let win = mon;
        assert!(rect_covers(&mon, &win, FULLSCREEN_TOL));
    }

    #[test]
    fn rect_covers_one_px_overhang_is_fullscreen() {
        // Borderless windows overhang by ~1px -- must still count as covering.
        let mon = RECT { left: 0, top: 0, right: 3840, bottom: 2160 };
        let win = RECT { left: -1, top: -1, right: 3841, bottom: 2161 };
        assert!(rect_covers(&mon, &win, FULLSCREEN_TOL));
    }

    #[test]
    fn rect_covers_tolerance_boundary() {
        let mon = RECT { left: 0, top: 0, right: 3840, bottom: 2160 };
        // Exactly at tolerance on every edge: still covers.
        let at = RECT {
            left: FULLSCREEN_TOL,
            top: FULLSCREEN_TOL,
            right: 3840 - FULLSCREEN_TOL,
            bottom: 2160 - FULLSCREEN_TOL,
        };
        assert!(rect_covers(&mon, &at, FULLSCREEN_TOL));
        // One px past tolerance on a single edge: does not cover.
        let over = RECT { left: FULLSCREEN_TOL + 1, ..at };
        assert!(!rect_covers(&mon, &over, FULLSCREEN_TOL));
    }

    #[test]
    fn rect_covers_windowed_is_not_fullscreen() {
        let mon = RECT { left: 0, top: 0, right: 3840, bottom: 2160 };
        let win = RECT { left: 100, top: 100, right: 1200, bottom: 800 };
        assert!(!rect_covers(&mon, &win, FULLSCREEN_TOL));
    }

    #[test]
    fn rect_covers_offset_monitor() {
        // Secondary monitor to the right of primary (non-zero origin).
        let mon = RECT { left: 3840, top: 0, right: 7680, bottom: 2160 };
        let win = mon;
        assert!(rect_covers(&mon, &win, FULLSCREEN_TOL));
        // A window at the primary's origin does NOT cover the secondary.
        let primary_win = RECT { left: 0, top: 0, right: 3840, bottom: 2160 };
        assert!(!rect_covers(&mon, &primary_win, FULLSCREEN_TOL));
    }

    // --- single-instance install semantics -----------------------------------

    #[test]
    fn second_install_is_rejected() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        reset_state();

        let (tx1, _rx1) = channel::<WatchEvent>();
        let w1 = Watcher::install(tx1, 0x1234)
            .expect("first install succeeds (or SetWinEventHook is unavailable in CI)");

        // Second install must be refused while the first is live.
        let (tx2, _rx2) = channel::<WatchEvent>();
        let err = Watcher::install(tx2, 0x5678)
            .expect_err("second concurrent install must be rejected");
        assert!(err.contains("already installed"), "unexpected error: {err}");

        // Dropping the first frees the slot; a fresh install then succeeds.
        drop(w1);
        let (tx3, _rx3) = channel::<WatchEvent>();
        let w3 = Watcher::install(tx3, 0x9abc).expect("install after drop succeeds");
        drop(w3);
    }

    #[test]
    fn uninstall_clears_slot_for_reinstall() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        reset_state();

        let (tx, _rx) = channel::<WatchEvent>();
        let w = Watcher::install(tx, 0x1).expect("install succeeds");
        // Explicit uninstall (consumes -> Drop -> clears the global slot).
        w.uninstall();

        let mut slot = state().lock().unwrap_or_else(|p| p.into_inner());
        assert!(slot.is_none(), "uninstall must clear the global watcher slot");
        *slot = None;
    }

    /// The vendored `exe_stem` must resolve the current test process to a non-empty
    /// stem (exercises the OpenProcess + QueryFullProcessImageNameW path for real).
    #[test]
    fn exe_stem_resolves_self() {
        let stem = exe_stem(std::process::id());
        assert!(stem.is_some(), "exe_stem(self) should resolve");
        assert!(!stem.unwrap().trim().is_empty(), "self stem is non-empty");
    }
}
