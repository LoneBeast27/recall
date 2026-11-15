//! The foreground window as OS events report it, instead of asking Windows
//! several times on every tick. `SetWinEventHook` (out of context: nothing is
//! loaded into any other process, so no game is touched) tells the UI thread
//! when the foreground window changes, a window is minimized or restored, a
//! move or resize ends, or the desktop switches; display, DPI and work-area
//! changes arrive as window messages. Each one marks the overlay's window
//! checks due and posts one coalesced [`WM_WINEVENT`] so the island looks
//! again at once. No `EVENT_OBJECT_LOCATIONCHANGE`: it fires for every cursor
//! move. Every tick also reads the foreground again ([`refresh`]): ticks are
//! rare at idle, and one missed event can then never keep the stats pill away.
//! Without a hook (it failed) every read asks Windows, as before. While the
//! island shows, a top-level window appearing (`EVENT_OBJECT_SHOW`: a toast
//! over the island) wakes the re-raise too ([`watch_shows`]).

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetForegroundWindow, GetWindowThreadProcessId, PostMessageW, GA_ROOT, WM_APP,
};

/// Posted (at most one at a time) when a window event or display change came in.
pub const WM_WINEVENT: u32 = WM_APP + 2;

const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
const EVENT_SYSTEM_MOVESIZEEND: u32 = 0x000B;
const EVENT_SYSTEM_MINIMIZESTART: u32 = 0x0016;
const EVENT_SYSTEM_MINIMIZEEND: u32 = 0x0017;
const EVENT_SYSTEM_DESKTOPSWITCH: u32 = 0x0020;
const EVENT_OBJECT_SHOW: u32 = 0x8002;
/// The callback runs on the hooking thread, out of every other process.
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
/// The island's own windows (pill, guide, panel) are left out: every tick
/// reads the foreground anyway, and a re-raise is for other programs' windows.
const WINEVENT_SKIPOWNPROCESS: u32 = 0x0002;
const FLAGS: u32 = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;

type WinEventProc = unsafe extern "system" fn(isize, u32, HWND, i32, i32, u32, u32);

#[link(name = "user32")]
unsafe extern "system" {
    fn SetWinEventHook(min: u32, max: u32, module: isize, callback: Option<WinEventProc>, pid: u32, tid: u32, flags: u32) -> isize;
    fn UnhookWinEvent(hook: isize) -> i32;
}

/// The window the events wake (the overlay's).
static TARGET: AtomicIsize = AtomicIsize::new(0);
/// An event came in that the UI thread has not looked at yet.
static PENDING: AtomicBool = AtomicBool::new(false);
/// The hooks are in: the cache below follows the events.
static HOOKED: AtomicBool = AtomicBool::new(false);
static FG_ROOT: AtomicIsize = AtomicIsize::new(0);
static FG_PID: AtomicU32 = AtomicU32::new(0);
/// The `EVENT_OBJECT_SHOW` hook while the island shows (0 = none).
static SHOW_HOOK: AtomicIsize = AtomicIsize::new(0);

/// Hooks the window events for `hwnd`'s thread (call on the UI thread, which
/// pumps messages). False when a hook failed: reads then ask Windows each time.
pub fn install(hwnd: HWND) -> bool {
    TARGET.store(hwnd.0 as isize, Ordering::Relaxed);
    let ranges = [
        (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
        (EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZEEND),
        (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
        (EVENT_SYSTEM_DESKTOPSWITCH, EVENT_SYSTEM_DESKTOPSWITCH),
    ];
    // Kept for the process's life (Windows removes them when the thread ends).
    let hooked = ranges
        .iter()
        .all(|&(min, max)| unsafe { SetWinEventHook(min, max, 0, Some(on_event), 0, 0, FLAGS) } != 0);
    refresh();
    HOOKED.store(hooked, Ordering::Relaxed);
    hooked
}

unsafe extern "system" fn on_event(_hook: isize, _event: u32, _hwnd: HWND, _object: i32, _child: i32, _thread: u32, _time: u32) {
    signal();
}

/// Marks the window checks due and wakes the UI thread (one message until it looks).
pub fn signal() {
    if PENDING.swap(true, Ordering::AcqRel) {
        return;
    }
    let target = TARGET.load(Ordering::Relaxed);
    let posted = target != 0 && unsafe { PostMessageW(Some(HWND(target as *mut _)), WM_WINEVENT, WPARAM(0), LPARAM(0)) }.is_ok();
    // Not posted (no window yet, a full queue): the next event tries again.
    if !posted {
        PENDING.store(false, Ordering::Release);
    }
}

/// Hooks `EVENT_OBJECT_SHOW` while the island shows (`on`), unhooks it when
/// it hides: at idle nothing is marshalled to this thread for every window
/// that appears. Call on the UI thread; repeated calls with the same `on` do nothing.
pub fn watch_shows(on: bool) {
    let hook = SHOW_HOOK.load(Ordering::Relaxed);
    if on && hook == 0 {
        let hook = unsafe { SetWinEventHook(EVENT_OBJECT_SHOW, EVENT_OBJECT_SHOW, 0, Some(on_show), 0, 0, FLAGS) };
        SHOW_HOOK.store(hook, Ordering::Relaxed);
    } else if !on && hook != 0 {
        unsafe { UnhookWinEvent(hook) };
        SHOW_HOOK.store(0, Ordering::Relaxed);
    }
}

/// A top-level window (`OBJID_WINDOW`, `CHILDID_SELF`) other than the island appeared.
unsafe extern "system" fn on_show(_hook: isize, _event: u32, hwnd: HWND, object: i32, child: i32, _thread: u32, _time: u32) {
    let island = hwnd.0 as isize == TARGET.load(Ordering::Relaxed);
    if object == 0 && child == 0 && !hwnd.is_invalid() && !island && unsafe { GetAncestor(hwnd, GA_ROOT) } == hwnd {
        signal();
    }
}

/// True once per burst of events since the last call.
pub fn take() -> bool {
    PENDING.swap(false, Ordering::AcqRel)
}

/// The window event hooks are in. Without them nothing reports a foreground
/// change, so the base tick keeps its own cadence (`app_wake::needs_base`).
pub fn hooked() -> bool {
    HOOKED.load(Ordering::Relaxed)
}

/// Reads the foreground window into the cache (at the start of every tick).
pub fn refresh() {
    let (root, pid) = live();
    FG_ROOT.store(root, Ordering::Relaxed);
    FG_PID.store(pid, Ordering::Relaxed);
}

/// The foreground window's root and its process: cached while hooked.
pub fn current() -> (isize, u32) {
    if HOOKED.load(Ordering::Relaxed) {
        (FG_ROOT.load(Ordering::Relaxed), FG_PID.load(Ordering::Relaxed))
    } else {
        live()
    }
}

fn live() -> (isize, u32) {
    let foreground = unsafe { GetForegroundWindow() };
    let root = unsafe { GetAncestor(foreground, GA_ROOT) };
    let mut owner = 0u32;
    unsafe { GetWindowThreadProcessId(foreground, Some(&mut owner)) };
    (root.0 as isize, owner)
}
