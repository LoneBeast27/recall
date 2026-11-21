//! How often resting the pointer on a library tile ends in opening that clip: the hit rate a
//! hover-start (begin loading on a rest, before the click) would have. Counts only, kept in
//! `<cache>\hover-count.json` on this PC; nothing is sent anywhere and hovering does nothing new.
//! Dev builds only: release builds (cfg `recall_release`) leave this module out.
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// A hover-start would begin after the pointer rests this long on one tile.
pub const REST: Duration = Duration::from_millis(200);
/// Counts are saved at most this often while they change, and when the app closes.
const SAVE_EVERY: Duration = Duration::from_secs(30);

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Counts {
    /// The pointer crossed a tile without resting on it (no load would start).
    pub passed: u64,
    /// It rested on a tile and moved on: a load would have been wasted.
    pub rested_not_opened: u64,
    /// It rested on a tile and that clip was opened: a load would have been ahead.
    pub rested_opened: u64,
    /// A clip was opened without a rest on its tile first (a keyboard open, a quick click).
    pub opened_without_rest: u64,
}

#[derive(Debug, Default)]
pub struct HoverCount {
    counts: Counts,
    hovered: Option<(String, Instant)>,
    changed: bool,
}

impl HoverCount {
    /// The pointer moved onto `path` (None: off every tile) at `now`.
    pub fn hover(&mut self, path: Option<&str>, now: Instant) {
        if self.hovered.as_ref().map(|(p, _)| p.as_str()) == path {
            return;
        }
        if let Some((_, since)) = self.hovered.take() {
            self.count(|c| if now - since >= REST { &mut c.rested_not_opened } else { &mut c.passed });
        }
        self.hovered = path.map(|p| (p.to_owned(), now));
    }

    /// `path` was opened at `now`. The hover on it ends here, counted as the open it led to.
    pub fn opened(&mut self, path: &str, now: Instant) {
        match self.hovered.take() {
            Some((hovered, since)) if hovered == path && now - since >= REST => self.count(|c| &mut c.rested_opened),
            other => {
                // A hover on another tile ended with this open (on this one it was too short).
                if let Some((_, since)) = other.filter(|(hovered, _)| hovered != path) {
                    self.count(|c| if now - since >= REST { &mut c.rested_not_opened } else { &mut c.passed });
                }
                self.count(|c| &mut c.opened_without_rest);
            }
        }
    }

    #[cfg(test)]
    pub fn counts(&self) -> &Counts {
        &self.counts
    }

    fn count(&mut self, which: impl FnOnce(&mut Counts) -> &mut u64) {
        *which(&mut self.counts) += 1;
        self.changed = true;
    }
}

struct Saved {
    tally: HoverCount,
    file: PathBuf,
    saved_at: Option<Instant>,
}

static SHARED: Mutex<Option<Saved>> = Mutex::new(None);

fn with<T>(act: impl FnOnce(&mut Saved) -> T) -> Option<T> {
    // Unit tests drive the library without a cache of their own: nothing is counted there.
    if crate::TESTING || cfg!(test) {
        return None;
    }
    let mut shared = SHARED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let saved = shared.get_or_insert_with(|| {
        let file = crate::services::storage::cache_root().join("hover-count.json");
        Saved { tally: HoverCount { counts: load(&file), ..HoverCount::default() }, file, saved_at: None }
    });
    Some(act(saved))
}

/// The library's hover changed.
pub fn hover(path: Option<&str>) {
    let _ = with(|saved| {
        saved.tally.hover(path, Instant::now());
        save_if_due(saved, false);
    });
}

/// A clip was opened from anywhere.
pub fn opened(path: &str) {
    let _ = with(|saved| {
        saved.tally.opened(path, Instant::now());
        save_if_due(saved, false);
    });
}

/// The app is closing: whatever changed is saved.
pub fn flush() {
    let _ = with(|saved| save_if_due(saved, true));
}

fn save_if_due(saved: &mut Saved, now: bool) {
    let due = now || saved.saved_at.is_none_or(|at| at.elapsed() >= SAVE_EVERY);
    if !(saved.tally.changed && due) {
        return;
    }
    let written = serde_json::to_vec_pretty(&saved.tally.counts).ok().is_some_and(|bytes| crate::services::durable::write_durable(&saved.file, &bytes).is_ok());
    if written {
        saved.tally.changed = false;
        saved.saved_at = Some(Instant::now());
    }
}

fn load(file: &Path) -> Counts {
    std::fs::read(file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{Counts, HoverCount, REST};

    #[test]
    fn rests_that_end_in_an_open_and_rests_that_do_not_are_counted_apart() {
        let (mut tally, start) = (HoverCount::default(), Instant::now());
        let at = |ms: u64| start + Duration::from_millis(ms);
        tally.hover(Some("a.mp4"), at(0));
        tally.hover(Some("b.mp4"), at(50)); // passed over a
        tally.hover(Some("c.mp4"), at(400)); // rested on b, moved on
        tally.opened("c.mp4", at(700)); // rested on c, opened it
        tally.opened("d.mp4", at(900)); // opened without a hover
        tally.hover(Some("e.mp4"), at(1000));
        tally.opened("f.mp4", at(1050)); // passed e, then opened another clip
        tally.hover(None, at(2000));
        assert_eq!(tally.counts(), &Counts { passed: 2, rested_not_opened: 1, rested_opened: 1, opened_without_rest: 2 });
    }

    #[test]
    fn a_quick_click_on_a_tile_is_not_a_rest() {
        let (mut tally, start) = (HoverCount::default(), Instant::now());
        tally.hover(Some("a.mp4"), start);
        tally.opened("a.mp4", start + REST - Duration::from_millis(1));
        assert_eq!(tally.counts(), &Counts { opened_without_rest: 1, ..Counts::default() });
        tally.hover(Some("a.mp4"), start + REST); // the same tile again: no double count
        tally.hover(Some("a.mp4"), start + REST * 3);
        tally.opened("a.mp4", start + REST * 3);
        assert_eq!(tally.counts().rested_opened, 1);
    }
}
