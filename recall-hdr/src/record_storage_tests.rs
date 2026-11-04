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

/// "Now" for selection: every fixture time above is hours in the past.
const NOW: u64 = 1_000_000;

#[test]
fn oldest_first_until_under_the_cap() {
    let clips = vec![clip("c.mp4", 3, 300), clip("a.mp4", 2, 100), clip("b.mp4", 2, 200), clip("d.mp4", 1, 400)];
    // 8 GiB in total, cap 4: drop a (oldest, 2) -> 6, b -> 4 = at the cap.
    let picks = select_oldest(&clips, 4 * GIB, NOW, |_| false);
    let names: Vec<&str> = picks.iter().map(|&i| clips[i].name.as_str()).collect();
    assert_eq!(names, vec!["a.mp4", "b.mp4"]);
    assert!(select_oldest(&clips, 8 * GIB, NOW, |_| false).is_empty(), "at the cap: nothing");
    assert!(select_oldest(&clips, 0, NOW, |_| false).is_empty(), "cap 0 = off");
}

#[test]
fn the_newest_clip_and_recent_clips_are_never_selected() {
    let clips = vec![
        clip("old.mp4", 1, 100),
        clip("recent.mp4", 1, NOW - RECENT_SECS + 1),
        clip("settled.mp4", 1, NOW - RECENT_SECS),
        clip("future.mp4", 1, NOW + 5),
        clip("mid.mp4", 1, 200),
    ];
    // Cap 1 byte: everything is over it, yet only clips older than 10 minutes
    // go (future.mp4 is both the newest and not settled).
    let picks = select_oldest(&clips, 1, NOW, |_| false);
    let names: Vec<&str> = picks.iter().map(|&i| clips[i].name.as_str()).collect();
    assert_eq!(names, vec!["old.mp4", "mid.mp4", "settled.mp4"]);
    // A lone just-saved clip over the cap stays (the bug: it was recycled).
    let just_saved = vec![clip("G__2026-10-22__14-05-09-275__ab.mp4", 50, 100)];
    assert!(select_oldest(&just_saved, GIB, NOW, |_| false).is_empty(), "the single newest clip stays");
    let two = vec![clip("a.mp4", 30, 100), clip("b.mp4", 30, 200)];
    let picks = select_oldest(&two, 20 * GIB, NOW, |_| false);
    assert_eq!(picks, vec![0], "the newest stays even when still over the cap");
}

#[test]
fn only_recall_named_clips_qualify() {
    for name in [
        "VALORANT__2026-10-22__14-05-09.mp4",
        "VALORANT__2026-10-17__22-34-31-275__0123456789abcdef0123456789abcdef.mp4",
        "THE-FINALS__2026-10-22__14-05-09__rec.mp4",
        "THE-FINALS__2026-10-22__14-05-09__rec-2.MP4",
        "Rocket-League-__2026-10-22__13-06-02_trim.mp4",
        "Game_With_Underscores__2026-10-22__23-23-00 BLIND FUZE.mp4",
    ] {
        assert!(is_recall_clip(name), "{name}");
    }
    for name in [
        "THE-FINALS__2026-10-22__14-05-09__rec.mp4.part",
        "VALORANT__2026-10-22__14-05-09.mkv",
        "__2026-10-22__14-05-09.mp4",
        "Valorant_2023_01_27_10_10_11.mp4",
        "Rocket League 2024.01.27 - 22.10.11.03.DVR.mp4",
        "lethal company moment.mp4",
        "G__2026-10-22__14-05.mp4",
        "holiday.mp4",
    ] {
        assert!(!is_recall_clip(name), "{name}");
    }
}

