//! Recording-only controller: no display enumeration, HDR switching, clamp or restore.
use crate::record_tray::{diagnostic, RecallUi, RecordingTray, RecordingTrayCmd};
use crate::supervisor::EngineHealth;
use crate::{control::PipeHub, hotkey::Hotkey, supervisor::Supervisor};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentProcess, SetPriorityClass, SetProcessAffinityMask, BELOW_NORMAL_PRIORITY_CLASS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, PeekMessageW, PostMessageW,
    PostQuitMessage, RegisterClassW, ShowWindow, TranslateMessage, HMENU, MSG, PM_REMOVE,
    SW_SHOWNOACTIVATE, WM_APP, WM_COMMAND, WM_DESTROY, WM_QUIT, WNDCLASSW, WS_CHILD,
    WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

pub fn run() -> i32 {
    let mask = std::env::var("RECALL_CPU_MASK").unwrap_or_else(|_| "ffff0000".into());
    let mask = match usize::from_str_radix(mask.trim_start_matches("0x"), 16) {
        Ok(m) if m != 0 => m,
        _ => {
            crate::log!("record-only: invalid CPU mask");
            return 2;
        }
    };
    if let Err(e) = unsafe { SetProcessAffinityMask(GetCurrentProcess(), mask) } {
        crate::log!("record-only: affinity failed: {e}");
        return 2;
    }
    let _ = unsafe { SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS) };
    if std::env::var_os("RECALL_BUFFER_SECS").is_none() {
        std::env::set_var("RECALL_BUFFER_SECS", "120");
    }
    let save = std::env::var("RECALL_SAVE_HOTKEY").unwrap_or_else(|_| "Alt+N".into());
    let mic = std::env::var("RECALL_MIC_HOTKEY").unwrap_or_else(|_| "Alt+M".into());
    // This helper registers two generic accelerators; no HDR action is called here.
    let (hotkeys, warning) = match Hotkey::register(&mic, Some(&save)) {
        Ok(v) => v,
        Err(e) => {
            crate::log!("record-only: hotkey failed: {e}");
            return 2;
        }
    };
    if let Some(w) = warning {
        crate::log!("record-only: save hotkey failed: {w}");
        return 2;
    }
    crate::log!("record-only: affinity=0x{mask:x}; save={save} (120s); microphone={mic}; display controls disabled");
    let (tx, rx) = mpsc::channel();
    let hub = PipeHub::start(tx);
    let supervisor = Supervisor::start();
    supervisor.attach_hub(hub.clone());
    let mut tray = match RecordingTray::build() {
        Ok(tray) => tray,
        Err(e) => {
            crate::log!("record-only: tray unavailable: {e}");
            supervisor.shutdown(Duration::from_secs(4));
            return 2;
        }
    };
    diagnostic(&format!(
        "iconcreated=true menuitems=status,open,hide,quit rect={:?}",
        tray.diagnostic_icon_rect()
    ));
    let mut ui = RecallUi::from_current_exe();
    let mut last_save: Option<Instant> = None;
    let mut last_mic: Option<Instant> = None;
    let mut message = MSG::default();
    let shutdown_reason = loop {
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            if message.message == WM_QUIT {
                break;
            }
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        if message.message == WM_QUIT {
            break "wm-quit";
        }
        tray.set_health(supervisor.health());
        if tray
            .poll()
            .into_iter()
            .any(|command| dispatch_tray_command(command, &mut ui, "record-only"))
        {
            break "menu-quit";
        }
        let mut control_quit = false;
        while let Ok(line) = rx.try_recv() {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            match value["cmd"].as_str(){
                Some("quit")=>{control_quit=true;break;},
                Some("save_clip"|"export_clip"|"set_mic_mute"|"toggle_mic_mute")=>supervisor.send_engine_line(&line),
                Some("status")=>hub.event("session",&serde_json::json!({"type":"status","record_only":true,"cpu_mask":format!("0x{mask:x}"),"save_hotkey":save,"save_seconds":120,"mic_hotkey":mic}).to_string()),
                _=>{},
            }
        }
        if control_quit {
            break "control-quit";
        }
        let fired = hotkeys.poll();
        if fired.save_clip && last_save.is_none_or(|t| t.elapsed() >= Duration::from_secs(2)) {
            last_save = Some(Instant::now());
            supervisor.send_engine_line(r#"{"cmd":"save_clip","seconds":120}"#);
            crate::log!("record-only: Alt+N -> save last 120 seconds");
        }
        if fired.toggle_hdr && last_mic.is_none_or(|t| t.elapsed() >= Duration::from_millis(250)) {
            last_mic = Some(Instant::now());
            supervisor.send_engine_line(r#"{"cmd":"toggle_mic_mute"}"#);
            crate::log!("record-only: Alt+M -> toggle recording microphone");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let matched = ui.close();
    diagnostic(&format!(
        "shutdown={shutdown_reason} ui-close matched={matched:?} supervisor=4s"
    ));
    crate::log!(
        "record-only: shutdown={shutdown_reason}, requested WM_CLOSE for {} Recall UI windows",
        matched.len()
    );
    supervisor.shutdown(Duration::from_secs(4));
    diagnostic("shutdown=supervisor-complete");
    crate::log!("record-only: clean shutdown");
    0
}

fn dispatch_tray_command(command: RecordingTrayCmd, ui: &mut RecallUi, scope: &str) -> bool {
    diagnostic(&format!("{scope} command={command:?}"));
    match command {
        RecordingTrayCmd::Open => {
            let matched = ui.open();
            diagnostic(&format!("{scope} command=open matched={matched:?}"));
            crate::log!("{scope}: tray open, matched UI windows={}", matched.len());
            false
        }
        RecordingTrayCmd::Hide => {
            let matched = ui.hide();
            diagnostic(&format!("{scope} command=hide matched={matched:?}"));
            crate::log!("{scope}: tray hide, matched UI windows={}", matched.len());
            false
        }
        RecordingTrayCmd::Quit => true,
    }
}

const SMOKE_BUTTON_ID: isize = 71;
const SMOKE_SHOW_MENU: u32 = WM_APP + 1;

unsafe extern "system" fn smoke_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_COMMAND if (wparam.0 & 0xffff) == SMOKE_BUTTON_ID as usize => {
            // BUTTON sends WM_COMMAND synchronously; relay it into the queued pump
            // where the live RecordingTray owner can show muda's actual menu.
            let _ = unsafe { PostMessageW(Some(hwnd), SMOKE_SHOW_MENU, WPARAM(0), LPARAM(0)) };
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// Native tray/menu smoke seam. It never starts capture, hotkeys, the supervisor,
/// overlay, display code, or GPU work; it times out after 180 seconds.
pub fn run_tray_smoke() -> i32 {
    let mut tray = match RecordingTray::build() {
        Ok(value) => value,
        Err(e) => {
            crate::log!("record-tray-smoke: tray unavailable: {e}");
            return 2;
        }
    };
    tray.set_health(EngineHealth::Stopped);
    diagnostic(&format!(
        "smoke iconcreated=true menuitems=status,open,hide,quit rect={:?}",
        tray.diagnostic_icon_rect()
    ));
    crate::log!(
        "record-tray-smoke: iconcreated=true menuitems=status,open,hide,quit rect={:?}",
        tray.diagnostic_icon_rect()
    );
    let instance = match unsafe { GetModuleHandleW(None) } {
        Ok(value) => value,
        Err(e) => {
            crate::log!("record-tray-smoke: module handle failed: {e}");
            return 2;
        }
    };
    let class_name = w!("RecallTraySmokeHost");
    let class = WNDCLASSW {
        lpfnWndProc: Some(smoke_window_proc),
        hInstance: instance.into(),
        lpszClassName: class_name,
        ..Default::default()
    };
    if unsafe { RegisterClassW(&class) } == 0 {
        crate::log!("record-tray-smoke: register host class failed");
        return 2;
    }
    let host = match unsafe {
        CreateWindowExW(
            Default::default(),
            class_name,
            w!("Recall tray check"),
            WS_OVERLAPPEDWINDOW,
            80,
            80,
            300,
            130,
            None,
            None,
            Some(instance.into()),
            None,
        )
    } {
        Ok(value) => value,
        Err(e) => {
            crate::log!("record-tray-smoke: host failed: {e}");
            return 2;
        }
    };
    let button = unsafe {
        CreateWindowExW(
            Default::default(),
            w!("BUTTON"),
            w!("Tray menu"),
            WS_CHILD | WS_VISIBLE,
            90,
            45,
            110,
            32,
            Some(host),
            Some(HMENU(SMOKE_BUTTON_ID as *mut _)),
            None,
            None,
        )
    };
    if button.is_err() {
        unsafe {
            let _ = DestroyWindow(host);
        }
        crate::log!("record-tray-smoke: button failed");
        return 2;
    }
    unsafe {
        let _ = ShowWindow(host, SW_SHOWNOACTIVATE);
    }
    let mut ui = RecallUi::from_current_exe();
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut message = MSG::default();
    let shutdown_reason = loop {
        if Instant::now() >= deadline {
            break "timeout";
        }
        let mut window_closed = false;
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            if message.message == WM_QUIT {
                window_closed = true;
                break;
            }
            if message.hwnd == host && message.message == SMOKE_SHOW_MENU {
                unsafe { tray.show_for_hwnd(host) };
                continue;
            }
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        if window_closed {
            break "window-close";
        }
        if tray
            .poll()
            .into_iter()
            .any(|command| dispatch_tray_command(command, &mut ui, "smoke"))
        {
            break "menu-quit";
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let matched = ui.close();
    diagnostic(&format!(
        "smoke shutdown={shutdown_reason} ui-close matched={matched:?}"
    ));
    crate::log!("record-tray-smoke: shutdown={shutdown_reason}, requested WM_CLOSE for {} Recall UI windows",matched.len());
    if shutdown_reason != "window-close" {
        unsafe {
            let _ = DestroyWindow(host);
        }
    }
    0
}
