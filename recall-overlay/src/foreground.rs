//! The foreground window as OS events report it, instead of asking Windows
//! several times on every tick. `SetWinEventHook` (out of context: nothing is
//! loaded into any other process, so no game is touched) tells the UI thread
//! when the foreground window changes, a window is minimized or restored, a
//! move or resize ends, or the desktop switches; display, DPI and work-area
//! changes arrive as window messages. Each one marks the overlay's window
//! checks due and posts one coalesced [`WM_WINEVENT`] so the island looks
//! again at once. No `EVENT_OBJECT_LOCATIONCHANGE`: it fires for every cursor
//! move. Without a hook (it failed) every read asks Windows, as before.

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
/// The callback runs on the hooking thread, out of every other process.
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;

type WinEventProc = unsafe extern "system" fn(isize, u32, HWND, i32, i32, u32, u32);

#[link(name = "user32")]
unsafe extern "system" {
    fn SetWinEventHook(min: u32, max: u32, module: isize, callback: Option<WinEventProc>, pid: u32, tid: u32, flags: u32) -> isize;
}

/// The window the events wake (the overlay's).
static TARGET: AtomicIsize = AtomicIsize::new(0);
/// An event came in that the UI thread has not looked at yet.
static PENDING: AtomicBool = AtomicBool::new(false);
/// The hooks are in: the cache below follows the events.
static HOOKED: AtomicBool = AtomicBool::new(false);
static FG_ROOT: AtomicIsize = AtomicIsize::new(0);
static FG_PID: AtomicU32 = AtomicU32::new(0);

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
        .all(|&(min, max)| unsafe { SetWinEventHook(min, max, 0, Some(on_event), 0, 0, WINEVENT_OUTOFCONTEXT) } != 0);
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
    if target != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(target as *mut _)), WM_WINEVENT, WPARAM(0), LPARAM(0));
        }
    }
}

/// True once per burst of events since the last call; the cached foreground
/// is read again then.
pub fn take() -> bool {
    let due = PENDING.swap(false, Ordering::AcqRel);
    if due {
        refresh();
    }
    due
}

/// Reads the foreground window into the cache (also the slow safety re-check).
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
