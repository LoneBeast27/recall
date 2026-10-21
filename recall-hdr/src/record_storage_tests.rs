//! Storage-cap selection with injected listings (no real deletes; the one
//! filesystem test lists a temp dir and recycles nothing).

use super::*;

fn clip(name: &str, gib: u64, modified: u64) -> ClipFile {
    ClipFile { name: name.into(), bytes: gib * GIB, modified }
}

fn listing(clips: Vec<ClipFile>, extra: &[&str], thumbs: &[&str]) -> Listing {
    let mut names: Vec<String> = clips.iter().map(|c| c.name.clone()).collect();
    names.extend(extra.iter().map(|s| s.to_string()));
    Listing { clips, names, thumbs: thumbs.iter().map(|s| s.to_string()).collect() }
}

#[test]
fn oldest_first_until_under_the_cap() {
    let clips = vec![clip("c.mp4", 3, 300), clip("a.mp4", 2, 100), clip("b.mp4", 2, 200), clip("d.mp4", 1, 400)];
    // 8 GiB in total, cap 4: drop a (oldest, 2) -> 6, b -> 4 = at the cap.
    let picks = select_oldest(&clips, 4 * GIB, |_| false);
    let names: Vec<&str> = picks.iter().map(|&i| clips[i].name.as_str()).collect();
    assert_eq!(names, vec!["a.mp4", "b.mp4"]);
    assert!(select_oldest(&clips, 8 * GIB, |_| false).is_empty(), "at the cap: nothing");
    assert!(select_oldest(&clips, 0, |_| false).is_empty(), "cap 0 = off");
}

#[test]
fn favourites_and_edited_clips_are_never_selected_but_still_count() {
    let clips = vec![clip("fav.mp4", 5, 1), clip("edited.mp4", 5, 2), clip("old.mp4", 1, 3), clip("new.mp4", 1, 4)];
    let l = listing(clips, &["edited.recall-edit.json", "new.mp4.recall-marks.json"], &[]);
    let favs: HashSet<String> = ["FAV.mp4".to_lowercase()].into_iter().collect();
    // 12 GiB, cap 9: the two protected clips alone are 10 GiB, so both
    // unprotected clips go (oldest first) and protected ones stay.
    let p = plan(&l, &favs, 9 * GIB);
    assert_eq!(p.remove, vec!["old.mp4".to_string(), "new.mp4".to_string()]);
    assert_eq!(p.freed, 2 * GIB);
    assert_eq!(p.total, 12 * GIB);
}

#[test]
fn edit_sidecars_protect_by_name_or_stem_case_insensitively() {
    let names: Vec<String> = ["A.mp4.recall-edit.json", "b.RECALL-EDIT.json", "c.mp4", "c.recall-marks.json"].map(String::from).to_vec();
    let e = edited(&names);
    assert!(e.contains("a.mp4") && e.contains("b.mp4"));
    assert!(!e.contains("c.mp4"), "marks are not edits");
}

#[test]
fn library_favourites_parse_and_corruption_is_an_error() {
    let favs = favourites(br#"{"VALORANT__2026-10-22__14-05-09.mp4":{"favourite":true,"title":"ace"},"x.mp4":{"title":"t"},"y.mp4":{"favourite":false}}"#).unwrap();
    assert_eq!(favs, ["valorant__2026-10-22__14-05-09.mp4".to_string()].into_iter().collect());
    assert!(favourites(b"{not json").is_err());
    assert!(favourites(b"[1,2]").is_err());
    assert!(favourites(b"{}").unwrap().is_empty());
}

#[test]
fn companions_are_the_clip_its_thumbnails_and_sidecars_only() {
    let dir = Path::new(r"V:\ALL RECORDINGS");
    let l = listing(
        vec![clip("G__2026-10-22__14-05-09.mp4", 1, 1), clip("G__2026-10-22__14-05-09-2.mp4", 1, 2)],
        &["G__2026-10-22__14-05-09.recall-marks.json", "G__2026-10-22__14-05-09.mp4.recall-edit.json", "G__2026-10-22__14-05-09-2.recall-marks.json"],
        &["G__2026-10-22__14-05-09-1783463424-thumb.jpg", "G__2026-10-22__14-05-09-2-1783463999-thumb.jpg", "notes.txt"],
    );
    let files = companions(dir, "G__2026-10-22__14-05-09.mp4", &l);
    assert_eq!(
        files,
        vec![
            dir.join("G__2026-10-22__14-05-09.mp4"),
            dir.join("G__2026-10-22__14-05-09.recall-marks.json"),
            dir.join("G__2026-10-22__14-05-09.mp4.recall-edit.json"),
            dir.join("thumbs").join("G__2026-10-22__14-05-09-1783463424-thumb.jpg"),
        ],
        "the -2 clip's sidecar and thumbnail stay"
    );
}

#[test]
fn scan_lists_top_level_clips_only() {
    let dir = std::env::temp_dir().join(format!("recall-storage-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("exports")).unwrap();
    std::fs::create_dir_all(dir.join("thumbs")).unwrap();
    std::fs::write(dir.join("a.mp4"), vec![0u8; 10]).unwrap();
    std::fs::write(dir.join("b.MP4"), vec![0u8; 20]).unwrap();
    std::fs::write(dir.join("c__rec.mp4.part"), vec![0u8; 99]).unwrap();
    std::fs::write(dir.join("exports").join("e.mp4"), vec![0u8; 99]).unwrap();
    std::fs::write(dir.join("thumbs").join("a-1783463424-thumb.jpg"), b"j").unwrap();
    let l = scan(&dir).unwrap();
    let mut clips: Vec<(String, u64)> = l.clips.iter().map(|c| (c.name.clone(), c.bytes)).collect();
    clips.sort();
    assert_eq!(clips, vec![("a.mp4".into(), 10), ("b.MP4".into(), 20)], ".part and exports\\ are not clips");
    assert_eq!(l.thumbs, vec!["a-1783463424-thumb.jpg".to_string()]);
    assert!(l.clips.iter().all(|c| c.modified > 1_700_000_000));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cleaned_event_shape() {
    let v: Value = serde_json::from_str(&cleaned_event(&Outcome { removed: 3, freed: 7 * GIB })).unwrap();
    assert_eq!(v, json!({"type":"storage_cleaned","removed":3,"freed_bytes":7 * GIB}));
}
