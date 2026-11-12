//! The island editor's panel on the live window: its scene and place each
//! tick, and what its buttons do. The pure side is `panel.rs`.

use std::time::Instant;

use windows::Win32::Foundation::HWND;

use super::App;
use crate::panel::{origin, placed_spot, scene, step_size, Action, PanelState};
use crate::placement::{self, Anchor, Spot};
use crate::spot_edit::EditKey;

impl App {
    /// The panel follows the editor: drawn again when the size, the place or
    /// the game changed, moved when the pill crossed to the other side.
    pub(super) fn update_panel(&mut self) {
        let (Some(window), Some(placed), Some(editor)) = (self.panel.as_mut(), self.placed.as_ref(), self.editor.as_ref()) else { return };
        // The monitor's own scale: the island size set here never grows the panel.
        let k = placement::scale(&placed.monitor, &Spot { anchor: Anchor::TopCentre, free_px: None });
        let state = PanelState { game: &editor.key, size: editor.size, edge: editor.spot.map(|s| s.edge) };
        let scene = scene(&state, k, &self.fonts);
        let at = origin((scene.w, scene.h), &placed.area, self.bounds, k);
        if let Err(error) = window.update(&self.fonts, scene, at) {
            self.trace.note("panel", error);
        }
    }

    /// A press on the panel at `at` (its own px). True when something
    /// changed (the island is then laid out again on the next tick).
    pub fn panel_press(&mut self, island: HWND, at: (f32, f32), now: Instant) -> bool {
        let Some(action) = self.panel.as_ref().and_then(|p| p.hit(at)) else { return false };
        let Some(editor) = self.editor.as_mut() else { return false };
        editor.touch(now);
        let key = editor.key.clone();
        match action {
            Action::Smaller | Action::Larger | Action::SizeReset => {
                let size = match action {
                    Action::SizeReset => 1.0,
                    _ => step_size(editor.size, action == Action::Larger),
                };
                if size == editor.size {
                    return false;
                }
                editor.size = size;
                self.hud_cache.set_size(&key, size);
                self.trace.note("editor", format!("size {:.0}% for {key}", size * 100.0));
                self.replace_island();
            }
            Action::Place(edge) => {
                let Some(placed) = self.placed.as_ref() else { return false };
                let spot = editor.place(placed_spot(edge, &placed.monitor, &placed.area), now);
                self.hud_cache.set_spot(&key, Some(spot));
                self.trace.note("editor", format!("placed {edge:?} for {key}"));
                self.sync_placed();
                self.replace_island();
            }
            Action::Auto => return self.finish_editor(island, EditKey::Reset),
            Action::Cancel => return self.finish_editor(island, EditKey::Cancel),
            Action::Done => return self.finish_editor(island, EditKey::Leave),
        }
        true
    }
}
