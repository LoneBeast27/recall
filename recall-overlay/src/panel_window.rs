//! The island editor's panel window: a layered, never-activated, topmost
//! tool popup owned by the island window, excluded from capture. Unlike the
//! guide it takes the mouse (its buttons are clicked); the game keeps the
//! keyboard focus. It exists only while the editor runs: made with the
//! guide by `spot_edit::begin`, destroyed with it by `spot_edit::end`
//! (every exit path) and on drop.

use std::sync::OnceLock;
use std::time::Instant;

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindow, LoadCursorW, PostMessageW, RegisterClassW,
    SetWindowDisplayAffinity, ShowWindow, UpdateLayeredWindowIndirect, GW_OWNER, HTCLIENT, IDC_ARROW, MA_NOACTIVATE,
    SW_SHOWNOACTIVATE, ULW_ALPHA, UPDATELAYEREDWINDOWINFO, WDA_EXCLUDEFROMCAPTURE, WM_LBUTTONDOWN, WM_MOUSEACTIVATE,
    WM_MOUSEMOVE, WM_LBUTTONUP, WM_NCHITTEST, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};

use crate::fonts::Fonts;
use crate::guide_render::GuideRenderer;
use crate::panel::{Action, PanelScene};

/// The extended styles: layered, never activated, no taskbar button, topmost (not click-through).
pub const STYLE: u32 = WS_EX_LAYERED.0 | WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0;

