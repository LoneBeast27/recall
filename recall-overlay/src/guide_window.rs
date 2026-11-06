//! The island editor's guide window: a second layered, click-through,
//! no-activate, topmost tool popup over the placement area, excluded from
//! capture like the island and kept just below it in z-order. It exists
//! only while the editor runs: made by `spot_edit::begin`, destroyed by
//! `spot_edit::end` (every exit path) and on drop.

use std::sync::OnceLock;

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindow, RegisterClassW, SetWindowDisplayAffinity, SetWindowPos,
    ShowWindow, UpdateLayeredWindowIndirect, GW_HWNDNEXT, HTTRANSPARENT, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER,
    SWP_NOSIZE, SW_SHOWNOACTIVATE, ULW_ALPHA, UPDATELAYEREDWINDOWINFO, WDA_EXCLUDEFROMCAPTURE, WM_NCHITTEST, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::fonts::Fonts;
use crate::guide::{frame, plan, GuideScene, Redraw};
use crate::guide_render::GuideRenderer;
use crate::placement::Rect;

/// The extended styles: layered, click-through, never activated, no taskbar button, topmost.
pub const STYLE: u32 = WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0;

extern "system" fn guide_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn register() -> Result<(), String> {
    static CLASS: OnceLock<Result<(), String>> = OnceLock::new();
    CLASS
        .get_or_init(|| unsafe {
            let module = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(guide_proc),
                hInstance: HINSTANCE(module.0),
                lpszClassName: w!("RecallEditorGuide"),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 { Err("RegisterClassW (guide) failed".into()) } else { Ok(()) }
        })
        .clone()
}

pub struct GuideWindow {
    pub hwnd: HWND,
    renderer: GuideRenderer,
    /// The scene on screen, and the area it covers.
    drawn: Option<(GuideScene, Rect)>,
    shown: bool,
    /// Whole-window draws so far (the cost check: once per size or highlight change).
    pub full_draws: u32,
    pub part_draws: u32,
}

impl GuideWindow {
    /// Made hidden; it shows on its first [`GuideWindow::update`]. A window
    /// that cannot be excluded from capture is destroyed again: the grid is
    /// never drawn where a recording could see it.
    pub fn create() -> Result<Self, String> {
        register()?;
        let module = unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None) }.map_err(|e| format!("GetModuleHandleW: {e}"))?;
        let renderer = GuideRenderer::new().map_err(|e| format!("guide Direct2D: {e}"))?;
        let hwnd = unsafe {
            CreateWindowExW(
                windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(STYLE),
                w!("RecallEditorGuide"),
                w!("Recall editor guide"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(HINSTANCE(module.0)),
                None,
            )
        }
        .map_err(|e| format!("CreateWindowExW (guide): {e}"))?;
        if let Err(error) = unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) } {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return Err(format!("guide capture exclusion failed: {error}"));
        }
        Ok(Self { hwnd, renderer, drawn: None, shown: false, full_draws: 0, part_draws: 0 })
    }

    /// Shows `scene` over `area` (screen px), just below `island`: the whole
    /// window on a new size, area or highlight, else only the readout's boxes.
    pub fn update(&mut self, island: HWND, area: &Rect, fonts: &Fonts, scene: GuideScene) -> Result<Redraw, String> {
        let redraw = match self.drawn.as_ref() {
            Some((_, at)) if at != area => Redraw::Full,
            drawn => plan(drawn.map(|(s, _)| s), &scene),
        };
        let clip = match redraw {
            Redraw::None => None,
            Redraw::Full => Some(None),
            Redraw::Part(rect) => Some(Some(rect)),
        };
        if let Some(clip) = clip {
            self.renderer.draw(fonts, &scene, clip).map_err(|e| format!("guide draw: {e}"))?;
            self.present(area, clip)?;
            match clip {
                None => self.full_draws += 1,
                Some(_) => self.part_draws += 1,
            }
            self.drawn = Some((scene, *area));
        }
        if !self.shown {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
            self.shown = true;
        }
        self.keep_below(island);
        Ok(redraw)
    }

    fn present(&self, area: &Rect, dirty: Option<[i32; 4]>) -> Result<(), String> {
        let ((x, y), (w, h)) = frame(area);
        debug_assert_eq!((w, h), self.renderer.size, "the drawn scene is the area's size");
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        let (dst, size, src) = (POINT { x, y }, SIZE { cx: w, cy: h }, POINT { x: 0, y: 0 });
        let rect = dirty.map(|[left, top, right, bottom]| RECT { left, top, right, bottom });
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
            prcDirty: rect.as_ref().map_or(std::ptr::null(), |r| r as *const RECT),
        };
        if unsafe { UpdateLayeredWindowIndirect(self.hwnd, &info) }.as_bool() {
            Ok(())
        } else {
            Err(format!("UpdateLayeredWindowIndirect (guide): {}", windows::core::Error::from_thread().message()))
        }
    }

    /// The guide sits directly below the island (both topmost); it is put
    /// back only when something came between them.
    pub fn keep_below(&self, island: HWND) {
        if island.is_invalid() || unsafe { GetWindow(island, GW_HWNDNEXT) }.ok() == Some(self.hwnd) {
            return;
        }
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(island), 0, 0, 0, 0, flags);
        }
    }

}

impl Drop for GuideWindow {
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

    /// Made hidden with the island's styles, gone once dropped. Never shown.
    #[test]
    fn the_guide_window_is_click_through_hidden_until_drawn_and_gone_after_drop() {
        let guide = GuideWindow::create().expect("guide window");
        let hwnd = guide.hwnd;
        assert!(unsafe { IsWindow(Some(hwnd)) }.as_bool());
        assert!(!unsafe { IsWindowVisible(hwnd) }.as_bool(), "hidden until its first update");
        let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
        assert_eq!(ex & STYLE, STYLE, "layered, transparent, no-activate, tool window, topmost: {ex:#x}");
        drop(guide);
        assert!(!unsafe { IsWindow(Some(hwnd)) }.as_bool(), "destroyed on drop");
    }
}
