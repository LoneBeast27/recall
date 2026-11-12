//! Where the island sits: the target's spot, monitor and scale; Dynamic's
//! HUD probes and kept scans; a spot placed with the island editor wins.

use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use serde_json::Value;

use super::{App, Placed};
use crate::dynamic::{self, HudCache, HudGrid};
use crate::placement::{self, Anchor, Spot};
use crate::spot_edit::{self, effective};
use crate::window::{scans_apply, target_monitor, Where};
use crate::zorder::game_in_front;

impl App {
    /// The current target's spot, monitor and scale (looked up on target changes
    /// and again each time the island appears). A fixed corner is that corner
    /// at full size; Dynamic takes the clearest spot of the game's kept HUD
    /// scans, else the built-in per-game spot (top right for unknown games).
    pub(super) fn place(&mut self) -> f32 {
        let session = self.reducer.session();
        let key = session.game_key();
        let (pid, hwnd) = (session.target_pid, session.target_hwnd);
        let probe = self.place_probe.take();
        if needs_place(self.placed.as_ref(), &key, pid, hwnd, probe) {
            let on_window = !session.desktop && self.dynamic();
            let probe = usable_probe(self.placed.as_ref(), &key, pid, hwnd, probe);
            let Where { monitor, area, picture } = probe.unwrap_or_else(|| target_monitor(hwnd, pid, on_window));
            // Scans are kept per picture shape (the shape the engine measured):
            // a landscape grid never places a portrait window.
            let shaped = dynamic::shape_key(&session.hud_key(), picture.aspect());
            // A small window's island sits on the monitor corners: its scans do not map there.
            let scanned = (!session.desktop && scans_apply(area, picture)).then(|| self.hud_cache.grid(&shaped)).flatten();
            let current = self.placed.as_ref().filter(|p| p.key == key).map(|p| p.spot.anchor);
            // Dynamic: a spot the user placed (or is placing) wins over the HUD pick.
            let placed_by_user = (!session.desktop && self.dynamic())
                .then(|| effective(self.editor.as_ref(), self.hud_cache.spot(&session.hud_key())))
                .flatten();
            let (spot, hud, custom) = match Anchor::from_corner(&self.corner) {
                Some(anchor) => (Spot { anchor, free_px: None }, None, None),
                None => {
                    let mut hud = None;
                    let (spot, custom) = spot_edit::dynamic_spot(placed_by_user, || match scanned {
                        // The grid covers the game picture: the area's shape (the client rect when windowed).
                        Some(grid) => Spot { anchor: dynamic::choose(&grid, area.aspect(), current), free_px: None },
                        None => {
                            hud = self.spots.hud(&key);
                            self.spots.lookup(&key)
                        }
                    });
                    (spot, hud, custom)
                }
            };
            // Dynamic: the island size the user set (or is setting) for the game.
            let size = match (!session.desktop && self.dynamic(), self.editor.as_ref()) {
                (false, _) => 1.0,
                (true, Some(editor)) => editor.size,
                (true, None) => self.sizes.size(&session.hud_key()),
            };
            let scale = placement::scale(&monitor, &spot) * size;
            let (grow, max_h) = (placement::grow(&spot), placement::free_depth(&spot, &monitor, scale));
            // The exe stem only: `key` comes from the window title and never reaches the log.
            let exe = match session.hud_key() {
                stem if stem.is_empty() => "<unknown>".to_string(),
                stem => stem,
            };
            let line = format!(
                "{exe} anchor {:?}{} monitor [{},{},{},{}] area [{},{},{},{}] scale {scale:.2} (pid {pid:?}, hwnd {:#x})",
                spot.anchor, if custom.is_some() { " (placed by user)" } else { "" }, monitor.left, monitor.top, monitor.right, monitor.bottom, area.left, area.top, area.right, area.bottom,
                hwnd.unwrap_or(0)
            );
            self.trace.note("placed", line);
            self.placed = Some(Placed { key, pid, hwnd, monitor, area, picture, spot, scale, grow, max_h, hud, custom });
        }
        self.placed.as_ref().map_or(1.0, |p| p.scale)
    }

    /// Once per base tick while the island shows: where the target is now, so
    /// `place` notices a move to another monitor, a mode or resolution change
    /// or (Dynamic) a windowed game moved or resized.
    pub(super) fn probe_place(&mut self) {
        let Some(p) = self.placed.as_ref().filter(|_| self.island.shown()) else { return };
        let on_window = !self.reducer.session().desktop && self.dynamic();
        self.place_probe = Some(target_monitor(p.hwnd, p.pid, on_window));
    }

    /// The Dynamic corner's kept scans (`--smoke-state` runs keep none).
    pub fn load_hud_cache(&mut self, path: Option<PathBuf>) {
        self.hud_cache = HudCache::load(path);
    }

    /// The island sizes set on the editor's panel (`--smoke-state` runs keep none).
    pub fn load_sizes(&mut self, path: Option<PathBuf>) {
        self.sizes = crate::island_sizes::SizeStore::load(path);
    }

    /// The current target's scan key, `<exe stem>@<shape>` of the game picture
    /// it was placed on ("" until it is placed, for the desktop, and for a
    /// window too small for its scans to place the island: no probes then).
    fn hud_cache_key(&self) -> String {
        let session = self.reducer.session();
        let game = session.game_key();
        match self.placed.as_ref().filter(|p| p.key == game && !session.desktop && scans_apply(p.area, p.picture)) {
            Some(p) => dynamic::shape_key(&session.hud_key(), p.picture.aspect()),
            None => String::new(),
        }
    }

