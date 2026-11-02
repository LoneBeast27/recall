//! How a title rename ends, what a filter does to the selection, which clips a tile menu acts
//! on, and the last played clip.
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

#[test]
fn menu_acts_on_the_selection_only_when_the_clip_is_in_it() {
    let order: Vec<String> = ["a", "b", "c", "d"].map(String::from).to_vec();
    let selection = ["d", "b"].map(String::from).into_iter().collect();
    assert_eq!(super::menu_targets(&selection, order.clone(), "b"), ["b", "d"], "selection, display order");
    assert_eq!(super::menu_targets(&selection, order.clone(), "c"), ["c"], "an unselected clip alone");
    assert_eq!(super::menu_targets(&Default::default(), order, "a"), ["a"]);
}

const CLIPS: [&str; 3] = ["VALORANT__2026-10-22__01-02-03.mp4", "VALORANT__2026-10-22__01-02-04.mp4", "VALORANT__2026-10-21__01-02-05.mp4"];

#[test]
fn last_played_is_the_latest_stamp_across_every_clip() {
    let mut lib = library(&CLIPS);
    assert!(lib.last_played().is_none());
    let (a, b) = (lib.records[0].path.clone(), lib.records[1].path.clone());
    lib.mark_played_at(&b, 200);
    lib.mark_played_at(&a, 100);
    assert_eq!(lib.last_played().map(|r| r.path.as_str()), Some(b.as_str()), "the later stamp wins, not the later call");
    lib.filter = Filter { query: "no such clip".into(), ..Default::default() };
    lib.refilter();
    assert_eq!(lib.last_played().map(|r| r.path.as_str()), Some(b.as_str()), "a filter does not hide it");
    lib.reindex();
    assert_eq!(lib.last_played().map(|r| r.path.as_str()), Some(b.as_str()), "rebuilt from the records");
}

#[test]
fn an_editor_path_marks_the_listed_clip() {
    let mut lib = library(&CLIPS);
    let listed = lib.records[2].path.clone();
    lib.mark_played_at(&format!(r"\\?\{listed}"), 300);
    assert_eq!(lib.records[2].played, Some(300));
    assert_eq!(lib.last_played().map(|r| r.path.as_str()), Some(listed.as_str()));
    lib.mark_played_at(r"D:\c\not-listed.mp4", 400);
    assert_eq!(lib.last_played().map(|r| r.path.as_str()), Some(listed.as_str()), "an unknown clip changes nothing");
}

#[test]
fn reveal_finds_the_row_that_holds_the_clip() {
    let mut lib = library(&CLIPS);
    lib.columns = 1;
    lib.refilter();
    // Rows: 22 Oct header, two tiles, 21 Oct header, one tile.
    let older = lib.records.iter().find(|r| r.file_name == CLIPS[2]).unwrap().path.clone();
    assert_eq!(lib.row_of(&older), Some(4));
    assert_eq!(lib.row_of(&format!(r"\\?\{older}")), Some(4));
    lib.filter = Filter { query: "no such clip".into(), ..Default::default() };
    lib.refilter();
    assert_eq!(lib.row_of(&older), None, "filtered out");
    lib.reveal(&older);
}
