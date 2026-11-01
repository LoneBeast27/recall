//! How a title rename ends, and what a filter does to the selection.
use super::*;
use crate::{backend::ClipEntry, services::library::Filter};

fn library(names: &[&str]) -> LibraryState {
    let mut lib = LibraryState::new();
    lib.store = MetaStore::default();
    let entries = names.iter().map(|n| ClipEntry { name: (*n).into(), path: format!(r"D:\c\{n}"), bytes: 10, mtime: 1_700_000_000, thumb: None, folder: None }).collect();
    lib.set_entries(Listing { entries, sidecars: Default::default(), drive: None });
    lib
}

fn renamed(text: &str, how: RenameEnd, title: Option<&str>) -> (Option<String>, String) {
    let mut lib = library(&["VALORANT__2026-10-22__01-02-03.mp4"]);
    let path = lib.records[0].path.clone();
    lib.records[0].title = title.map(str::to_owned);
    lib.renaming = Some(path);
    lib.finish_rename(text, how);
    assert!(lib.renaming.is_none(), "every end closes the field");
    (lib.records[0].title.clone(), lib.records[0].display_title().to_owned())
}

#[test]
fn enter_and_click_away_save_escape_does_not() {
    assert_eq!(renamed("ace", RenameEnd::Save, None).0.as_deref(), Some("ace"));
    assert_eq!(renamed("  ace ", RenameEnd::Away, None).0.as_deref(), Some("ace"));
    assert_eq!(renamed("ace", RenameEnd::Cancel, Some("old")).0.as_deref(), Some("old"));
}

#[test]
fn an_empty_field_clears_the_title_only_on_enter() {
    assert_eq!(renamed("  ", RenameEnd::Save, Some("old")).0, None);
    assert_eq!(renamed("  ", RenameEnd::Away, Some("old")).0.as_deref(), Some("old"));
}

#[test]
fn opening_and_leaving_pins_no_title() {
    let game = renamed("x", RenameEnd::Cancel, None).1;
    assert_eq!(renamed(&game, RenameEnd::Away, None).0, None, "click away");
    assert_eq!(renamed(&game, RenameEnd::Save, None).0, None, "Enter");
}

#[test]
fn with_no_rename_open_nothing_happens() {
    let mut lib = library(&["VALORANT__2026-10-22__01-02-03.mp4"]);
    lib.finish_rename("stale text", RenameEnd::Save);
    assert_eq!(lib.records[0].title, None);
}

#[test]
fn a_filter_takes_hidden_clips_out_of_the_selection_and_ends_the_question() {
    let mut lib = library(&["VALORANT__2026-10-22__01-02-03.mp4", "VALORANT__2026-10-22__01-02-04.mp4"]);
    let (a, b) = (lib.records[0].path.clone(), lib.records[1].path.clone());
    lib.records[0].favourite = true;
    lib.ask_delete(&[a.clone(), b.clone()]);
    lib.filter = Filter { favourites_only: true, ..Default::default() };
    lib.refilter();
    assert_eq!(lib.selection.iter().collect::<Vec<_>>(), [&a]);
    assert!(lib.confirm_delete, "a clip is still selected");
    lib.records[0].favourite = false;
    lib.refilter();
    assert!(lib.selection.is_empty() && !lib.confirm_delete);
}
