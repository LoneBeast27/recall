//! The island editor on the live window. The shortcut (a `session` event
//! `{"type":"island_editor"}` from the controller) starts it for the game in
//! front, in Dynamic mode only, and never over exclusive fullscreen. While it
//! runs the window takes the mouse (no `WS_EX_TRANSPARENT`; still
//! `WS_EX_NOACTIVATE`, so the game keeps the keyboard focus) and the pill
//! says what to do. It holds no keyboard keys. Every exit goes through
//! [`end`], which puts the window back to click-through.

use std::time::Instant;

use serde_json::Value;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetCapture, ReleaseCapture};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_TRANSPARENT};

use super::App;
use crate::guide;
use crate::guide_window::GuideWindow;
use crate::panel_window::PanelWindow;
use crate::island::Content;
use crate::layout::{Measure, Role};
use crate::notice::{NoticeView, Tone};
use crate::placement::Spot;
use crate::spot_edit::{begin, end, refusal, EditKey, EditWindow, SNAP};
use crate::zorder::{exclusive_fullscreen, game_in_front};

pub const HINT_TITLE: &str = "Island editor";
pub const HINT_DETAIL: &str = "Drag to place · right-click cancel · ⟲ auto";
/// The reset button: the end of the detail line, under the pointer.
pub const RESET_LABEL: &str = "⟲ auto";
/// The notice's room after its text (`layout::notice_scene`'s `pad_r`, 1440p px).
const PAD_R: f32 = 20.0;

/// The controller's editor shortcut event.
pub fn is_toggle(value: &Value) -> bool {
    value.get("mod").and_then(Value::as_str) == Some("session")
        && value.get("event").and_then(|e| e.get("type")).and_then(Value::as_str) == Some("island_editor")
}

/// The live window as the editor sees it, and the guide grid's and the
/// panel's slots.
pub(super) struct Window<'a> {
    hwnd: HWND,
    guide: &'a mut Option<GuideWindow>,
    panel: &'a mut Option<PanelWindow>,
    /// Why the grid or the panel could not be made (the editor runs without them).
    error: Option<String>,
}

impl<'a> Window<'a> {
    pub(super) fn new(hwnd: HWND, guide: &'a mut Option<GuideWindow>, panel: &'a mut Option<PanelWindow>) -> Self {
        Self { hwnd, guide, panel, error: None }
    }
}

impl EditWindow for Window<'_> {
    fn set_click_through(&mut self, on: bool) {
        unsafe {
            let style = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            let bit = WS_EX_TRANSPARENT.0 as isize;
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, if on { style | bit } else { style & !bit });
        }
    }

    fn release_capture(&mut self) {
        if unsafe { GetCapture() } == self.hwnd {
            unsafe {
                let _ = ReleaseCapture();
            }
        }
    }

    fn show_guide(&mut self, on: bool) {
        *self.guide = None;
        *self.panel = None;
        if on {
            match GuideWindow::create() {
                Ok(guide) => *self.guide = Some(guide),
                Err(error) => self.error = Some(error),
            }
            match PanelWindow::create(self.hwnd) {
                Ok(panel) => *self.panel = Some(panel),
                Err(error) => self.error = Some(self.error.take().map_or(error.clone(), |e| format!("{e}; {error}"))),
            }
        }
    }
}

fn cursor() -> (f32, f32) {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    (p.x as f32, p.y as f32)
}

/// True when `(x, y)` (screen px) is on the reset button: the last `zone` px
/// of the window `[l, t, r, b]` along its long side (the end of the detail line).
pub fn on_reset([l, t, r, b]: [i32; 4], (x, y): (f32, f32), zone: f32) -> bool {
    let inside = x >= l as f32 && x < r as f32 && y >= t as f32 && y < b as f32;
    let tall = b - t > r - l;
    inside && if tall { y >= b as f32 - zone } else { x >= r as f32 - zone }
}

impl App {
    pub fn editing(&self) -> bool {
        self.editor.is_some()
    }

    /// The pill's text while the editor runs (over anything else).
    pub(super) fn editor_content(&self) -> Option<Content> {
        self.editor.as_ref().map(|_| Content::Notice(NoticeView::new(Tone::Neutral, HINT_TITLE, HINT_DETAIL, false)))
    }

    /// The shortcut from the controller: toggle, then answer it on the pipe
    /// (`island_editor_ack`; it beeps on a refusal, or when an older
    /// overlay never answers).
    pub(super) fn answer_editor_toggle(&mut self, hwnd: HWND, now: Instant) -> bool {
        let was_on = self.editor.is_some();
        let changed = self.toggle_editor(hwnd, now);
        let started = was_on || self.editor.is_some();
        let line = format!("{{\"cmd\":\"island_editor_ack\",\"started\":{started}}}\n");
        if let Ok(mut slot) = self.out.lock() {
            if let Some(pipe) = slot.as_mut() {
                let _ = std::io::Write::write_all(pipe, line.as_bytes());
            }
        }
        changed
    }

