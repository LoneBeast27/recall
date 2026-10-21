//! The record mode's notification-area UI (user ruling 2026-10-22, 03:41 IST:
//! "confirm recall system tray utility too. for start record stop record and
//! quit"). The menu is exactly: status line (disabled: "Recording VALORANT" /
//! "Waiting for a game" / "Recording paused") · Open Recall (focus or launch
//! `recall-native.exe`) · Stop recording / Start recording (pause toggle) ·
//! separator · Quit Recall. Double-click opens Recall. Saving and Start with
//! Windows live in the app. The icon is the Recall mark (ring + red dot while
//! recording, grey dot while paused; `assets/tray-*.rgba`, 32x32 RGBA). It
//! performs none of the actions itself -- [`RecordingTray::poll`] returns
//! commands for the pump.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use tray_icon::menu::{ContextMenu, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, BELOW_NORMAL_PRIORITY_CLASS, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowLongPtrW, GetWindowThreadProcessId, IsWindowVisible, PostMessageW,
    SetForegroundWindow, ShowWindow, GWL_EXSTYLE, GWL_STYLE, SW_RESTORE, WM_CLOSE, WS_CAPTION,
    WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};

use crate::supervisor::EngineHealth;

const ID_STATUS: &str = "record_status";
const ID_OPEN: &str = "record_open";
const ID_PAUSE: &str = "record_pause";
const ID_QUIT: &str = "record_quit";

/// The menu as shipped (ids, in order); the smoke host logs it.
pub const MENU_ITEMS: &str = "status,open,pause,separator,quit";

const ICON_SIZE: u32 = 32;
static ICON_RECORDING: &[u8] = include_bytes!("../assets/tray-recording.rgba");
static ICON_PAUSED: &[u8] = include_bytes!("../assets/tray-paused.rgba");

/// Commands emitted by the recording tray. Dispatch lives in the pump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingTrayCmd {
    Open,
    /// "Stop recording" (pause) / "Start recording" (resume).
    TogglePause,
    Quit,
}

/// RAII owner for the recording tray icon and menu.
pub struct RecordingTray {
    tray: TrayIcon,
    menu: Menu,
    status: MenuItem,
    pause: MenuItem,
    paused: bool,
    last_line: Option<String>,
}

impl RecordingTray {
    pub fn build() -> Result<Self, String> {
        let status = MenuItem::with_id(ID_STATUS, "Recall: starting", false, None);
        let open = MenuItem::with_id(ID_OPEN, "Open Recall", true, None);
        let pause = MenuItem::with_id(ID_PAUSE, pause_label(false), true, None);
        let quit = MenuItem::with_id(ID_QUIT, "Quit Recall", true, None);
        let menu = Menu::with_items(&[&status, &open, &pause, &PredefinedMenuItem::separator(), &quit])
            .map_err(|e| format!("build recording tray menu: {e}"))?;
        let tray = TrayIconBuilder::new()
            .with_tooltip("Recall: starting")
            .with_menu(Box::new(menu.clone()) as Box<dyn ContextMenu>)
            .with_icon(tray_icon(false)?)
            .build()
            .map_err(|e| format!("build recording tray icon: {e}"))?;
        Ok(Self { tray, menu, status, pause, paused: false, last_line: None })
    }