extern "system" fn panel_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_NCHITTEST => LRESULT(HTCLIENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // Hover and pressed looks: the button under the pointer lights up.
        WM_MOUSEMOVE | WM_LBUTTONUP => {
            if message == WM_MOUSEMOVE {
                let mut track = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                unsafe {
                    let _ = TrackMouseEvent(&mut track);
                }
            }
            let held = message == WM_MOUSEMOVE && wparam.0 & 0x0001 != 0;
            crate::with_app(|app| app.panel_pointer(Some(point(lparam)), held));
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            crate::with_app(|app| app.panel_pointer(None, false));
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = point(lparam);
            let island = unsafe { GetWindow(hwnd, GW_OWNER) }.unwrap_or_default();
            crate::with_app(|app| app.panel_pointer(Some((x, y)), true));
            // The press may end the editor, which destroys this window: nothing
            // of it is touched after the call.
            if crate::with_app(|app| app.panel_press(island, (x, y), Instant::now())) == Some(true) && !island.is_invalid() {
                unsafe {
                    let _ = PostMessageW(Some(island), crate::WM_FEED, WPARAM(0), LPARAM(0));
                }
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// `WM_MOUSELEAVE` (WinUser.h; the crate keeps it under `UI::Controls`).
const WM_MOUSELEAVE: u32 = 0x02A3;

/// A mouse message's client point.
fn point(lparam: LPARAM) -> (f32, f32) {
    ((lparam.0 & 0xffff) as i16 as f32, ((lparam.0 >> 16) & 0xffff) as i16 as f32)
}

fn register() -> Result<(), String> {
    static CLASS: OnceLock<Result<(), String>> = OnceLock::new();
    CLASS
        .get_or_init(|| unsafe {
            let module = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(panel_proc),
                hInstance: HINSTANCE(module.0),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: w!("RecallEditorPanel"),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 { Err("RegisterClassW (panel) failed".into()) } else { Ok(()) }
        })
        .clone()
}

pub struct PanelWindow {
    pub hwnd: HWND,
    renderer: GuideRenderer,
    /// The scene on screen and its top-left (screen px).
    drawn: Option<(PanelScene, (i32, i32))>,
    /// The button under the pointer, and whether it is pressed.
    hot: Option<(Action, bool)>,
    shown: bool,
    pub draws: u32,
}

impl PanelWindow {
    /// Made hidden, owned by `island` (so it stays above it); it shows on
    /// its first [`PanelWindow::update`]. A window that cannot be excluded
    /// from capture is destroyed again: the panel never reaches a recording.
    /// `exclude` is false only in a `--smoke-allow-capture` look check.
    pub fn create(island: HWND, exclude: bool) -> Result<Self, String> {
        register()?;
        let module = unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None) }.map_err(|e| format!("GetModuleHandleW: {e}"))?;
        let renderer = GuideRenderer::new().map_err(|e| format!("panel Direct2D: {e}"))?;
        let owner = (!island.is_invalid()).then_some(island);
        let hwnd = unsafe {
            CreateWindowExW(
                windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(STYLE),
                w!("RecallEditorPanel"),
                w!("Recall island editor"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                owner,
                None,
                Some(HINSTANCE(module.0)),
                None,
            )
        }
        .map_err(|e| format!("CreateWindowExW (panel): {e}"))?;
        let excluded = if exclude { unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) } } else { Ok(()) };
        if let Err(error) = excluded {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return Err(format!("panel capture exclusion failed: {error}"));
        }
        Ok(Self { hwnd, renderer, drawn: None, hot: None, shown: false, draws: 0 })
    }

    /// The scene on screen and its top-left (screen px).
    pub fn drawn(&self) -> Option<(&PanelScene, (i32, i32))> {
        self.drawn.as_ref().map(|(scene, at)| (scene, *at))
    }

    /// The button under `at` (window px) in the scene on screen.
    pub fn hit(&self, at: (f32, f32)) -> Option<Action> {
        self.drawn.as_ref().and_then(|(scene, _)| scene.hit(at))
    }

    /// Shows `scene` at `origin` (screen px): drawn again only when it
    /// changed, moved only when the origin did.
    pub fn update(&mut self, fonts: &Fonts, scene: PanelScene, origin: (i32, i32)) -> Result<(), String> {
        let same_scene = self.drawn.as_ref().is_some_and(|(s, _)| *s == scene);
        let same_spot = self.drawn.as_ref().is_some_and(|(_, at)| *at == origin);
        if !same_scene {
            self.renderer.draw_panel(fonts, &scene, self.hot).map_err(|e| format!("panel draw: {e}"))?;
            self.draws += 1;
        }
        if !(same_scene && same_spot) {
            self.present(origin, (scene.w, scene.h))?;
            self.drawn = Some((scene, origin));
        }
        if !self.shown {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
            self.shown = true;
        }
        Ok(())
    }

    /// The button under the pointer (and pressed) changed: drawn again now.
    pub fn set_hot(&mut self, fonts: &Fonts, hot: Option<(Action, bool)>) -> Result<(), String> {
        if hot == self.hot {
            return Ok(());
        }
        self.hot = hot;
        let Some((scene, origin)) = self.drawn.as_ref() else { return Ok(()) };
        self.renderer.draw_panel(fonts, scene, hot).map_err(|e| format!("panel draw: {e}"))?;
        self.draws += 1;
        self.present(*origin, (scene.w, scene.h))
    }

    /// What is lit now (tests and diagnostics).
    pub fn hot(&self) -> Option<(Action, bool)> {
        self.hot
    }

    fn present(&self, (x, y): (i32, i32), (w, h): (i32, i32)) -> Result<(), String> {
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        let (dst, size, src) = (POINT { x, y }, SIZE { cx: w, cy: h }, POINT { x: 0, y: 0 });
        let info = UPDATELAYEREDWINDOWINFO {
            cbSize: std::mem::size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
            hdcDst: Default::default(),
            pptDst: &dst,
            psize: &size,
            hdcSrc: self.renderer.dc,
            pptSrc: &src,
            crKey: COLORREF(0),
            pblend: &blend,
            dwFlags: ULW_ALPHA,
            prcDirty: std::ptr::null(),
        };
        if unsafe { UpdateLayeredWindowIndirect(self.hwnd, &info) }.as_bool() {
            Ok(())
        } else {
            Err(format!("UpdateLayeredWindowIndirect (panel): {}", windows::core::Error::from_thread().message()))
        }
    }
}

impl Drop for PanelWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, IsWindow, IsWindowVisible, GWL_EXSTYLE};
    use windows::Win32::UI::WindowsAndMessaging::WS_EX_TRANSPARENT;

    /// Made hidden, takes the mouse (no click-through), never activated,
    /// gone once dropped. Never shown.
    #[test]
    fn the_panel_window_takes_clicks_without_focus_and_is_gone_after_drop() {
        let panel = PanelWindow::create(HWND::default(), true).expect("panel window");
        let hwnd = panel.hwnd;
        assert!(unsafe { IsWindow(Some(hwnd)) }.as_bool());
        assert!(!unsafe { IsWindowVisible(hwnd) }.as_bool(), "hidden until its first update");
        let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
        assert_eq!(ex & STYLE, STYLE, "layered, no-activate, tool window, topmost: {ex:#x}");
        assert_eq!(ex & WS_EX_TRANSPARENT.0, 0, "the panel's buttons take clicks");
        assert_eq!(panel.hit((1.0, 1.0)), None, "nothing drawn, nothing to hit");
        drop(panel);
        assert!(!unsafe { IsWindow(Some(hwnd)) }.as_bool(), "destroyed on drop");
    }
}