    /// The shortcut: start for the game in front, or (running) leave without
    /// writing anything more (spots saved by releases stay).
    pub(super) fn toggle_editor(&mut self, hwnd: HWND, now: Instant) -> bool {
        if self.editor.is_some() {
            return self.finish_editor(hwnd, EditKey::Leave);
        }
        let session = self.reducer.session();
        let key = session.hud_key();
        let has_game = !session.desktop && !key.is_empty();
        let refused = refusal(self.dynamic(), has_game, game_in_front(session.target_pid, session.target_hwnd), exclusive_fullscreen());
        let mut window = Window::new(hwnd, &mut self.guide, &mut self.panel);
        let begun = begin(&mut window, refused, &key, self.hud_cache.spot(&key), now);
        if let Some(error) = window.error.take() {
            self.trace.note("guide", format!("no grid or panel: {error}"));
        }
        match begun {
            Ok(editor) => {
                let editor = editor.with_size(self.hud_cache.size(&key));
                self.editor = Some(editor);
                self.trace.note("editor", format!("on for {key}"));
                self.replace_island();
                true
            }
            Err(why) => {
                self.trace.note("editor", format!("refused: {why}"));
                false
            }
        }
    }

    /// Leaves the editor; `how` decides what is written for the game.
    pub fn finish_editor(&mut self, hwnd: HWND, how: EditKey) -> bool {
        let Some(mut editor) = self.editor.take() else { return false };
        let written = end(&mut Window::new(hwnd, &mut self.guide, &mut self.panel), &mut editor, how);
        if let Some(size) = editor.finish_size(how) {
            self.hud_cache.set_size(&editor.key, size);
        }
        let line = match written {
            Some(spot) => {
                self.hud_cache.set_spot(&editor.key, spot);
                match spot {
                    Some(spot) => format!("{how:?}: put back {:?} for {}", spot.edge, editor.key),
                    None => format!("{how:?}: automatic for {}", editor.key),
                }
            }
            None => format!("{how:?}: nothing more written for {}", editor.key),
        };
        self.trace.note("editor", line);
        self.replace_island();
        true
    }

    /// Each tick: leaves without saving after a minute idle (a held button
    /// that never came up too), two minutes in all, when the game loses
    /// focus, or when a drag lost the mouse capture.
    pub(super) fn watch_editor(&mut self, hwnd: HWND, now: Instant) -> bool {
        let Some(editor) = self.editor.as_ref() else { return false };
        let session = self.reducer.session();
        let in_front = game_in_front(session.target_pid, session.target_hwnd);
        let captured = unsafe { GetCapture() } == hwnd;
        match editor.watch(now, in_front, captured) {
            Some(how) => self.finish_editor(hwnd, how),
            None => false,
        }
    }

    /// The left button went down on the pill: the reset button ends the
    /// editor (back to automatic); anywhere else starts a drag. True when
    /// the window should take the mouse capture.
    pub fn editor_press(&mut self, hwnd: HWND, now: Instant) -> bool {
        let [l, t, r, b] = self.bounds;
        let Some(scale) = self.editor.as_ref().map(|_| self.placed.as_ref().map_or(1.0, |p| p.scale)) else { return false };
        let zone = (self.fonts.measure(Role::NoticeUi, scale, RESET_LABEL).width + PAD_R * scale + 8.0).ceil();
        let at = cursor();
        if on_reset(self.bounds, at, zone) {
            self.finish_editor(hwnd, EditKey::Reset);
            return false;
        }
        let Some(editor) = self.editor.as_mut() else { return false };
        editor.press(at, ((l + r) as f32 / 2.0, (t + b) as f32 / 2.0), now);
        true
    }

    /// The cursor moved. `held`: the left button is down (`MK_LBUTTON`); a
    /// move without it means the release never reached the window, and the
    /// drag is dropped (nothing saved). A held pill follows the cursor.
    pub fn editor_drag(&mut self, hwnd: HWND, held: bool, now: Instant) {
        if !held {
            if self.editor.as_mut().is_some_and(|e| e.drop_drag()) {
                self.trace.note("editor", "drag dropped: the button came up unseen");
                self.sync_placed();
                self.refresh(hwnd, now);
            }
            return;
        }
        let [l, t, r, b] = self.bounds;
        let (Some(editor), Some(placed)) = (self.editor.as_mut(), self.placed.as_mut()) else { return };
        let shown = ((r - l) as f32, (b - t) as f32);
        if editor.drag(cursor(), shown, &placed.monitor, &placed.area, SNAP * placed.scale, now) {
            self.sync_placed();
            self.refresh(hwnd, now);
        }
    }