    /// Non-blocking drain of only this component's process-global menu events.
    pub fn poll(&self) -> Vec<RecordingTrayCmd> {
        let mut commands = Vec::new();
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if let Some(command) = map_id(event.id.as_ref()) {
                commands.push(command);
            }
        }
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if matches!(event, TrayIconEvent::DoubleClick { .. }) {
                commands.push(RecordingTrayCmd::Open);
            }
        }
        commands
    }

    /// Update the status line + tooltip (only on change).
    pub fn set_status(&mut self, line: &str) {
        if self.last_line.as_deref() == Some(line) {
            return;
        }
        self.status.set_text(line);
        let _ = self.tray.set_tooltip(Some(line));
        self.last_line = Some(line.to_string());
    }

    pub fn set_health(&mut self, health: EngineHealth) {
        self.set_status(&health_line(health));
    }

    /// Reflect the pause state: menu toggle text + icon (only on change).
    pub fn set_paused(&mut self, paused: bool) {
        if self.paused == paused {
            return;
        }
        self.paused = paused;
        self.pause.set_text(pause_label(paused));
        if let Ok(icon) = tray_icon(paused) {
            let _ = self.tray.set_icon(Some(icon));
        }
    }

    /// Used only by the no-capture smoke host; this is muda's real native menu.
    pub unsafe fn show_for_hwnd(&self, hwnd: HWND) {
        diagnostic(&format!("menu-open rect={:?}", self.diagnostic_icon_rect()));
        let _ = self.menu.show_context_menu_for_hwnd(hwnd.0 as isize, None);
    }

    pub fn diagnostic_icon_rect(&self) -> Option<String> {
        self.tray.rect().map(|r| {
            format!(
                "x={:.0},y={:.0},w={},h={}",
                r.position.x, r.position.y, r.size.width, r.size.height
            )
        })
    }
}

/// The pause toggle's text: what clicking it does.
pub fn pause_label(paused: bool) -> &'static str {
    if paused {
        "Start recording"
    } else {
        "Stop recording"
    }
}

fn map_id(id: &str) -> Option<RecordingTrayCmd> {
    Some(match id {
        ID_OPEN => RecordingTrayCmd::Open,
        ID_PAUSE => RecordingTrayCmd::TogglePause,
        ID_QUIT => RecordingTrayCmd::Quit,
        _ => return None,
    })
}

fn health_line(health: EngineHealth) -> String {
    match health {
        EngineHealth::Running { .. } => "Recall: replay recorder running".into(),
        EngineHealth::Restarting { attempt } => {
            format!("Recall: capture engine restarting (attempt {attempt})")
        }
        EngineHealth::GivenUp => "Recall: capture engine unavailable".into(),
        EngineHealth::Stopped => "Recall: recording stopped".into(),
    }
}

/// The Recall mark for the tray: (RGBA, width, height).
pub fn icon_rgba(paused: bool) -> (Vec<u8>, u32, u32) {
    let bytes = if paused { ICON_PAUSED } else { ICON_RECORDING };
    (bytes.to_vec(), ICON_SIZE, ICON_SIZE)
}

fn tray_icon(paused: bool) -> Result<Icon, String> {
    let (rgba, w, h) = icon_rgba(paused);
    Icon::from_rgba(rgba, w, h).map_err(|e| format!("build tray icon: {e}"))
}

/// Matches top-level windows owned by exact candidate executable paths, including hidden windows.
pub fn matching_windows(candidates: &[PathBuf]) -> Vec<(HWND, u32, PathBuf)> {
    let targets: Vec<PathBuf> = candidates
        .iter()
        .filter_map(|p| normalized_path(p))
        .collect();
    if targets.is_empty() {
        return Vec::new();
    }
    let mut scan = Scan {
        targets: &targets,
        found: Vec::new(),
    };
    let _ = unsafe {
        EnumWindows(
            Some(scan_window),
            LPARAM((&mut scan as *mut Scan<'_>) as isize),
        )
    };
    scan.found
}

struct Scan<'a> {
    targets: &'a [PathBuf],
    found: Vec<(HWND, u32, PathBuf)>,
}
unsafe extern "system" fn scan_window(hwnd: HWND, data: LPARAM) -> BOOL {
    let scan = unsafe { &mut *(data.0 as *mut Scan<'_>) };
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
    let extended = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    if !is_application_window(style, extended) {
        return BOOL(1);
    }
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    if let Some(path) = process_path(pid) {
        if scan.targets.iter().any(|target| *target == path) {
            scan.found.push((hwnd, pid, path));
        }
    }
    BOOL(1)
}

// GPUI marks normal windows APPWINDOW and popups TOOLWINDOW. The caption fallback
// includes standard Win32/Tauri windows while excluding IME/dispatcher helpers.
fn is_application_window(style: u32, extended: u32) -> bool {
    extended & WS_EX_TOOLWINDOW.0 == 0
        && (extended & WS_EX_APPWINDOW.0 != 0 || style & WS_CAPTION.0 != 0)
}

fn process_path(pid: u32) -> Option<PathBuf> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()? };
    let mut wide = vec![0u16; 32_768];
    let mut len = wide.len() as u32;
    let result = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(wide.as_mut_ptr()),
            &mut len,
        )
        .ok()
    };
    unsafe {
        let _ = CloseHandle(process);
    }
    result
        .map(|_| normalized_path(Path::new(&String::from_utf16_lossy(&wide[..len as usize]))))
        .flatten()
}

