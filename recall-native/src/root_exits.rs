//! How short-lived UI ends: the title rename, menus, the delete question, and the keyboard going
//! back to the page. A field or menu that outlives what opened it is how a person gets stuck.
use gpui::{Context, DispatchPhase, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Styled, Window, canvas};

use super::{Field, Root, Route};
use crate::library_state::RenameEnd;

impl Root {
    /// First look at every press in the page, before any control sees it.
    pub(super) fn on_press(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.drag_origin = None;
        // The first click of a double-click opened the clip: the second lands on whatever the
        // editor shows there (a timeline, Delete, Export).
        if std::mem::take(&mut self.opened_by_click) && event.click_count >= 2 {
            cx.stop_propagation();
            return;
        }
        self.press_dismisses = self.tile_menu.is_some() || self.lib.renaming.is_some() || self.sort_open;
        self.press_saw_sort = self.sort_open;
    }

    /// Follows a timeline or gain drag anywhere in the window. These listeners take no hitbox:
    /// over the title bar's status pill, an occluding element, a hover-based one would never fire
    /// and the release would be lost.
    pub(super) fn drag_watch(cx: &mut Context<Self>) -> impl IntoElement {
        let root = cx.entity().downgrade();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let moved = root.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Capture {
                        let _ = moved.update(cx, |this, cx| {
                            if this.ed.drag.is_some() {
                                match event.pressed_button {
                                    Some(MouseButton::Left) => this.ed.drag_to(f32::from(event.position.x)),
                                    _ => this.ed.end_drag(),
                                }
                                cx.notify();
                            }
                        });
                    }
                });
                let released = root.clone();
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                        let _ = released.update(cx, |this, cx| {
                            if this.ed.drag.is_some() {
                                this.ed.end_drag();
                                cx.notify();
                            }
                        });
                    }
                });
            },
        )
        .absolute()
        .size_0()
    }

    pub fn begin_rename(&mut self, path: String, window: &mut Window) {
        self.lib.finish_rename(&self.rename.value, RenameEnd::Away);
        let current = self.lib.record(&path).map(|r| r.display_title().to_owned()).unwrap_or_default();
        self.rename.set(current);
        self.lib.renaming = Some(path);
        self.rename_painted.set(true);
        window.focus(&self.rename.focus);
    }

    pub fn end_rename(&mut self, how: RenameEnd, window: &mut Window) {
        self.lib.finish_rename(&self.rename.value, how);
        window.focus(&self.focus);
    }

    /// Runs before every frame. Some exits have no event of their own: a click in the page takes
    /// focus without telling the field, a scroll or a filter takes the tile off screen, a clip
    /// vanishes. GPUI sends no keys to the page while focus sits on a field that isn't drawn.
    pub(super) fn settle_focus(&mut self, window: &mut Window) {
        let on_screen = self.rename_painted.replace(false);
        if self.lib.renaming.is_some() && (!on_screen || !self.rename.is_focused(window)) {
            self.end_rename(RenameEnd::Away, window);
        }
        // Clicking elsewhere applies a typed cache size, like Enter; leaving the page does too.
        let cache_shown = self.cache.painted.replace(false);
        if self.cache.editing && (!cache_shown || !self.cache_field.is_focused(window)) {
            self.end_cache_edit(true, window);
        }
        let orphan = match self.focused_field(window) {
            Some(Field::Search) => self.route != Route::Library || !self.lib.selection.is_empty(),
            Some(Field::Rename) => self.lib.renaming.is_none(),
            Some(Field::Fps) => self.route != Route::Editor || !self.fps_custom || self.ed.lossless(),
            Some(Field::CacheSize) => !self.cache.editing,
            None => false,
        };
        if orphan {
            window.focus(&self.focus);
        }
        // An fps field that isn't being typed in shows the rate an export would use.
        if self.focused_field(window) != Some(Field::Fps) {
            self.fps_before = self.ed.fps;
            let fps = self.ed.fps.to_string();
            if self.fps_custom && self.fps_field.value != fps {
                self.fps_field.set(fps);
            }
        }
    }

    pub(super) fn trace_ui(&self, window: &Window) {
        if !crate::trace::enabled() {
            return;
        }
        crate::trace::set_ui(serde_json::json!({
            "field": self.focused_field(window).map(|f| format!("{f:?}")),
            "renaming": self.lib.renaming,
            "text": self.rename.value,
            "tile_menu": self.tile_menu.is_some(),
            "sort_open": self.sort_open,
            "selected": self.lib.selection.len(),
            "confirm_delete": self.lib.confirm_delete,
            "hovered": self.lib.hovered,
            "query": self.lib.filter.query,
            "titles": self.lib.records.iter().map(|r| r.display_title()).collect::<Vec<_>>(),
            "custom_titles": self.lib.records.iter().filter(|r| r.title.is_some()).count(),
            "edited": self.lib.records.iter().filter(|r| r.edited).count(),
        }));
    }

    /// Escape backs out one step at a time.
    pub(super) fn library_escape(&mut self) {
        if self.close_tile_menu() {
        } else if self.sort_open {
            self.sort_open = false;
        } else if self.lib.confirm_delete {
            self.lib.confirm_delete = false;
        } else if !self.lib.selection.is_empty() {
            self.lib.clear_selection();
        } else {
            self.clear_search();
        }
    }

    pub(super) fn clear_search(&mut self) {
        self.search.value.clear();
        if !self.lib.filter.query.is_empty() {
            self.lib.filter.query.clear();
            self.lib.refilter();
        }
    }

    /// The selection bar stands where the search box is: no box, no focus.
    pub(super) fn focus_search(&mut self, window: &mut Window) {
        if self.lib.selection.is_empty() {
            self.close_tile_menu();
            self.sort_open = false;
            window.focus(&self.search.focus);
        }
    }
}
