//! Apps for the Audio page's app sources: the apps that have a sound session on any active
//! output right now, with their names and icons, and the entry for an exe picked by hand.
//! Listed while the Audio page is open, on a thread of its own (COM lives and dies there).
//! Nothing here runs before first paint.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use gpui::RenderImage;

use windows::Win32::Foundation::{CloseHandle, S_OK};
use windows::Win32::Media::Audio::{
    AudioSessionStateExpired, DEVICE_STATE_ACTIVE, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator,
    eRender,
};
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows::core::{HSTRING, Interface, PWSTR};

/// Longest track name the Audio page stores (the controller refuses more).
pub const LABEL_MAX: usize = 64;

/// An app as Settings shows and stores it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayingApp {
    /// Image file name, e.g. `player.exe`.
    pub exe: String,
    /// The app's own name (its file description), else the exe name without `.exe`.
    pub label: String,
    /// Full path of the exe ("" when unknown).
    pub path: String,
}

/// The playing apps and an icon per exe (lowercase), including the saved app sources that run.
#[derive(Clone, Default)]
pub struct AppList {
    pub apps: Vec<PlayingApp>,
    pub icons: HashMap<String, Arc<RenderImage>>,
}

/// `list_blocking` plus icons for those apps and for the running exes in `saved`. Blocks: call
/// from a background task.
pub fn list_with_icons_blocking(saved: &[String]) -> AppList {
    let apps = list_blocking();
    let mut icons = HashMap::new();
    let mut paths: Vec<(String, String)> = apps.iter().map(|a| (a.exe.to_lowercase(), a.path.clone())).collect();
    for exe in saved {
        let key = exe.trim().to_lowercase();
        if !paths.iter().any(|(e, _)| *e == key)
            && let Some(path) = crate::services::process_tree::tree_root(exe).and_then(image_path)
        {
            paths.push((key, path));
        }
    }
    for (exe, path) in paths.into_iter().filter(|(_, p)| !p.is_empty()) {
        if let Some(icon) = crate::services::app_icons::icon_for(Path::new(&path)) {
            icons.insert(exe, icon);
        }
    }
    AppList { apps, icons }
}

/// Apps with a sound session on an active output, by name, without Recall itself. Blocks:
/// call from a background task, never the UI thread.
pub fn list_blocking() -> Vec<PlayingApp> {
    let own = std::process::id();
    let worker = std::thread::Builder::new().name("recall-audio-apps".into()).spawn(move || {
        // SAFETY: COM is initialised and torn down on this thread only.
        let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let pids = session_pids().unwrap_or_default();
        if com.is_ok() {
            unsafe { CoUninitialize() };
        }
        apps_for(pids.into_iter().filter(|&pid| pid != 0 && pid != own))
    });
    worker.ok().and_then(|w| w.join().ok()).unwrap_or_default()
}

/// Process IDs of the live, non-system sound sessions on every active output.
fn session_pids() -> windows::core::Result<Vec<u32>> {
    // SAFETY: plain COM calls on an initialised MTA thread; every interface is owned.
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        let mut pids = Vec::new();
        for d in 0..devices.GetCount()? {
            let Ok(manager) = devices.Item(d).and_then(|device| device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None)) else { continue };
            let Ok(sessions) = manager.GetSessionEnumerator() else { continue };
            for s in 0..sessions.GetCount().unwrap_or(0) {
                let Ok(session) = sessions.GetSession(s) else { continue };
                if session.GetState().is_ok_and(|state| state == AudioSessionStateExpired) {
                    continue;
                }
                let Ok(session) = session.cast::<IAudioSessionControl2>() else { continue };
                if session.IsSystemSoundsSession() == S_OK {
                    continue;
                }
                if let Ok(pid) = session.GetProcessId() {
                    pids.push(pid);
                }
            }
        }
        Ok(pids)
    }
}

/// One entry per exe (case-insensitive), sorted by name.
fn apps_for(pids: impl Iterator<Item = u32>) -> Vec<PlayingApp> {
    let mut apps: Vec<PlayingApp> = Vec::new();
    for pid in pids {
        let Some(path) = image_path(pid) else { continue };
        let app = app_from_path(Path::new(&path));
        if !apps.iter().any(|a| a.exe.eq_ignore_ascii_case(&app.exe)) {
            apps.push(app);
        }
    }
    apps.sort_by_key(|a| a.label.to_lowercase());
    apps
}

/// Full path of a process's exe.
pub(crate) fn image_path(pid: u32) -> Option<String> {
    // SAFETY: the handle is checked, used for one query and closed.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = [0u16; 1024];
        let mut size = buffer.len() as u32;
        let queried = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut size);
        let _ = CloseHandle(process);
        queried.ok()?;
        Some(String::from_utf16_lossy(&buffer[..size as usize]))
    }
}

/// The entry Settings stores for an exe on disk: its file name and display name.
pub fn app_from_path(path: &Path) -> PlayingApp {
    let exe = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let label = file_description(path).unwrap_or_else(|| exe_stem(&exe));
    PlayingApp { exe, label: clip_label(&label), path: path.to_string_lossy().into_owned() }
}

/// `player.exe` → `player`.
pub fn exe_stem(exe: &str) -> String {
    let n = exe.len().saturating_sub(4);
    if exe.len() > 4 && exe[n..].eq_ignore_ascii_case(".exe") { exe[..n].to_owned() } else { exe.to_owned() }
}

