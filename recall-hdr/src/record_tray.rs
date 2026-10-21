//! The record mode's notification-area UI: a status line ("Recording VALORANT"
//! / "Waiting for a game"), Open Recall (focus or launch `recall-native.exe`),
//! Save replay now, Start with Windows, Quit Recall. It performs none of the
//! actions itself -- [`RecordingTray::poll`] returns commands for the pump.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use tray_icon::menu::{CheckMenuItem, ContextMenu, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
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
const ID_SAVE: &str = "record_save";
const ID_AUTOSTART: &str = "record_autostart";
const ID_QUIT: &str = "record_quit";

/// Commands emitted by the recording tray. Dispatch lives in the pump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingTrayCmd {
    Open,
    SaveNow,
    /// "Start with Windows" toggled to this (post-click) value.
    SetAutostart(bool),
    Quit,
}

/// RAII owner for the recording tray icon and menu.
pub struct RecordingTray {
    tray: TrayIcon,
    menu: Menu,
    status: MenuItem,
    autostart: CheckMenuItem,
    last_line: Option<String>,
}

impl RecordingTray {
    pub fn build(autostart_on: bool) -> Result<Self, String> {
        let status = MenuItem::with_id(ID_STATUS, "Recall: starting", false, None);
        let open = MenuItem::with_id(ID_OPEN, "Open Recall", true, None);
        let save = MenuItem::with_id(ID_SAVE, "Save replay now", true, None);
        let autostart = CheckMenuItem::with_id(ID_AUTOSTART, "Start with Windows", true, autostart_on, None);
        let quit = MenuItem::with_id(ID_QUIT, "Quit Recall", true, None);
        let menu = Menu::with_items(&[&status, &open, &save, &autostart, &PredefinedMenuItem::separator(), &quit])
            .map_err(|e| format!("build recording tray menu: {e}"))?;
        let (rgba, width, height) = recording_icon_rgba();
        let icon = Icon::from_rgba(rgba, width, height)
            .map_err(|e| format!("build recording icon: {e}"))?;
        let tray = TrayIconBuilder::new()
            .with_tooltip("Recall: starting")
            .with_menu(Box::new(menu.clone()) as Box<dyn ContextMenu>)
            .with_icon(icon)
            .build()
            .map_err(|e| format!("build recording tray icon: {e}"))?;
        Ok(Self { tray, menu, status, autostart, last_line: None })
    }

    /// Non-blocking drain of only this component's process-global menu events.
    pub fn poll(&self) -> Vec<RecordingTrayCmd> {
        let mut commands = Vec::new();
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            // muda flips a CheckMenuItem before emitting, so is_checked() is
            // the post-click value.
            if let Some(command) = map_id(event.id.as_ref(), self.autostart.is_checked()) {
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

    /// Reflect the config's autostart flag (e.g. changed from the app).
    pub fn set_autostart_checked(&self, on: bool) {
        if self.autostart.is_checked() != on {
            self.autostart.set_checked(on);
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

fn map_id(id: &str, autostart_now: bool) -> Option<RecordingTrayCmd> {
    Some(match id {
        ID_OPEN => RecordingTrayCmd::Open,
        ID_SAVE => RecordingTrayCmd::SaveNow,
        ID_AUTOSTART => RecordingTrayCmd::SetAutostart(autostart_now),
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

const ICON_SIZE: u32 = 32;
/// A monochrome white `R` mark with transparent surroundings.
pub fn recording_icon_rgba() -> (Vec<u8>, u32, u32) {
    let mut rgba = vec![0; (ICON_SIZE * ICON_SIZE * 4) as usize];
    for y in 5..27 {
        for x in 6..10 {
            set_pixel(&mut rgba, x, y);
        }
    }
    for x in 9..23 {
        set_pixel(&mut rgba, x, 5);
        set_pixel(&mut rgba, x, 15);
    }
    for y in 6..15 {
        for x in 20..24 {
            set_pixel(&mut rgba, x, y);
        }
    }
    for p in 0..14 {
        set_pixel(&mut rgba, 10 + p, 15 + p);
    }
    (rgba, ICON_SIZE, ICON_SIZE)
}
fn set_pixel(rgba: &mut [u8], x: u32, y: u32) {
    let i = ((y * ICON_SIZE + x) * 4) as usize;
    rgba[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
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
        assert_eq!(map_id(ID_OPEN, false), Some(RecordingTrayCmd::Open));
        assert_eq!(map_id(ID_SAVE, false), Some(RecordingTrayCmd::SaveNow));
        assert_eq!(map_id(ID_AUTOSTART, true), Some(RecordingTrayCmd::SetAutostart(true)));
        assert_eq!(map_id(ID_AUTOSTART, false), Some(RecordingTrayCmd::SetAutostart(false)));
        assert_eq!(map_id(ID_QUIT, false), Some(RecordingTrayCmd::Quit));
        assert_eq!(map_id(ID_STATUS, false), None);
    }
    #[test]
    fn icon_buffer_is_rgba_and_monochrome() {
        let (rgba, w, h) = recording_icon_rgba();
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        for p in rgba.chunks_exact(4) {
            assert!(p == [0, 0, 0, 0] || p == [255, 255, 255, 255]);
        }
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