fn normalized_path(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok().map(|p| {
        let raw = p.to_string_lossy().replace('/', "\\");
        let raw = raw
            .strip_prefix(r"\\?\UNC\")
            .map(|rest| format!(r"\\{rest}"))
            .unwrap_or(raw);
        let raw = raw.strip_prefix(r"\\?\").map(str::to_owned).unwrap_or(raw);
        PathBuf::from(raw.to_ascii_lowercase())
    })
}

/// Best-effort line diagnostics for integration checks; absent unless explicitly requested.
pub fn diagnostic(line: &str) {
    let Some(path) = std::env::var_os("RECALL_TRAY_DIAGNOSTICS") else {
        return;
    };
    if let Some(parent) = Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}

/// Owns the bounded open/close operations for only Recall's native UI.
pub struct RecallUi {
    candidates: Vec<PathBuf>,
    launch_not_before: Option<Instant>,
    spawned: Option<std::process::Child>,
}
impl RecallUi {
    pub fn from_current_exe() -> Self {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("recall-hdr.exe"));
        let env_override = std::env::var_os("RECALL_NATIVE").map(PathBuf::from);
        Self {
            candidates: ui_candidates(&exe, env_override),
            launch_not_before: None,
            spawned: None,
        }
    }
    pub fn open(&mut self) -> Vec<(u32, PathBuf, bool)> {
        let found = matching_windows(&self.candidates);
        if !found.is_empty() {
            self.spawned = None;
            return found
                .into_iter()
                .map(|(hwnd, pid, path)| {
                    unsafe {
                        let _ = ShowWindow(hwnd, SW_RESTORE);
                        let _ = SetForegroundWindow(hwnd);
                    }
                    let visible = unsafe { IsWindowVisible(hwnd).as_bool() };
                    (pid, path, visible)
                })
                .collect();
        }
        if self
            .spawned
            .as_mut()
            .is_some_and(|child| child.try_wait().ok().flatten().is_none())
        {
            return Vec::new();
        }
        self.spawned = None;
        if self
            .launch_not_before
            .is_some_and(|until| Instant::now() < until)
        {
            return Vec::new();
        }
        if let Some(path) = self.candidates.iter().find(|p| p.is_file()) {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            if let Ok(child) = Command::new(path)
                .env_remove("RECALL_NATIVE_BACKGROUND")
                .creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS.0)
                .spawn()
            {
                self.spawned = Some(child);
                self.launch_not_before = Some(Instant::now() + Duration::from_secs(4));
            }
        }
        Vec::new()
    }
    pub fn close(&self) -> Vec<(u32, PathBuf, bool)> {
        matching_windows(&self.candidates)
            .into_iter()
            .map(|(hwnd, pid, path)| {
                let visible = unsafe { IsWindowVisible(hwnd).as_bool() };
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                (pid, path, visible)
            })
            .collect()
    }
}

