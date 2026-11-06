//! The island editor on the live window. The shortcut (a `session` event
//! `{"type":"island_editor"}` from the controller) starts it for the game in
//! front, in Dynamic mode only. While it runs the window takes the mouse
//! (no `WS_EX_TRANSPARENT`; still `WS_EX_NOACTIVATE`, so the game keeps the
//! keyboard focus), the overlay holds Enter / Esc / R as hotkeys, and the
//! pill says what to do. Leaving puts the window back to click-through.

use std::time::Instant;

use serde_json::Value;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, ReleaseCapture, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_R,
    VK_RETURN, VK_RIGHT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_TRANSPARENT};

use super::App;
use crate::island::Content;
use crate::notice::{NoticeView, Tone};
use crate::placement::Spot;
use crate::spot_edit::{EditKey, Editor, NUDGE, SNAP};
use crate::zorder::game_in_front;

/// Hotkey ids the overlay holds only while the editor runs.
const HELD: [(i32, u16, EditKey); 3] = [
    (0xE501, VK_RETURN.0, EditKey::Save),
    (0xE502, VK_ESCAPE.0, EditKey::Cancel),
    (0xE503, VK_R.0, EditKey::Reset),
];

/// The arrow keys, held (with key repeat) only while editing: they move the
/// pill when the game owns the cursor (exclusive fullscreen).
const ARROWS: [(i32, u16, (f32, f32)); 4] = [
    (0xE511, VK_LEFT.0, (-1.0, 0.0)),
    (0xE512, VK_RIGHT.0, (1.0, 0.0)),
    (0xE513, VK_UP.0, (0.0, -1.0)),
    (0xE514, VK_DOWN.0, (0.0, 1.0)),
];

pub const HINT_TITLE: &str = "Drag or use arrow keys";
pub const HINT_DETAIL: &str = "Enter save · Esc cancel · R reset to auto";

/// The controller's editor shortcut event.
pub fn is_toggle(value: &Value) -> bool {
    value.get("mod").and_then(Value::as_str) == Some("session")
        && value.get("event").and_then(|e| e.get("type")).and_then(Value::as_str) == Some("island_editor")
}

/// The arrow a `WM_HOTKEY` id stands for (its direction).
pub fn held_arrow(id: i32) -> Option<(f32, f32)> {
    ARROWS.iter().find(|(held, _, _)| *held == id).map(|(_, _, dir)| *dir)
}

/// The editor key a `WM_HOTKEY` id stands for.
pub fn held_key(id: i32) -> Option<EditKey> {
    HELD.iter().find(|(held, _, _)| *held == id).map(|(_, _, key)| *key)
}

fn set_click_through(hwnd: HWND, on: bool) {
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let bit = WS_EX_TRANSPARENT.0 as isize;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, if on { style | bit } else { style & !bit });
    }
}

fn cursor() -> (f32, f32) {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    (p.x as f32, p.y as f32)
}

impl App {
    pub fn editing(&self) -> bool {
        self.editor.is_some()
    }

    /// The pill's text while the editor runs (over anything else).
    pub(super) fn editor_content(&self) -> Option<Content> {
        self.editor.as_ref().map(|_| Content::Notice(NoticeView::new(Tone::Neutral, HINT_TITLE, HINT_DETAIL, false)))
    }

    /// The shortcut: start for the game in front, or (running) save and leave.
    pub(super) fn toggle_editor(&mut self, hwnd: HWND, now: Instant) -> bool {
        if self.editor.is_some() {
            return self.finish_editor(hwnd, EditKey::Save);
        }
        let session = self.reducer.session();
        let key = session.hud_key();
        let refused = if !self.dynamic() {
            Some("the island is not on Dynamic")
        } else if session.desktop || key.is_empty() {
            Some("no game target")
        } else if !game_in_front(session.target_pid) {
            Some("the game is not in front")
        } else {
            None
        };
        if let Some(why) = refused {
            self.trace.note("editor", format!("refused: {why}"));
            return false;
        }
        self.editor = Some(Editor::new(&key, self.hud_cache.spot(&key), now));
        set_click_through(hwnd, false);
        let keys = HELD.iter().map(|&(id, vk, _)| (id, vk, MOD_NOREPEAT));
        let arrows = ARROWS.iter().map(|&(id, vk, _)| (id, vk, HOT_KEY_MODIFIERS(0)));
        for (id, vk, modifiers) in keys.chain(arrows) {
            if let Err(error) = unsafe { RegisterHotKey(Some(hwnd), id, modifiers, u32::from(vk)) } {
                self.trace.note("editor", format!("key {vk:#x} not held: {error}"));
            }
        }
        self.trace.note("editor", format!("on for {key}"));
        self.replace_island();
        true
    }

