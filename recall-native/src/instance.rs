//! One Recall window per user. A second start brings the running window to the front
//! (restoring it when minimized) and exits; it never opens a second library.
//!
//! Scripted checks (`RECALL_NATIVE_OFFSCREEN`, `RECALL_NATIVE_BACKGROUND`) run beside the
//! person's own Recall and take no guard, unless `RECALL_INSTANCE_TAG` gives them one of
//! their own. A check never moves a window to the front.
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GWL_STYLE, GetWindowLongPtrW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, SW_RESTORE,
    SW_SHOW, SetForegroundWindow, ShowWindow, WS_CAPTION, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, PWSTR};

#[path = "../../recall/src/single_instance.rs"]
mod single_instance;

pub use single_instance::Guard;

/// How long a second start looks for the first one's window (it may still be opening),
/// or waits for a closing instance to let go.
const HAND_OFF_WAIT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(100);

/// What `main` does next.
pub enum Start {
    /// Run the app; keep the guard (if any) until the window closes.
    Run(Option<Guard>),
    /// Another instance has the window: exit 0.
    Exit,
}

/// Claims the app's guard, or hands off to the running instance.
pub fn start() -> Start {
    let check = crate::offscreen::enabled() || std::env::var_os("RECALL_NATIVE_BACKGROUND").is_some();
    let tagged = std::env::var_os(single_instance::TAG_ENV).is_some();
    if check && !tagged {
        return Start::Run(None);
    }
    let name = single_instance::mutex_name(single_instance::APP);
    let me = std::process::id();
    let exe = std::env::current_exe().ok().and_then(|p| normalized(&p));
    let until = Instant::now() + HAND_OFF_WAIT;
    loop {
        match single_instance::claim(&name) {
            single_instance::Claim::First(guard) => return Start::Run(Some(guard)),
            single_instance::Claim::Unavailable(code) => {
                note("guard unavailable", serde_json::json!({ "error": code }));
                return Start::Run(None);
            }
            single_instance::Claim::Taken => {}
        }
        // A check looks only at its own program file. A person's start shows any Recall
        // window: the installed app's, when a dev build was opened.
        let windows = match exe.as_deref() {
            Some(exe) if check => app_windows(&|path| path == exe, me),
            Some(exe) => {
                let name = exe.file_name().map(|n| n.to_os_string());
                app_windows(&|path| path.file_name().map(|n| n.to_os_string()) == name, me)
            }
            None => Vec::new(),
        };
        if let Some(&(hwnd, pid)) = windows.first() {
            if !check {
                bring_to_front(hwnd);
            }
            note("second start: showed the running window", serde_json::json!({ "pid": pid, "moved": !check }));
            return Start::Exit;
        }
        if Instant::now() >= until {
            // The running one has no window yet (still opening) or no longer (closing
            // with nothing to save). Starting a second library would be worse.
            note("second start: no window found; exiting", serde_json::json!({}));
            return Start::Exit;
        }
        std::thread::sleep(POLL);
    }
}

fn note(event: &str, data: serde_json::Value) {
    eprintln!("Recall native: {event} {data}");
    let _ = crate::trace::player_log(event, data);
}

/// Restores a minimized window (a maximized one stays maximized) and activates it.
fn bring_to_front(hwnd: HWND) {
    unsafe {
        let show = if IsIconic(hwnd).as_bool() { SW_RESTORE } else { SW_SHOW };
        let _ = ShowWindow(hwnd, show);
        let _ = SetForegroundWindow(hwnd);
    }
}

/// Top-level application windows of other processes whose program file `wanted` accepts
/// (normalized: canonical, lower case).
fn app_windows(wanted: &dyn Fn(&Path) -> bool, except_pid: u32) -> Vec<(HWND, u32)> {
    struct Scan<'a> {
        wanted: &'a dyn Fn(&Path) -> bool,
        except: u32,
        found: Vec<(HWND, u32)>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> BOOL {
        let scan = unsafe { &mut *(data.0 as *mut Scan<'_>) };
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
        let extended = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
        if !is_application_window(style, extended) {
            return BOOL(1);
        }
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid != scan.except && process_path(pid).is_some_and(|path| (scan.wanted)(&path)) {
            scan.found.push((hwnd, pid));
        }
        BOOL(1)
    }
    let mut scan = Scan { wanted, except: except_pid, found: Vec::new() };
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut scan as *mut Scan<'_> as isize)) };
    // A visible window first: GPUI may own hidden helper windows too.
    scan.found.sort_by_key(|(hwnd, _)| !unsafe { IsWindowVisible(*hwnd) }.as_bool());
    scan.found
}

/// GPUI marks its normal windows APPWINDOW and popups TOOLWINDOW (same rule as the tray's
/// "Open Recall").
fn is_application_window(style: u32, extended: u32) -> bool {
    extended & WS_EX_TOOLWINDOW.0 == 0 && (extended & WS_EX_APPWINDOW.0 != 0 || style & WS_CAPTION.0 != 0)
}

fn process_path(pid: u32) -> Option<PathBuf> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()? };
    let mut wide = vec![0u16; 32_768];
    let mut len = wide.len() as u32;
    let read = unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(wide.as_mut_ptr()), &mut len) };
    let _ = unsafe { CloseHandle(process) };
    read.ok()?;
    normalized(Path::new(&String::from_utf16_lossy(&wide[..len as usize])))
}

/// Canonical, lower-case, without the `\\?\` prefix, so two spellings of one file compare equal.
fn normalized(path: &Path) -> Option<PathBuf> {
    let full = std::fs::canonicalize(path).ok()?;
    let raw = full.to_string_lossy().replace('/', "\\");
    let raw = raw.strip_prefix(r"\\?\").unwrap_or(&raw);
    Some(PathBuf::from(raw.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_windows_exclude_tool_windows() {
        assert!(is_application_window(0, WS_EX_APPWINDOW.0));
        assert!(is_application_window(WS_CAPTION.0, 0));
        assert!(!is_application_window(WS_CAPTION.0, WS_EX_TOOLWINDOW.0));
        assert!(!is_application_window(0, 0));
    }

    #[test]
    fn our_own_windows_are_never_the_running_instance() {
        let exe = normalized(&std::env::current_exe().unwrap()).unwrap();
        assert!(app_windows(&|path| path == exe, std::process::id()).iter().all(|(_, pid)| *pid != std::process::id()));
    }

    #[test]
    fn paths_compare_case_and_prefix_free() {
        let exe = std::env::current_exe().unwrap();
        let upper = PathBuf::from(exe.to_string_lossy().to_uppercase());
        assert_eq!(normalized(&exe), normalized(&upper));
    }
}
