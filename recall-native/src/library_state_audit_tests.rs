//! Library view model over time and at scale: batch favourites, same-named clips in two
//! folders, removed clips staying removed, Edited on the right clip, midnight, failed previews
//! and lengths.
use std::path::PathBuf;

use super::*;
use crate::{
    backend::ClipEntry,
    services::library::watch::{AddedClip, ClipChanges},
};

fn entry(path: &str) -> ClipEntry {
    let name = Path::new(path).file_name().unwrap().to_string_lossy().into_owned();
    ClipEntry { name, path: path.into(), bytes: 10, mtime: 1_700_000_000, thumb: None, folder: None }
}

fn listing(paths: &[&str]) -> Listing {
    Listing { entries: paths.iter().map(|p| entry(p)).collect(), sidecars: Default::default(), drive: None }
}

fn library_with(store: MetaStore, paths: &[&str]) -> LibraryState {
    let mut lib = LibraryState::new();
    lib.store = store;
    lib.set_entries(listing(paths));
    lib
}

fn library(paths: &[&str]) -> LibraryState {
    library_with(MetaStore::default(), paths)
}

fn rec<'a>(lib: &'a LibraryState, path: &str) -> &'a ClipRecord {
    lib.records.iter().find(|r| r.path == path).unwrap()
}

#[test]
fn a_length_no_probe_could_read_is_not_probed_again_on_the_next_start() {
    let path = r"D:\croken.mkv";
    let mut first = library(&[path]);
    first.on_duration(path.into(), None);
    // The next start: a store holding what the first run learned.
    let mut store = MetaStore::default();
    std::mem::swap(&mut store, &mut first.store);
    let next = library_with(store, &[path]);
    assert!(next.duration_unknown(path), "shown as unknown at once");
    assert!(next.duration_queued.contains(path), "never queued for ffprobe again");
}

#[test]
fn favouriting_a_big_selection_writes_the_library_once() {
    let paths: Vec<String> = (0..500).map(|i| format!(r"D:\c\G__2026-10-01__10-00-00-{i:03}.mp4")).collect();
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let mut lib = library(&refs);
    let before = lib.store.writes;
    lib.toggle_favourite(&paths);
    assert_eq!(lib.store.writes - before, 1, "one write for the whole selection");
    assert_eq!(lib.favourites, 500);
}

#[test]
fn same_named_clips_in_two_folders_keep_their_own_metadata() {
    let (a, b) = (r"D:\Recall\x.mp4", r"E:\Backup\x.mp4");
    let mut lib = library(&[a, b]);
    lib.toggle_favourite(&[a.to_owned()]);
    lib.rename(b, "ace");
    lib.mark_played_at(r"\\?\E:\BACKUP\x.mp4", 42);
    assert!(rec(&lib, a).favourite && !rec(&lib, b).favourite);
    assert_eq!((rec(&lib, a).title.as_deref(), rec(&lib, b).title.as_deref()), (None, Some("ace")));
    assert_eq!((rec(&lib, a).played, rec(&lib, b).played), (None, Some(42)));
    // A fresh listing reads them back apart.
    lib.set_entries(listing(&[a, b]));
    assert!(rec(&lib, a).favourite && !rec(&lib, b).favourite);
    // The name-only fallback never guesses between two clips.
    assert!(lib.record(r"Z:\elsewhere\x.mp4").is_none());
}

#[test]
fn name_keyed_metadata_moves_to_every_clip_with_that_name_once() {
    let mut store = MetaStore::default();
    store.set_favourite("x.mp4", true).unwrap();
    store.set_title("gone.mp4", Some("kept")).unwrap();
    let (a, b) = (r"D:\Recall\x.mp4", r"E:\Backup\x.mp4");
    let mut lib = library_with(store, &[a, b]);
    assert!(rec(&lib, a).favourite && rec(&lib, b).favourite, "an ambiguous name goes to both");
    lib.toggle_favourite(&[b.to_owned()]);
    lib.set_entries(listing(&[a, b]));
    assert!(rec(&lib, a).favourite && !rec(&lib, b).favourite, "after the move each has its own");
    // A name with no listed clip waits for its clip.
    lib.set_entries(listing(&[a, b, r"F:\Old\gone.mp4"]));
    assert_eq!(rec(&lib, r"F:\Old\gone.mp4").title.as_deref(), Some("kept"));
}

#[test]
fn a_removed_clip_stays_removed_when_the_next_listing_replays_the_watcher() {
    let mut lib = library(&[r"D:\Medal\a.mp4"]);
    let added = AddedClip { entry: entry(r"D:\Medal\b.mp4"), edited: false };
    lib.apply_changes(ClipChanges { added: vec![added], ..Default::default() });
    assert!(lib.lists(r"D:\Medal\b.mp4"));
    lib.forget(&[PathBuf::from(r"D:\Medal\b.mp4")]);
    // The next listing (any Recall save) leaves the hidden clip out.
    lib.set_entries(listing(&[r"D:\Medal\a.mp4"]));
    assert!(!lib.lists(r"D:\Medal\b.mp4"));
}

#[test]
fn edited_is_set_on_the_flushed_clip_only() {
    let (mkv, mp4) = (r"D:\a\x.mkv", r"D:\b\x.mp4");
    let mut lib = library(&[mkv, mp4]);
    lib.mark_edited(mkv, true);
    assert!(rec(&lib, mkv).edited && !rec(&lib, mp4).edited);
    lib.mark_edited(r"\\?\D:\A\X.MKV", false);
    assert!(!rec(&lib, mkv).edited, "the editor's canonical path finds the clip");
}

#[test]
fn today_rolls_over_without_a_new_listing() {
    let now = library::today();
    let name = format!(r"D:\c\G__{:04}-{:02}-{:02}__10-00-00.mp4", now.year, now.month, now.day);
    let mut lib = library(&[]);
    lib.today = LocalStamp { year: 2001, month: 1, day: 1, ..LocalStamp::default() };
    lib.apply_changes(ClipChanges { added: vec![AddedClip { entry: entry(&name), edited: false }], ..Default::default() });
    assert_eq!(lib.groups[0].label, "Today");
    // Midnight with no change at all: the poll's day check relabels.
    let mut lib = library(&[r"D:\c\G__2026-10-01__10-00-00.mp4"]);
    lib.roll_day_to(LocalStamp { year: 2026, month: 10, day: 1, hour: 23, ..LocalStamp::default() });
    assert_eq!(lib.groups[0].label, "Today");
    let tomorrow = LocalStamp { year: 2026, month: 10, day: 2, ..LocalStamp::default() };
    assert!(lib.roll_day_to(tomorrow));
    assert_eq!(lib.groups[0].label, "Yesterday");
    assert!(!lib.roll_day_to(tomorrow), "same day: nothing to redo");
}

#[test]
fn a_failed_preview_stops_waiting_and_an_unknown_length_counts_as_known() {
    let path = r"D:\c\G__2026-10-01__10-00-00.mp4";
    let mut lib = library(&[path]);
    lib.hovered = Some(path.into());
    assert!(lib.scrub_waiting(path));
    lib.on_scrub(path.into(), Err("ffmpeg: invalid data".into()));
    assert!(!lib.scrub_waiting(path), "no 'Loading preview…' for a clip that has none");
    assert!(lib.on_duration(path.into(), None));
    assert!(lib.duration_unknown(path) && lib.groups[0].durations_complete);
    lib.refilter();
    assert!(lib.groups[0].durations_complete, "still complete after a refilter");
}
