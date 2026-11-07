//! What a press on a clip tile does to the selection. A plain click opens the clip
//! (`views/tile_click.rs`), clears the selection and marks where a Shift run starts. Ctrl adds or
//! removes one clip, Shift takes the run from the last clicked clip (or adds one when there is
//! none); neither opens anything.
use std::{collections::HashSet, path::PathBuf};

use super::LibraryState;

impl LibraryState {
    /// A click on a tile with its modifier keys.
    pub fn click(&mut self, path: &str, ctrl: bool, shift: bool) {
        self.confirm_delete = false;
        if shift && !ctrl {
            if let Some(range) = self.anchor.clone().and_then(|anchor| self.run_between(&anchor, path)) {
                self.selection = range.into_iter().collect();
                return;
            }
        }
        if ctrl || shift {
            if !self.selection.remove(path) {
                self.selection.insert(path.to_owned());
            }
        } else {
            self.selection.clear();
        }
        self.anchor = Some(path.to_owned());
    }

    /// The one selected clip, if exactly one is selected (what Enter and the bar's Open open).
    pub fn only_selected(&self) -> Option<String> {
        if self.selection.len() == 1 { self.selection.iter().next().cloned() } else { None }
    }

    /// The clips from `a` to `b` in display order, both ends included.
    fn run_between(&self, a: &str, b: &str) -> Option<Vec<String>> {
        let order = self.display_order();
        let (x, y) = (order.iter().position(|p| p == a)?, order.iter().position(|p| p == b)?);
        Some(order[x.min(y)..=x.max(y)].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::super::rename_tests::library;

    const NAMES: [&str; 4] = [
        "VALORANT__2026-10-22__01-02-01.mp4",
        "VALORANT__2026-10-22__01-02-02.mp4",
        "VALORANT__2026-10-22__01-02-03.mp4",
        "VALORANT__2026-10-22__01-02-04.mp4",
    ];

    fn order(lib: &super::LibraryState) -> Vec<String> {
        lib.display_order()
    }

    fn selected(lib: &super::LibraryState) -> Vec<String> {
        order(lib).into_iter().filter(|p| lib.selection.contains(p)).collect()
    }

    #[test]
    fn a_plain_click_clears_the_selection_and_selects_nothing() {
        let mut lib = library(&NAMES);
        let o = order(&lib);
        lib.click(&o[0], true, false);
        lib.click(&o[2], true, false);
        lib.confirm_delete = true;
        lib.click(&o[1], false, false);
        assert!(lib.selection.is_empty(), "no selection bar for a plain click (it opens the clip)");
        assert!(!lib.confirm_delete, "the delete question goes with the selection");
        assert_eq!(lib.only_selected(), None);
    }

    #[test]
    fn shift_takes_the_run_from_the_last_clicked_clip() {
        let mut lib = library(&NAMES);
        let o = order(&lib);
        lib.click(&o[1], false, false);
        lib.click(&o[3], false, true);
        assert_eq!(selected(&lib), o[1..=3].to_vec());
        lib.click(&o[0], false, true);
        assert_eq!(selected(&lib), o[0..=1].to_vec(), "the run starts from the same clip");
        assert_eq!(lib.only_selected(), None);
    }

    #[test]
    fn shift_with_no_clip_clicked_yet_adds_one() {
        let mut lib = library(&NAMES);
        let o = order(&lib);
        lib.click(&o[2], false, true);
        assert_eq!(selected(&lib), [o[2].clone()]);
        lib.clear_selection();
        lib.click(&o[0], false, true);
        assert_eq!(selected(&lib), [o[0].clone()]);
        lib.click(&o[1], false, true);
        assert_eq!(selected(&lib), o[0..=1].to_vec(), "that clip starts the next run");
    }

    #[test]
    fn ctrl_adds_and_removes_one_clip() {
        let mut lib = library(&NAMES);
        let o = order(&lib);
        lib.click(&o[0], true, false);
        lib.click(&o[2], true, false);
        assert_eq!(selected(&lib), [o[0].clone(), o[2].clone()]);
        lib.click(&o[0], true, false);
        assert_eq!(selected(&lib), [o[2].clone()]);
        assert_eq!(lib.only_selected().as_deref(), Some(o[2].as_str()));
    }
}

impl LibraryState {
    /// Clips a tile menu acts on: the whole selection (in display order) when `path` is part
    /// of it, else just `path`.
    pub fn menu_targets(&self, path: &str) -> Vec<String> {
        menu_targets(&self.selection, self.display_order(), path)
    }

    /// Selects exactly `paths` and asks the inline "Move to Recycle Bin?" question.
    pub fn ask_delete(&mut self, paths: &[String]) {
        self.selection = paths.iter().cloned().collect();
        self.anchor = paths.first().cloned();
        self.confirm_delete = !self.selection.is_empty();
    }

    pub fn selected_paths(&self) -> Vec<PathBuf> {
        self.display_order().into_iter().filter(|p| self.selection.contains(p)).map(PathBuf::from).collect()
    }
}

pub(super) fn menu_targets(selection: &HashSet<String>, display_order: Vec<String>, path: &str) -> Vec<String> {
    if selection.contains(path) {
        display_order.into_iter().filter(|p| selection.contains(p)).collect()
    } else {
        vec![path.to_owned()]
    }
}
