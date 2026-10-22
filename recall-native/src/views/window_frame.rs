//! Window frame behaviour for the custom title bar: minimize / maximize / close buttons, drag
//! to move (with Windows snap), double-click to maximize.
//!
//! GPUI's own caption handling (`window_control_area`) answers Windows' hit test from the last
//! mouse position it processed, not from the point Windows asks about. A quick move between
//! buttons then reports the old button (maximize clicks minimized), and right after a maximize a
//! stale "maximize" answer covered the whole title bar, so the next click toggled it again. The
//! title bar is therefore ordinary client area: clicks act on their real position, and a drag
//! hands over to the system move loop once the pointer has moved a few pixels with the button
//! held (posting the move for a plain click would leave the window stuck to the cursor).
use gpui::{App, Context, MouseButton, MouseDownEvent, MouseMoveEvent, Stateful, Div, Window, div, prelude::*, px};
use windows::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    UI::{
        Input::KeyboardAndMouse::ReleaseCapture,
        WindowsAndMessaging::{IsZoomed, PostMessageW, SC_MOVE, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, ShowWindowAsync, WM_CLOSE, WM_SYSCOMMAND},
    },
};

use crate::{
    root::Root,
    theme::{self, c},
    ui::{Icon, icon},
    views::titlebar::TITLEBAR_H,
};

/// Pointer travel (logical px) before a press on the title bar becomes a window drag.
const DRAG_SLOP: f32 = 4.0;
/// `wParam` low bits for "move by caption" in `WM_SYSCOMMAND`.
const HTCAPTION: usize = 2;

#[derive(Clone, Copy)]
pub enum Caption {
    Minimize,
    Maximize,
    Close,
}

fn hwnd(window: &Window) -> Option<HWND> {
    crate::root::parent_hwnd(window).ok().map(|raw| HWND(raw as *mut _))
}

pub fn toggle_maximize(window: &Window) {
    let Some(hwnd) = hwnd(window) else { return };
    unsafe {
        let command = if IsZoomed(hwnd).as_bool() { SW_RESTORE } else { SW_MAXIMIZE };
        let _ = ShowWindowAsync(hwnd, command);
    }
}

fn act(caption: Caption, window: &Window) {
    let Some(hwnd) = hwnd(window) else { return };
    unsafe {
        match caption {
            Caption::Minimize => {
                let _ = ShowWindowAsync(hwnd, SW_MINIMIZE);
            }
            Caption::Maximize => toggle_maximize(window),
            Caption::Close => {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }
}

/// Starts the system move loop (posted, so it runs after GPUI's input dispatch returns).
fn start_move(window: &Window) {
    let Some(hwnd) = hwnd(window) else { return };
    unsafe {
        let _ = ReleaseCapture();
        let _ = PostMessageW(Some(hwnd), WM_SYSCOMMAND, WPARAM(SC_MOVE as usize | HTCAPTION), LPARAM(0));
    }
}

/// A 46 px caption button that acts on click at its real position.
pub fn caption_button(id: &'static str, kind: Icon, caption: Caption) -> Stateful<Div> {
    let close = matches!(caption, Caption::Close);
    div()
        .id(id)
        .w(px(46.))
        .h(px(TITLEBAR_H))
        .flex()
        .items_center()
        .justify_center()
        .occlude()
        .hover(move |s| if close { s.bg(c(0xc42b1c)) } else { s.bg(c(theme::OVERLAY)) })
        .child(icon(kind, 12., theme::MUTED))
        .on_click(move |_, window: &mut Window, _: &mut App| act(caption, window))
}

impl Root {
    /// Press on an empty part of the title bar: double-click toggles maximize; otherwise
    /// remember where, so a drag can start once the pointer moves.
    pub fn title_down(&mut self, event: &MouseDownEvent, window: &mut Window, _: &mut Context<Self>) {
        if event.click_count >= 2 {
            self.title_drag = None;
            toggle_maximize(window);
        } else {
            self.title_drag = Some(event.position);
        }
    }

    pub fn title_move(&mut self, event: &MouseMoveEvent, window: &mut Window, _: &mut Context<Self>) {
        let Some(origin) = self.title_drag else { return };
        if event.pressed_button != Some(MouseButton::Left) {
            self.title_drag = None;
            return;
        }
        let (dx, dy) = (f32::from(event.position.x - origin.x), f32::from(event.position.y - origin.y));
        if dx.abs() > DRAG_SLOP || dy.abs() > DRAG_SLOP {
            self.title_drag = None;
            start_move(window);
        }
    }
}

/// Gives the window a solid DWM backdrop in `rgb`. GPUI's window has no surface of its own
/// (everything is a composition layer above the native video), so while a resize waits for the
/// next frame, the new strip, and any part of the video cut-out the video has not repainted
/// yet, showed the desktop behind the window.
pub fn solid_backdrop(window: &Window, rgb: u32) {
    #[repr(C)]
    struct AccentPolicy {
        state: u32,
        flags: u32,
        color: u32,
        animation: u32,
    }
    #[repr(C)]
    struct CompositionData {
        attribute: u32,
        data: *mut core::ffi::c_void,
        size: usize,
    }
    type SetCompositionAttribute = unsafe extern "system" fn(HWND, *mut CompositionData) -> windows::core::BOOL;
    const ACCENT_ENABLE_GRADIENT: u32 = 1;
    const WCA_ACCENT_POLICY: u32 = 19;
    let Some(hwnd) = hwnd(window) else { return };
    unsafe {
        let Ok(user32) = windows::Win32::System::LibraryLoader::GetModuleHandleW(windows::core::w!("user32.dll")) else { return };
        let Some(address) = windows::Win32::System::LibraryLoader::GetProcAddress(user32, windows::core::s!("SetWindowCompositionAttribute")) else { return };
        let set: SetCompositionAttribute = std::mem::transmute(address);
        let (r, g, b) = ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
        let mut accent = AccentPolicy { state: ACCENT_ENABLE_GRADIENT, flags: 2, color: 0xff00_0000 | (b << 16) | (g << 8) | r, animation: 0 };
        let mut data = CompositionData { attribute: WCA_ACCENT_POLICY, data: (&mut accent as *mut AccentPolicy).cast(), size: std::mem::size_of::<AccentPolicy>() };
        let _ = set(hwnd, &mut data);
    }
}