    /// The left button came up: the dragged spot is saved for the game.
    pub fn editor_release(&mut self, now: Instant) {
        let Some(editor) = self.editor.as_mut() else { return };
        let Some(spot) = editor.release(now) else { return };
        let key = editor.key.clone();
        self.hud_cache.set_spot(&key, Some(spot));
        self.trace.note("editor", format!("saved {:?} for {key}", spot.edge));
    }

    /// Right-click on the pill: the old spot comes back and the editor ends.
    pub fn editor_cancel(&mut self, hwnd: HWND) -> bool {
        self.editor.is_some() && self.finish_editor(hwnd, EditKey::Cancel)
    }

    /// The mouse capture went elsewhere: a drag in progress ends the editor
    /// (nothing more written).
    pub fn editor_capture_lost(&mut self, hwnd: HWND) -> bool {
        self.editor.as_ref().is_some_and(|e| e.dragging()) && self.finish_editor(hwnd, EditKey::Leave)
    }

    /// The guide grid over the placement area: drawn whole once per size or
    /// when a centre line lights or dims, else only the readout moves.
    pub(super) fn update_guide(&mut self, hwnd: HWND) {
        let (Some(grid), Some(placed), Some(editor)) = (self.guide.as_mut(), self.placed.as_ref(), self.editor.as_ref()) else { return };
        let area = placed.area;
        let lines = guide::lines_of(editor.spot, &placed.monitor, &area);
        let [l, t, r, b] = self.bounds;
        let pill = (r > l && b > t).then(|| {
            let centre = ((l + r) as f32 / 2.0, (t + b) as f32 / 2.0);
            ([l - area.left, t - area.top, r - area.left, b - area.top], guide::readout(centre, &area))
        });
        let (_, (w, h)) = guide::frame(&area);
        let scene = guide::scene(w, h, placed.scale, lines, pill, &self.fonts);
        if let Err(error) = grid.update(hwnd, &area, &self.fonts, scene) {
            self.trace.note("guide", error);
        }
    }

    /// The placement follows the editor's spot.
    pub(super) fn sync_placed(&mut self) {
        let (Some(editor), Some(placed)) = (self.editor.as_ref(), self.placed.as_mut()) else { return };
        placed.custom = editor.spot;
        placed.spot = Spot { anchor: editor.spot.map_or(placed.spot.anchor, |s| s.anchor()), free_px: None };
    }

    /// Looks the placement up again on the next frame (the spot changed).
    pub(super) fn replace_island(&mut self) {
        if let Some(p) = self.placed.as_mut() {
            p.pid = None;
            p.hwnd = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reset_button_is_the_end_of_the_pill() {
        let wide = [100, 10, 500, 60];
        assert!(on_reset(wide, (480.0, 30.0), 60.0) && !on_reset(wide, (420.0, 30.0), 60.0));
        assert!(!on_reset(wide, (520.0, 30.0), 60.0), "outside the window");
        let tall = [2500, 400, 2550, 900];
        assert!(on_reset(tall, (2520.0, 880.0), 60.0) && !on_reset(tall, (2520.0, 600.0), 60.0), "a side pill: its bottom end");
    }

    /// The live window side: the grid's and the panel's real windows are made
    /// on start and destroyed on every way out (never shown: it is drawn only on a frame).
    #[test]
    fn every_exit_path_destroys_the_real_grid_window() {
        use crate::spot_edit::EditKey;
        use windows::Win32::UI::WindowsAndMessaging::IsWindow;
        let t0 = Instant::now();
        for how in [EditKey::Leave, EditKey::Cancel, EditKey::Reset] {
            let (mut slot, mut panel) = (None, None);
            let mut editor = begin(&mut Window::new(HWND::default(), &mut slot, &mut panel), None, "valorant", None, t0).unwrap();
            let grid = slot.as_ref().map(|g| g.hwnd).expect("the grid is made on start");
            let buttons = panel.as_ref().map(|p| p.hwnd).expect("the panel is made on start");
            assert!(unsafe { IsWindow(Some(grid)) }.as_bool() && unsafe { IsWindow(Some(buttons)) }.as_bool());
            end(&mut Window::new(HWND::default(), &mut slot, &mut panel), &mut editor, how);
            assert!(slot.is_none() && !unsafe { IsWindow(Some(grid)) }.as_bool(), "{how:?}: the grid window is destroyed");
            assert!(panel.is_none() && !unsafe { IsWindow(Some(buttons)) }.as_bool(), "{how:?}: the panel window is destroyed");
        }
    }

    #[test]
    fn the_hint_names_the_mouse_only_controls() {
        assert_eq!(HINT_DETAIL, "Drag to place · right-click cancel · ⟲ auto");
        assert!(HINT_DETAIL.ends_with(RESET_LABEL), "the button is the end of the line");
    }
}
