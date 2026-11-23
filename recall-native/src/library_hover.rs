//! Hover-scrub on library tiles: frames asked for once per clip, shown while hovered; a clip
//! whose frames cannot be made goes back to its thumbnail.
use super::{LibraryState, interaction};
use crate::worker::Workers;

impl LibraryState {
    pub fn set_hover(&mut self, path: Option<String>, workers: &Workers) {
        if self.hovered == path {
            return;
        }
        self.hovered = path.clone();
        self.hover_frac = 0.0;
        #[cfg(not(recall_release))]
        crate::services::hover_count::hover(path.as_deref());
        if let Some(path) = path {
            interaction(crate::services::storage::Interaction::Hovered(path.clone()));
            if self.scrub_wanted(&path, crate::services::storage::thumb_want()) {
                workers.scrub(path);
            }
        }
    }

    /// Ask for `path`'s hover frames to fill `want`: none yet, or the ones shown are smaller
    /// than the cards are now (a bigger grid size, a wider window, a denser display). The old
    /// frames stay on screen until the new ones arrive. Records the ask, so each box is asked
    /// for once.
    pub(super) fn scrub_wanted(&mut self, path: &str, want: crate::services::storage::Want) -> bool {
        let shown = self.scrub.contains_key(path);
        let small = self.scrub_sizes.get(path).is_some_and(|&(w, h)| !want.covered_by(w, h));
        let asked = self.scrub_requested.get(path).copied();
        let ask = if shown { small && asked.is_none_or(|a| a.width < want.width || a.height < want.height) } else { asked.is_none() };
        if ask {
            self.scrub_requested.insert(path.to_owned(), want);
        }
        ask
    }

    pub fn on_scrub(&mut self, path: String, result: Result<Vec<String>, String>) -> bool {
        match result {
            Ok(frames) if !frames.is_empty() => {
                // Remade in place at a larger size: the view must drop the old decodes.
                if self.scrub.get(&path) == Some(&frames) {
                    self.refreshed_images.extend(frames.iter().cloned());
                }
                self.scrub_sizes.insert(path.clone(), super::file_size(&frames[0]));
                self.scrub.insert(path.clone(), frames);
                self.hovered.as_deref() == Some(path.as_str())
            }
            _ => {
                // The tile shows its thumbnail again instead of waiting for frames for good.
                let hovered = self.hovered.as_deref() == Some(path.as_str());
                self.scrub_failed.insert(path);
                hovered
            }
        }
    }

    /// The thumbnail is on its way from the drive (a failed one stays a plain placeholder).
    pub fn thumb_waiting(&self, path: &str) -> bool {
        !self.thumb_failed.contains(path)
    }

    /// Hover frames were asked for and are not here yet.
    pub fn scrub_waiting(&self, path: &str) -> bool {
        self.hovered.as_deref() == Some(path) && !self.scrub.contains_key(path) && !self.scrub_failed.contains(path)
    }

    /// The clip's length could not be read (shown as "—").
    pub fn duration_unknown(&self, path: &str) -> bool {
        self.duration_failed.contains(path)
    }

    /// Frame to show for the hovered tile, if scrub frames are ready.
    pub fn scrub_frame(&self, path: &str) -> Option<&str> {
        if self.hovered.as_deref() != Some(path) {
            return None;
        }
        let frames = self.scrub.get(path)?;
        let index = ((self.hover_frac * frames.len() as f32) as usize).min(frames.len() - 1);
        frames.get(index).map(String::as_str)
    }
}
