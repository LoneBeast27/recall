//! What the user set with the island editor, per exe stem, in the HUD
//! cache's file: the placed spot and the island size.

use super::{exe_stem_key, HudCache};
use crate::panel::{sane_size, size_pct};
use crate::spot_edit::SavedSpot;

impl HudCache {
    /// The spot the user placed for the game (island editor).
    pub fn spot(&self, key: &str) -> Option<SavedSpot> {
        self.file.spots.get(key).copied()
    }

    /// Keeps (`Some`) or clears the game's placed spot and saves. Only an
    /// exe-stem key is kept (never a window title).
    pub fn set_spot(&mut self, key: &str, spot: Option<SavedSpot>) {
        if !exe_stem_key(key) {
            return;
        }
        match spot.filter(SavedSpot::sane) {
            Some(spot) => {
                self.file.seq += 1;
                self.file.spots.insert(key.to_string(), spot);
                self.file.spots_seq.insert(key.to_string(), self.file.seq);
            }
            None => {
                self.file.spots.remove(key);
                self.file.spots_seq.remove(key);
            }
        }
        self.trim();
        self.save();
    }

    /// The island size the user set for the game (1.0 when none).
    pub fn size(&self, key: &str) -> f32 {
        self.file.sizes.get(key).copied().unwrap_or(1.0)
    }

    /// Keeps the game's island size and saves; 100% (or a size out of
    /// range) clears it. Only an exe-stem key is kept.
    pub fn set_size(&mut self, key: &str, size: f32) {
        if !exe_stem_key(key) {
            return;
        }
        if sane_size(size) && size_pct(size) != 100 {
            self.file.seq += 1;
            self.file.sizes.insert(key.to_string(), size);
            self.file.sizes_seq.insert(key.to_string(), self.file.seq);
        } else {
            self.file.sizes.remove(key);
            self.file.sizes_seq.remove(key);
        }
        self.trim();
        self.save();
    }
}
