//! Keyboard routing: the focused text field first, then per-page shortcuts.
use gpui::{Context, KeyDownEvent, Window};

use super::{Field, Root, Route};
use crate::ui::FieldEvent;

impl Root {
    pub fn focused_field(&self, window: &Window) -> Option<Field> {
        if self.search.is_focused(window) {
            Some(Field::Search)
        } else if self.rename.is_focused(window) {
            Some(Field::Rename)
        } else if self.fps_field.is_focused(window) {
            Some(Field::Fps)
        } else {
            None
        }
    }

    pub(super) fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let clipboard = cx.read_from_clipboard().and_then(|item| item.text());
        if let Some(field) = self.focused_field(window) {
            self.field_key(field, event, clipboard, window);
            cx.notify();
            return;
        }
        let k = &event.keystroke;
        let ctrl = k.modifiers.control;
        let shift = k.modifiers.shift;
        let key = k.key.as_str();
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
                "k" if ctrl => window.focus(&self.search.focus),
                "/" => window.focus(&self.search.focus),
                "f" if ctrl => window.focus(&self.search.focus),
                "escape" if self.close_tile_menu() => {}
                "escape" => self.lib.clear_selection(),
                // The open tile menu shows "Del" beside Delete: the key acts on the menu's clips.
                "delete" if self.tile_menu.is_some() => {
                    if let Some(menu) = self.tile_menu.take() {
                        self.lib.ask_delete(&menu.targets);
                    }
                }
                "delete" if !self.lib.selection.is_empty() => self.lib.confirm_delete = true,
                "a" if ctrl => {
                    self.lib.selection = self.lib.groups.iter().flat_map(|g| g.items.iter()).map(|i| self.lib.records[*i].path.clone()).collect();
                }
                _ => return,
            },
            _ => match key {
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
                    self.search.value.clear();
                    self.lib.filter.query.clear();
                    self.lib.refilter();
                    window.focus(&self.focus);
                }
                FieldEvent::Submit => window.focus(&self.focus),
                FieldEvent::None => {}
            },
            Field::Rename => match self.rename.key_down(event, clipboard) {
                FieldEvent::Submit => {
                    if let Some(path) = self.lib.renaming.take() {
                        let title = self.rename.value.clone();
                        self.lib.rename(&path, &title);
                    }
                    window.focus(&self.focus);
                }
                FieldEvent::Cancel => {
                    self.lib.renaming = None;
                    window.focus(&self.focus);
                }
                _ => {}
            },
            Field::Fps => match self.fps_field.key_down(event, clipboard.map(|t| t.chars().filter(char::is_ascii_digit).collect())) {
                FieldEvent::Changed | FieldEvent::Submit => {
                    if let Ok(fps) = self.fps_field.value.parse::<u32>() {
                        if (1..=i32::MAX as u32).contains(&fps) {
                            self.ed.set_fps(fps);
                        }
                    }
                    if matches!(event.keystroke.key.as_str(), "enter") {
                        window.focus(&self.focus);
                    }
                }
                FieldEvent::Cancel => window.focus(&self.focus),
                FieldEvent::None => {}
            },
        }
    }

    pub fn begin_rename(&mut self, path: String, window: &mut Window) {
        let current = self.lib.record(&path).map(|r| r.display_title().to_owned()).unwrap_or_default();
        self.rename.set(current);
        self.lib.renaming = Some(path);
        window.focus(&self.rename.focus);
    }
}