    pub(super) fn dynamic(&self) -> bool {
        self.smoke.as_ref().is_none_or(|s| s.editor) && Anchor::from_corner(&self.corner).is_none()
    }

    /// Dynamic: ask the engine for a HUD scan of the game when one is due
    /// (`Prober`), only while the game is recorded and in front.
    pub(super) fn probe_hud(&mut self, now: Instant) {
        if !self.dynamic() {
            return;
        }
        let session = self.reducer.session();
        let key = self.hud_cache_key();
        let active = session.recording() && game_in_front(session.target_pid.filter(|_| !session.desktop), session.target_hwnd.filter(|_| !session.desktop));
        let scans = self.hud_cache.scans(&key);
        if self.prober.due(&key, active, scans, now) {
            let sent = self.out.lock().ok().and_then(|mut slot| Some(slot.as_mut()?.write_all(b"{\"cmd\":\"hud_probe\"}\n").is_ok()));
            if sent != Some(true) {
                self.prober.answered(false, scans, now);
            }
        }
    }

    /// A `hud_scan` from the engine: kept for the game it was asked for, and
    /// the island moves to the clearest spot. True when the island moved.
    pub(super) fn on_hud_scan(&mut self, event: Value, now: Instant) -> bool {
        let grid = HudGrid::from_scan(&event);
        let key = self.hud_cache_key();
        let scans = self.hud_cache.scans(&key) + grid.is_some() as usize;
        let Some(asked) = self.prober.answered(grid.is_some(), scans, now) else { return false };
        let Some(grid) = grid else { return false };
        // Kept under the shape the scan measured (the game picture), not the area's.
        let for_key = dynamic::scan_key(&asked, &event);
        if HudGrid::is_ui(&event) { self.hud_cache.add_ui(&for_key, grid) } else { self.hud_cache.add(&for_key, grid) }
        let before = self.placed.as_ref().map(|p| p.spot.anchor);
        // Re-place now (the current spot is kept when it is still among the clearest).
        if let Some(p) = self.placed.as_mut() {
            p.pid = None;
            p.hwnd = None;
        }
        self.place();
        before != self.placed.as_ref().map(|p| p.spot.anchor)
    }
}

/// True when the island must be placed again: no placement yet, another
/// target or window, or (`probe`: this tick's monitor and area) the game
/// moved monitor, changed size or mode.
pub(super) fn needs_place(placed: Option<&Placed>, key: &str, pid: Option<u32>, hwnd: Option<isize>, probe: Option<Where>) -> bool {
    let moved = |p: &Placed| probe.is_some_and(|w| w.monitor != p.monitor || w.area != p.area || w.picture != p.picture);
    !placed.is_some_and(|p| same_target(p, key, pid, hwnd) && !moved(p))
}

fn same_target(p: &Placed, key: &str, pid: Option<u32>, hwnd: Option<isize>) -> bool {
    p.key == key && p.pid == pid && p.hwnd == hwnd
}

/// This tick's probe, only while it describes the session's target: it was
/// taken of the placed window, so a new target looks its own window up.
pub(super) fn usable_probe(placed: Option<&Placed>, key: &str, pid: Option<u32>, hwnd: Option<isize>, probe: Option<Where>) -> Option<Where> {
    probe.filter(|_| placed.is_some_and(|p| same_target(p, key, pid, hwnd)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::placement::Rect;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect { left, top, right, bottom }
    }

    fn placed(monitor: Rect, area: Rect) -> Placed {
        Placed {
            key: "VALORANT".into(),
            pid: Some(42),
            hwnd: None,
            monitor,
            area,
            picture: area,
            spot: placement::DEFAULT_SPOT,
            scale: 1.0,
            grow: 1.0,
            max_h: f32::INFINITY,
            hud: None,
            custom: None,
        }
    }

    fn at(monitor: Rect, area: Rect) -> Option<Where> {
        Some(Where { monitor, area, picture: area })
    }

    #[test]
    fn a_game_that_moves_monitor_or_mode_is_placed_again() {
        let (left, right) = (rect(0, 0, 2560, 1440), rect(2560, 0, 4480, 1080));
        let p = placed(left, left);
        assert!(!needs_place(Some(&p), "VALORANT", Some(42), None, at(left, left)), "nothing changed");
        assert!(!needs_place(Some(&p), "VALORANT", Some(42), None, None), "no probe this call");
        assert!(needs_place(Some(&p), "VALORANT", Some(42), None, at(right, right)), "moved to the other monitor");
        let work = rect(0, 0, 2560, 1392);
        assert!(needs_place(Some(&p), "VALORANT", Some(42), None, at(left, work)), "borderless to windowed");
        assert!(needs_place(Some(&p), "Other", Some(42), None, at(left, left)));
        assert!(needs_place(None, "VALORANT", Some(42), None, None));
    }

    #[test]
    fn a_new_target_is_never_placed_with_the_old_windows_probe() {
        let (left, right) = (rect(0, 0, 2560, 1440), rect(2560, 0, 4480, 1080));
        let p = placed(left, left);
        let old = at(left, left);
        assert_eq!(usable_probe(Some(&p), "VALORANT", Some(42), None, old), old, "same target: this tick's probe");
        assert_eq!(usable_probe(Some(&p), "VALORANT", Some(42), Some(0x77), old), None, "the splash handed over to a new window");
        assert_eq!(usable_probe(Some(&p), "VALORANT", Some(43), None, old), None, "another process");
        assert_eq!(usable_probe(Some(&p), "Other", Some(42), None, old), None, "another game");
        assert_eq!(usable_probe(None, "VALORANT", Some(42), None, at(right, right)), None, "nothing placed yet");
    }
}