#[test]
fn favourites_and_edited_clips_are_never_selected_but_still_count() {
    let clips = vec![clip("fav.mp4", 5, 1), clip("edited.mp4", 5, 2), clip("old.mp4", 1, 3), clip("new.mp4", 1, 4)];
    let l = listing(clips, &["edited.recall-edit.json", "new.mp4.recall-marks.json"], &[]);
    let favs: HashSet<String> = ["FAV.mp4".to_lowercase()].into_iter().collect();
    // 12 GiB, cap 9: the two protected clips alone are 10 GiB, so every
    // unprotected clip but the newest goes and protected ones stay.
    let p = plan(&l, &favs, 9 * GIB, NOW);
    assert_eq!(p.remove, vec!["old.mp4".to_string()], "new.mp4 is the newest clip");
    assert_eq!(p.freed, GIB);
    assert_eq!(p.total, 12 * GIB);
}

#[test]
fn a_quarantined_library_file_cancels_the_run() {
    let dir = std::env::temp_dir().join(format!("recall-storage-lib-{}-{}", std::process::id(), line!()));
    std::fs::create_dir_all(&dir).unwrap();
    let lib = dir.join("library.json");
    assert_eq!(quarantine_path(&lib), dir.join("library.json.bad"));
    assert!(favourites_at(&lib).unwrap().is_empty(), "missing, nothing quarantined: no favourites");
    std::fs::write(&lib, br#"{"G__2026-10-22__14-05-09.mp4":{"favourite":true}}"#).unwrap();
    assert_eq!(favourites_at(&lib).unwrap().len(), 1);
    std::fs::write(dir.join("library.json.bad"), b"{corrupt").unwrap();
    assert_eq!(favourites_at(&lib).unwrap().len(), 1, "a present library wins over an old .bad");
    std::fs::remove_file(&lib).unwrap();
    let err = favourites_at(&lib).expect_err("missing + .bad = favourites unknown");
    assert!(err.contains("quarantined"), "{err}");
    std::fs::write(&lib, b"{not json").unwrap();
    assert!(favourites_at(&lib).is_err(), "corrupt = unknown");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn caps_below_the_minimum_never_run() {
    let err = run_once(Path::new(r"Z:\does-not-matter"), STORAGE_CAP_MIN_GB - 1).expect_err("refused");
    assert!(err.contains("minimum"), "{err}");
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
    let dir = Path::new(r"E:\Clips");
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
    let (a, b) = ("G__2026-10-22__14-05-09.mp4", "G__2026-10-22__15-00-00__REC-2.MP4");
    std::fs::write(dir.join(a), vec![0u8; 10]).unwrap();
    std::fs::write(dir.join(b), vec![0u8; 20]).unwrap();
    std::fs::write(dir.join("G__2026-10-22__16-00-00__rec.mp4.part"), vec![0u8; 99]).unwrap();
    std::fs::write(dir.join("holiday.mp4"), vec![0u8; 99]).unwrap();
    std::fs::write(dir.join("exports").join("G__2026-10-22__14-05-09.mp4"), vec![0u8; 99]).unwrap();
    std::fs::write(dir.join("thumbs").join("a-1783463424-thumb.jpg"), b"j").unwrap();
    let l = scan(&dir).unwrap();
    let mut clips: Vec<(String, u64)> = l.clips.iter().map(|c| (c.name.clone(), c.bytes)).collect();
    clips.sort();
    assert_eq!(clips, vec![(a.into(), 10), (b.into(), 20)], ".part, other videos and exports\\ are not clips");
    assert!(l.names.iter().any(|n| n == "holiday.mp4"), "still listed (sidecar lookups), never a clip");
    assert_eq!(l.thumbs, vec!["a-1783463424-thumb.jpg".to_string()]);
    assert!(l.clips.iter().all(|c| c.modified > 1_700_000_000));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cleaned_event_shape() {
    let v: Value = serde_json::from_str(&cleaned_event(&Outcome { removed: 3, freed: 7 * GIB })).unwrap();
    assert_eq!(v, json!({"type":"storage_cleaned","removed":3,"freed_bytes":7 * GIB}));
}