    /// Leaves the editor; `key` decides what is written for the game.
    pub fn finish_editor(&mut self, hwnd: HWND, key: EditKey) -> bool {
        let Some(editor) = self.editor.take() else { return false };
        for id in HELD.iter().map(|k| k.0).chain(ARROWS.iter().map(|k| k.0)) {
            unsafe {
                let _ = UnregisterHotKey(Some(hwnd), id);
            }
        }
        unsafe {
            let _ = ReleaseCapture();
        }
        set_click_through(hwnd, true);
        let line = match editor.finish(key) {
            Some(spot) => {
                self.hud_cache.set_spot(&editor.key, spot);
                match spot {
                    Some(spot) => format!("saved {:?} for {}", spot.edge, editor.key),
                    None => format!("reset to automatic for {}", editor.key),
                }
            }
            None => format!("cancelled for {}", editor.key),
        };
        self.trace.note("editor", line);
        self.replace_island();
        true
    }

    /// Leaves without saving after a minute idle or when the game loses focus.
    pub(super) fn watch_editor(&mut self, hwnd: HWND, now: Instant) -> bool {
        let Some(editor) = self.editor.as_ref() else { return false };
        let target = self.reducer.session().target_pid;
        if editor.timed_out(now) || (!editor.dragging() && !game_in_front(target)) {
            return self.finish_editor(hwnd, EditKey::Cancel);
        }
        false
    }

    /// The button went down on the pill: true when the editor takes the drag.
    pub fn editor_press(&mut self, now: Instant) -> bool {
        let [l, t, r, b] = self.bounds;
        let Some(editor) = self.editor.as_mut() else { return false };
        editor.press(cursor(), ((l + r) as f32 / 2.0, (t + b) as f32 / 2.0), now);
        true
    }

    /// The cursor moved: a held pill follows it (snapping near an edge,
    /// stacked on the side edges) and redraws.
    pub fn editor_drag(&mut self, hwnd: HWND, now: Instant) {
        let [l, t, r, b] = self.bounds;
        let (Some(editor), Some(placed)) = (self.editor.as_mut(), self.placed.as_mut()) else { return };
        let shown = ((r - l) as f32, (b - t) as f32);
        if editor.drag(cursor(), shown, &placed.monitor, SNAP * placed.scale, now) {
            placed.custom = editor.spot;
            placed.spot = Spot { anchor: editor.spot.map_or(placed.spot.anchor, |s| s.anchor()), free_px: None };
            self.refresh(hwnd, now);
        }
    }

    /// An arrow key: the pill moves one step that way (snapping near an edge).
    pub fn editor_nudge(&mut self, hwnd: HWND, dir: (f32, f32), now: Instant) {
        let [l, t, r, b] = self.bounds;
        let (Some(editor), Some(placed)) = (self.editor.as_mut(), self.placed.as_mut()) else { return };
        let (centre, shown) = (((l + r) as f32 / 2.0, (t + b) as f32 / 2.0), ((r - l) as f32, (b - t) as f32));
        let step = NUDGE * placed.scale;
        if editor.nudge((dir.0 * step, dir.1 * step), centre, shown, &placed.monitor, SNAP * placed.scale, now) {
            placed.custom = editor.spot;
            placed.spot = Spot { anchor: editor.spot.map_or(placed.spot.anchor, |s| s.anchor()), free_px: None };
            self.refresh(hwnd, now);
        }
    }

    pub fn editor_release(&mut self, now: Instant) {
        if let Some(editor) = self.editor.as_mut() {
            editor.release(now);
        }
    }

    /// Looks the placement up again on the next frame (the spot changed).
    fn replace_island(&mut self) {
        if let Some(p) = self.placed.as_mut() {
            p.pid = None;
            p.hwnd = None;
        }
    }
}
