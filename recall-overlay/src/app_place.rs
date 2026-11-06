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
use crate::window::target_monitor;
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
        let fresh = self.placed.as_ref().is_some_and(|p| p.key == key && p.pid == pid && p.hwnd == hwnd);
        if !fresh {
            let (monitor, area) = target_monitor(hwnd, pid);
            // Scans are kept per picture shape: a landscape grid never places a portrait window.
            let shaped = dynamic::shape_key(&session.hud_key(), area.aspect());
            let scanned = (!session.desktop).then(|| self.hud_cache.grid(&shaped)).flatten();
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
                        Some(grid) => {
                            let aspect = (monitor.right - monitor.left) as f32 / monitor.height().max(1) as f32;
                            Spot { anchor: dynamic::choose(&grid, aspect, current), free_px: None }
                        }
                        None => {
                            hud = self.spots.hud(&key);
                            self.spots.lookup(&key)
                        }
                    });
                    (spot, hud, custom)
                }
            };
            let scale = placement::scale(monitor.height(), &spot);
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
            self.placed = Some(Placed { key, pid, hwnd, monitor, area, spot, scale, grow, max_h, hud, custom });
        }
        self.placed.as_ref().map_or(1.0, |p| p.scale)
    }

    /// The Dynamic corner's kept scans (`--smoke-state` runs keep none).
    pub fn load_hud_cache(&mut self, path: Option<PathBuf>) {
        self.hud_cache = HudCache::load(path);
    }

    /// The current target's scan key, `<exe stem>@<shape>` of the area it was
    /// placed in ("" until it is placed, and for the desktop).
    fn hud_cache_key(&self) -> String {
        let session = self.reducer.session();
        let game = session.game_key();
        match self.placed.as_ref().filter(|p| p.key == game && !session.desktop) {
            Some(p) => dynamic::shape_key(&session.hud_key(), p.area.aspect()),
            None => String::new(),
        }
    }

    pub(super) fn dynamic(&self) -> bool {
        self.smoke.is_none() && Anchor::from_corner(&self.corner).is_none()
    }

    /// Dynamic: ask the engine for a HUD scan of the game when one is due
    /// (`Prober`), only while the game is recorded and in front.
    pub(super) fn probe_hud(&mut self, now: Instant) {
        if !self.dynamic() {
            return;
        }
        let session = self.reducer.session();
        let key = self.hud_cache_key();
        let active = session.recording() && game_in_front(session.target_pid.filter(|_| !session.desktop));
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
        let Some(for_key) = self.prober.answered(grid.is_some(), scans, now) else { return false };
        let Some(grid) = grid else { return false };
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
