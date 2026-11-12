//! The spot the user placed with the island editor, per exe stem, in the
//! HUD cache's file. (Sizes live in their own file: `island_sizes.rs`.)

use super::{exe_stem_key, HudCache};
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
}
