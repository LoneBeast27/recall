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

/// The live window as the editor sees it.
struct Window(HWND);

impl EditWindow for Window {
    fn set_click_through(&mut self, on: bool) {
        unsafe {
            let style = GetWindowLongPtrW(self.0, GWL_EXSTYLE);
            let bit = WS_EX_TRANSPARENT.0 as isize;
            SetWindowLongPtrW(self.0, GWL_EXSTYLE, if on { style | bit } else { style & !bit });
        }
    }

    fn release_capture(&mut self) {
        if unsafe { GetCapture() } == self.0 {
            unsafe {
                let _ = ReleaseCapture();
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
        let refused = refusal(self.dynamic(), has_game, game_in_front(session.target_pid), exclusive_fullscreen());
        match begin(&mut Window(hwnd), refused, &key, self.hud_cache.spot(&key), now) {
            Ok(editor) => {
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
        let line = match end(&mut Window(hwnd), &mut editor, how) {
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
        let in_front = game_in_front(self.reducer.session().target_pid);
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
        if editor.drag(cursor(), shown, &placed.monitor, SNAP * placed.scale, now) {
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

    /// The placement follows the editor's spot.
    fn sync_placed(&mut self) {
        let (Some(editor), Some(placed)) = (self.editor.as_ref(), self.placed.as_mut()) else { return };
        placed.custom = editor.spot;
        placed.spot = Spot { anchor: editor.spot.map_or(placed.spot.anchor, |s| s.anchor()), free_px: None };
    }

    /// Looks the placement up again on the next frame (the spot changed).
    fn replace_island(&mut self) {
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

    #[test]
    fn the_hint_names_the_mouse_only_controls() {
        assert_eq!(HINT_DETAIL, "Drag to place · right-click cancel · ⟲ auto");
        assert!(HINT_DETAIL.ends_with(RESET_LABEL), "the button is the end of the line");
    }
}
