//! Keyboard routing: the focused text field first, then per-page shortcuts.
use gpui::{Context, KeyDownEvent, Window};

use super::{Field, Root, Route};
use crate::{library_state::RenameEnd, ui::FieldEvent};

impl Root {
    pub fn focused_field(&self, window: &Window) -> Option<Field> {
        if self.search.is_focused(window) {
            Some(Field::Search)
        } else if self.rename.is_focused(window) {
            Some(Field::Rename)
        } else if self.fps_field.is_focused(window) {
            Some(Field::Fps)
        } else if self.cache_field.is_focused(window) {
            Some(Field::CacheSize)
        } else {
            None
        }
    }

    pub(super) fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Any key (Escape, F2, Enter) cancels a title click still waiting to open (`tile_click.rs`).
        self.lib.title_click = None;
        // Settings → Shortcuts: a chip waiting for keys takes the press first.
        if self.capture_shortcut_key(&event.keystroke) {
            cx.notify();
            return;
        }
        let clipboard = cx.read_from_clipboard().and_then(|item| item.text());
        // Audio page: a source's name being edited.
        if self.audio_name_key(event, clipboard.clone(), window) {
            cx.notify();
            return;
        }
        // Settings → Clip: the Custom frame rate box.
        if self.fps_input_key(event, clipboard.clone(), window) {
            cx.notify();
            return;
        }
        if let Some(field) = self.focused_field(window) {
            self.field_key(field, event, clipboard, window);
            cx.notify();
            return;
        }
        let k = &event.keystroke;
        let ctrl = k.modifiers.control;
        let shift = k.modifiers.shift;
        let key = k.key.as_str();
        // Keys would act on the plan the mouse is holding: Escape puts it back.
        if self.ed.drag.is_some() {
            if key == "escape" {
                self.ed.cancel_drag();
                cx.notify();
            }
            return;
        }
        match self.route {
            Route::Editor => match key {
                "space" => self.ed.toggle_play(),
                "i" => self.ed.set_in(),
                "o" => self.ed.set_out(),
                "c" | "s" if !ctrl => self.ed.split(),
                "delete" | "backspace" => self.ed.delete_selected(),
                "z" if ctrl && shift => self.ed.redo(),
                "z" if ctrl => self.ed.undo(),
                "y" if ctrl => self.ed.redo(),
                "left" => self.ed.nudge(if shift { -1.0 } else { -1.0 / 60.0 }),
                "right" => self.ed.nudge(if shift { 1.0 } else { 1.0 / 60.0 }),
                "j" => self.ed.nudge(-5.0),
                "l" => self.ed.nudge(5.0),
                "k" => self.ed.pause(),
                "home" => self.ed.seek(self.ed.plan.ranges.first().map_or(0.0, |r| r.start)),
                "end" => self.ed.seek(self.ed.plan.ranges.last().map_or(0.0, |r| r.end)),
                "escape" => self.go(Route::Library),
                _ => return,
            },
            Route::Library => match key {
                "k" | "f" if ctrl => self.focus_search(window),
                "/" => self.focus_search(window),
                "escape" => self.library_escape(),
                // The open tile menu shows "Del" beside Delete: the key acts on the menu's clips.
                "delete" if self.tile_menu.is_some() => {
                    if let Some(menu) = self.tile_menu.take() {
                        self.lib.ask_delete(&menu.targets);
                    }
                }
                "delete" if !self.lib.selection.is_empty() => self.lib.confirm_delete = true,
                // Enter opens the one clip a Shift/Ctrl click selected (a plain click opens at once).
                "enter" if self.tile_menu.is_none() && !self.lib.confirm_delete && self.lib.only_selected().is_some() => {
                    if let Some(path) = self.lib.only_selected() {
                        self.open_clip(path);
                    }
                }
                "a" if ctrl => {
                    self.lib.selection = self.lib.groups.iter().flat_map(|g| g.items.iter()).map(|i| self.lib.records[*i].path.clone()).collect();
                }
                _ => return,
            },
            _ => match key {
                "escape" if self.settings_ui.quality_preview.is_some() => self.settings_ui.quality_preview = None,
                "escape" => self.go(Route::Library),
                _ => return,
            },
        }
        cx.notify();
    }

    fn field_key(&mut self, field: Field, event: &KeyDownEvent, clipboard: Option<String>, window: &mut Window) {
        match field {
            Field::Search => match self.search.key_down(event, clipboard) {
                FieldEvent::Changed => {
                    self.lib.filter.query = self.search.value.clone();
                    self.lib.refilter();
                }
                FieldEvent::Cancel => {
                    self.clear_search();
                    window.focus(&self.focus);
                }
                FieldEvent::Submit => window.focus(&self.focus),
                FieldEvent::None => {}
            },
            Field::Rename => match self.rename.key_down(event, clipboard) {
                FieldEvent::Submit => self.end_rename(RenameEnd::Save, window),
                FieldEvent::Cancel => self.end_rename(RenameEnd::Cancel, window),
                _ => {}
            },
            Field::Fps => {
                let outcome = self.fps_field.key_down(event, clipboard);
                if outcome == FieldEvent::Changed {
                    if let Ok(fps) = self.fps_field.value.parse::<u32>() {
                        if (1..=i32::MAX as u32).contains(&fps) {
                            self.ed.set_fps(fps);
                        }
                    }
                }
                if outcome == FieldEvent::Cancel {
                    self.ed.set_fps(self.fps_before);
                }
                if matches!(outcome, FieldEvent::Submit | FieldEvent::Cancel) {
                    window.focus(&self.focus);
                }
            }
            Field::CacheSize => match self.cache_field.key_down(event, clipboard) {
                FieldEvent::Submit => self.end_cache_edit(true, window),
                FieldEvent::Cancel => self.end_cache_edit(false, window),
                _ => {}
            },
        }
    }
}