/// One line, at most `LABEL_MAX` characters.
pub fn clip_label(label: &str) -> String {
    let one_line: String = label.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    one_line.trim().chars().take(LABEL_MAX).collect::<String>().trim_end().to_owned()
}

/// The `FileDescription` from an exe's version resource (its first language).
fn file_description(path: &Path) -> Option<String> {
    let name = HSTRING::from(path.as_os_str());
    // SAFETY: the buffer is sized by GetFileVersionInfoSizeW; VerQueryValueW returns pointers
    // into it that are read within their reported lengths while it is alive.
    unsafe {
        let size = GetFileVersionInfoSizeW(&name, None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(&name, None, size, data.as_mut_ptr().cast()).ok()?;
        let (mut pointer, mut length) = (std::ptr::null_mut(), 0u32);
        if !VerQueryValueW(data.as_ptr().cast(), &HSTRING::from(r"\VarFileInfo\Translation"), &mut pointer, &mut length).as_bool() || length < 4 {
            return None;
        }
        let pair = std::slice::from_raw_parts(pointer as *const u16, 2);
        let key = format!(r"\StringFileInfo\{:04x}{:04x}\FileDescription", pair[0], pair[1]);
        if !VerQueryValueW(data.as_ptr().cast(), &HSTRING::from(key), &mut pointer, &mut length).as_bool() || length == 0 {
            return None;
        }
        let text = std::slice::from_raw_parts(pointer as *const u16, length as usize);
        let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
        let description = String::from_utf16_lossy(&text[..end]).trim().to_owned();
        (!description.is_empty()).then_some(description)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_one_short_line_and_fall_back_to_the_exe() {
        assert_eq!(exe_stem("player.EXE"), "player");
        assert_eq!(exe_stem("tool"), "tool");
        assert_eq!(clip_label("  Two\nlines  "), "Two lines");
        assert_eq!(clip_label(&"x".repeat(80)).chars().count(), LABEL_MAX);
    }

    #[test]
    fn an_exe_reads_its_own_display_name() {
        // The test binary has no version resource; a Windows exe always does.
        let own = std::env::current_exe().unwrap();
        assert_eq!(app_from_path(&own).label, exe_stem(&own.file_name().unwrap().to_string_lossy()));
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        let notepad = Path::new(&windir).join("System32").join("notepad.exe");
        if notepad.exists() {
            let app = app_from_path(&notepad);
            assert_eq!(app.exe.to_lowercase(), "notepad.exe");
            assert_ne!(app.label, "notepad", "read from the version resource, not the file name");
        }
    }

    /// Prints what the Audio page would show on this PC: every output and input, the defaults,
    /// the apps with a sound session (with the pid of each session and the tree root an app
    /// source captures), and 2 s of process-loopback peak per app. Reads only.
    #[test]
    #[ignore = "live hardware: lists this PC's endpoints and sessions, opens process loopback"]
    fn live_inventory() {
        if !crate::hw_gate::enabled("live_inventory") {
            return;
        }
        let lists = crate::services::audio_devices::enumerate_blocking();
        eprintln!("default output: {:?}\ndefault input: {:?}", lists.default_output, lists.default_input);
        lists.outputs.iter().for_each(|n| eprintln!("output: {n}"));
        lists.inputs.iter().for_each(|n| eprintln!("input: {n}"));
        let _ = wasapi::initialize_mta();
        let pids = session_pids().unwrap_or_default();
        for pid in &pids {
            eprintln!("session pid {pid}: {:?}", image_path(*pid));
        }
        for app in list_blocking() {
            let root = crate::services::process_tree::tree_root(&app.exe);
            eprintln!("app: {} ({}) tree root {root:?} peak {:?}", app.label, app.exe, root.map(loopback_peak));
        }
    }

    /// Peak sample over 2 s of process loopback on a tree, or the error text.
    fn loopback_peak(pid: u32) -> Result<f32, String> {
        use wasapi::{AudioClient, Direction, SampleType, StreamMode, WaveFormat};
        let text = |e: wasapi::WasapiError| e.to_string();
        let mut client = AudioClient::new_application_loopback_client(pid, true).map_err(text)?;
        let format = WaveFormat::new(32, 32, &SampleType::Float, 48000, 2, None);
        client.initialize_client(&format, &Direction::Capture, &StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 200_000 }).map_err(text)?;
        let event = client.set_get_eventhandle().map_err(text)?;
        let capture = client.get_audiocaptureclient().map_err(text)?;
        client.start_stream().map_err(text)?;
        let (mut raw, mut peak) = (std::collections::VecDeque::new(), 0.0f32);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < until {
            if event.wait_for_event(200).is_ok() {
                capture.read_from_device_to_deque(&mut raw).map_err(text)?;
                let full = raw.len() - raw.len() % 4;
                let bytes: Vec<u8> = raw.drain(..full).collect();
                peak = bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]).abs()).fold(peak, f32::max);
            }
        }
        let _ = client.stop_stream();
        Ok(peak)
    }

    #[test]
    fn listing_playing_apps_never_lists_recall_itself() {
        let own = std::env::current_exe().unwrap().file_name().unwrap().to_string_lossy().to_lowercase();
        assert!(list_blocking().iter().all(|app| app.exe.to_lowercase() != own));
    }
}
