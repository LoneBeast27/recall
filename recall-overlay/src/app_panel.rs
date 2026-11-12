//! The island editor's panel on the live window: its scene and place each
//! tick, and what its buttons do. The pure side is `panel.rs`.

use std::time::Instant;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{MonitorFromRect, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

use super::App;
use crate::panel::{origin, panel_scale, placed_spot, scene, step_size, Action, PanelState};
use crate::placement::Rect;
use crate::spot_edit::EditKey;

/// The monitor's effective DPI (96 when Windows can't say).
fn monitor_dpi(monitor: &Rect) -> u32 {
    let rect = RECT { left: monitor.left, top: monitor.top, right: monitor.right, bottom: monitor.bottom };
    let (mut x, mut y) = (96, 96);
    let found = unsafe { GetDpiForMonitor(MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST), MDT_EFFECTIVE_DPI, &mut x, &mut y) };
    if found.is_ok() && x > 0 { x } else { 96 }
}

/// A short stable tag for a game in the log (FNV-1a), never its exe name.
pub(super) fn game_tag(key: &str) -> String {
    let hash = key.bytes().fold(0x811c_9dc5u32, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193));
    format!("game {hash:08x}")
}

/// The editor as `--diagnostics` shows it: what a look check clicks and reads.
#[derive(serde::Serialize)]
pub struct EditorView {
    pub size: f32,
    pub edge: Option<crate::spot_edit::Edge>,
    /// How often a drag moved the pill to another edge.
    pub edge_changes: u32,
    /// The panel's screen box `[l, t, r, b]`, once drawn.
    pub panel_px: Option<[i32; 4]>,
    /// Each button's action and screen box.
    pub buttons: Vec<(String, [i32; 4])>,
}

impl App {
    pub fn editor_view(&self) -> Option<EditorView> {
        let editor = self.editor.as_ref()?;
        let drawn = self.panel.as_ref().and_then(|p| p.drawn());
        let panel_px = drawn.map(|(scene, (x, y))| [x, y, x + scene.w, y + scene.h]);
        let buttons = drawn.map_or_else(Vec::new, |(scene, (x, y))| {
            let at = |v: f32, o: i32| o + v.round() as i32;
            scene.buttons.iter().map(|b| (format!("{:?}", b.action), [at(b.rect[0], x), at(b.rect[1], y), at(b.rect[2], x), at(b.rect[3], y)])).collect()
        });
        Some(EditorView { size: editor.size, edge: editor.spot.map(|s| s.edge), edge_changes: editor.edge_changes, panel_px, buttons })
    }

    /// The panel follows the editor: drawn again when the size, the place or
    /// the game changed, moved when the pill crossed to the other side.
    pub(super) fn update_panel(&mut self) {
        let (Some(window), Some(placed), Some(editor)) = (self.panel.as_mut(), self.placed.as_ref(), self.editor.as_ref()) else { return };
        // The monitor's own scale (the island size set here never grows the
        // panel), kept readable at the monitor's DPI.
        let state = PanelState { game: &editor.key, size: editor.size, edge: editor.spot.map(|s| s.edge) };
        let w1 = scene(&state, 1.0, &self.fonts).w as f32;
        let k = panel_scale(&placed.monitor, monitor_dpi(&placed.monitor), &placed.area, w1);
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
        self.editor_changed = true;
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
                self.sizes.set_size(&key, size);
                self.trace.note("editor", format!("size {:.0}% for {}", size * 100.0, game_tag(&key)));
                self.replace_island();
            }
            Action::Place(edge) => {
                let Some(placed) = self.placed.as_ref() else { return false };
                let spot = editor.place(placed_spot(edge, &placed.monitor, &placed.area), now);
                self.hud_cache.set_spot(&key, Some(spot));
                self.trace.note("editor", format!("placed {edge:?} for {}", game_tag(&key)));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_tag_is_stable_short_and_never_the_name() {
        assert_eq!(game_tag("valorant"), game_tag("valorant"));
        assert_ne!(game_tag("valorant"), game_tag("apex"));
        let tag = game_tag("valorant");
        assert!(!tag.contains("valorant") && tag.len() == "game ".len() + 8, "{tag}");
    }
}
