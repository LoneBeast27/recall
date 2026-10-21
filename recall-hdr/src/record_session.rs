//! `--record-tray-smoke`: the recording tray + its real native menu on a small
//! host window, with NO capture, hotkeys, supervisor, overlay, display code,
//! registry writes or GPU work. The live record mode is `record_app`.
use crate::record_tray::{diagnostic, RecallUi, RecordingTray, RecordingTrayCmd};
use crate::supervisor::EngineHealth;
use std::time::{Duration, Instant};
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, PeekMessageW, PostMessageW,
    PostQuitMessage, RegisterClassW, ShowWindow, TranslateMessage, HMENU, MSG, PM_REMOVE,
    SW_SHOWNOACTIVATE, WM_APP, WM_COMMAND, WM_DESTROY, WM_QUIT, WNDCLASSW, WS_CHILD,
    WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

const MENU_ITEMS: &str = "status,open,save,autostart,quit";

/// Smoke-only dispatch: Open is real (focus/launch the native UI); Save and
/// Start-with-Windows are logged, never executed. Returns `true` on Quit.
fn dispatch_tray_command(command: RecordingTrayCmd, ui: &mut RecallUi, scope: &str) -> bool {
    diagnostic(&format!("{scope} command={command:?}"));
    match command {
        RecordingTrayCmd::Open => {
            let matched = ui.open();
            diagnostic(&format!("{scope} command=open matched={matched:?}"));
            crate::log!("{scope}: tray open, matched UI windows={}", matched.len());
            false
        }
        RecordingTrayCmd::SaveNow => {
            crate::log!("{scope}: tray save (smoke: no engine, nothing saved)");
            false
        }
        RecordingTrayCmd::SetAutostart(on) => {
            crate::log!("{scope}: tray start-with-windows -> {on} (smoke: registry untouched)");
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
    let mut tray = match RecordingTray::build(false) {
        Ok(value) => value,
        Err(e) => {
            crate::log!("record-tray-smoke: tray unavailable: {e}");
            return 2;
        }
    };
    tray.set_health(EngineHealth::Stopped);
    diagnostic(&format!("smoke iconcreated=true menuitems={MENU_ITEMS} rect={:?}", tray.diagnostic_icon_rect()));
    crate::log!("record-tray-smoke: iconcreated=true menuitems={MENU_ITEMS} rect={:?}", tray.diagnostic_icon_rect());
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
    diagnostic(&format!("smoke shutdown={shutdown_reason} ui-close matched={matched:?}"));
    crate::log!("record-tray-smoke: shutdown={shutdown_reason}, requested WM_CLOSE for {} Recall UI windows", matched.len());
    if shutdown_reason != "window-close" {
        unsafe {
            let _ = DestroyWindow(host);
        }
    }
    0
}