/// The native UI executables "Open Recall" focuses or launches, in order:
/// 1. `RECALL_NATIVE` -- an explicit path to `recall-native.exe`;
/// 2. `recall-native.exe` next to the controller exe (installed layout);
/// 3. `<repo>/target-gpui/debug/recall-native.exe`;
/// 4. `<repo>/target-gpui/release/recall-native.exe`,
/// where `<repo>` is the nearest ancestor of the controller exe holding both
/// `Cargo.toml` and `recall-hdr/Cargo.toml`. The first that exists is
/// launched; a running window of ANY of them is focused instead.
fn ui_candidates(exe: &Path, env_override: Option<PathBuf>) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = env_override.into_iter().collect();
    let Some(directory) = exe.parent() else {
        return candidates;
    };
    candidates.push(directory.join("recall-native.exe"));
    if let Some(root) = directory.ancestors().find(|root| {
        root.join("Cargo.toml").is_file() && root.join("recall-hdr/Cargo.toml").is_file()
    }) {
        candidates.extend([
            root.join("target-gpui/debug/recall-native.exe"),
            root.join("target-gpui/release/recall-native.exe"),
        ]);
    }
    let mut unique = Vec::new();
    for candidate in candidates {
        if !unique.contains(&candidate) {
            unique.push(candidate);
        }
    }
    unique
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_ids_map_only_to_recording_commands() {
        assert_eq!(map_id(ID_OPEN), Some(RecordingTrayCmd::Open));
        assert_eq!(map_id(ID_PAUSE), Some(RecordingTrayCmd::TogglePause));
        assert_eq!(map_id(ID_QUIT), Some(RecordingTrayCmd::Quit));
        assert_eq!(map_id(ID_STATUS), None);
        assert_eq!(map_id("record_save"), None, "Save replay now left the tray");
        assert_eq!(map_id("record_autostart"), None, "Start with Windows left the tray");
        assert_eq!(MENU_ITEMS, "status,open,pause,separator,quit");
    }
    #[test]
    fn pause_toggle_says_what_a_click_does() {
        assert_eq!(pause_label(false), "Stop recording");
        assert_eq!(pause_label(true), "Start recording");
    }
    #[test]
    fn tray_icons_are_the_committed_32px_rgba_marks() {
        for paused in [false, true] {
            let (rgba, w, h) = icon_rgba(paused);
            assert_eq!((w, h), (32, 32));
            assert_eq!(rgba.len(), (w * h * 4) as usize);
            assert!(rgba.chunks_exact(4).any(|p| p[3] == 255), "has opaque pixels");
            assert!(rgba.chunks_exact(4).any(|p| p[3] == 0), "has a transparent background");
        }
        assert_ne!(icon_rgba(false).0, icon_rgba(true).0, "paused looks different");
        // Recording dot is red, paused dot is grey (the centre pixel).
        let centre = |paused| {
            let (rgba, w, h) = icon_rgba(paused);
            let i = ((h / 2 * w + w / 2) * 4) as usize;
            [rgba[i], rgba[i + 1], rgba[i + 2]]
        };
        let [r, g, b] = centre(false);
        assert!(r > 200 && g < 120 && b < 120, "recording centre {:?}", centre(false));
        let [r, g, b] = centre(true);
        assert!(r.abs_diff(g) < 20 && g.abs_diff(b) < 20, "paused centre is grey {:?}", centre(true));
    }
    #[test]
    fn candidate_paths_are_native_only_in_documented_order() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let exe = root.join("target-dev/debug/recall-hdr.exe");
        let pinned = PathBuf::from(r"D:\Apps\recall-native.exe");
        let candidates = ui_candidates(&exe, Some(pinned.clone()));
        assert_eq!(
            candidates,
            vec![
                pinned,
                root.join("target-dev/debug/recall-native.exe"),
                root.join("target-gpui/debug/recall-native.exe"),
                root.join("target-gpui/release/recall-native.exe"),
            ]
        );
        assert!(ui_candidates(&exe, None).iter().all(|p| p.ends_with("recall-native.exe")), "never the Hub");
    }

    #[test]
    fn window_filter_restores_main_windows_without_revealing_helpers() {
        assert!(is_application_window(0, WS_EX_APPWINDOW.0));
        assert!(is_application_window(WS_CAPTION.0, 0));
        assert!(!is_application_window(0, 0));
        assert!(!is_application_window(WS_CAPTION.0, WS_EX_TOOLWINDOW.0));
    }

    #[test]
    fn normalization_requires_real_exact_path() {
        assert!(normalized_path(Path::new("this-does-not-exist.exe")).is_none());
    }
}
