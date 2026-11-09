//! Storage-cap selection with injected listings (no real deletes; the one
//! filesystem test lists a temp dir and recycles nothing).

use super::*;

/// Nothing became a favourite, was played or opened during the run.
fn no_keeps() -> Result<HashSet<String>, String> {
    Ok(HashSet::new())
}

/// A Recycle Bin that takes anything (the bin checks have their own tests).
fn any_bin(_: &Path, _: u64) -> Result<(), String> {
    Ok(())
}

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

fn set_mtime(path: &Path, unix: u64) {
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(unix)).unwrap();
}

#[test]
fn a_bad_copy_newer_than_the_library_cancels_the_run() {
    let dir = std::env::temp_dir().join(format!("recall-storage-lib-{}-{}", std::process::id(), line!()));
    std::fs::create_dir_all(&dir).unwrap();
    let lib = dir.join("library.json");
    // The app quarantined a corrupt file, then wrote a fresh one without the favourites.
    std::fs::write(&lib, b"{}").unwrap();
    set_mtime(&lib, 2_000);
    for name in ["library.json.bad", "library.json.bad-2026-11-07"] {
        let bad = dir.join(name);
        std::fs::write(&bad, br#"{"G__2026-10-22__14-05-09.mp4":{"favourite":true}"#).unwrap();
        set_mtime(&bad, 3_000);
        let err = favourites_at(&lib).expect_err("a .bad newer than the library = favourites unknown");
        assert!(err.contains("quarantined"), "{err}");
        set_mtime(&bad, 1_000);
    }
    let favs = favourites_at(&lib).expect("the library was written after every .bad: known");
    assert!(favs.contains("g__2026-10-22__14-05-09.mp4"), "the .bad copies' favourites still count: {favs:?}");
    let _ = std::fs::remove_dir_all(&dir);
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
    set_mtime(&dir.join("library.json.bad"), 1_000);
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
    let err = run_once(Path::new(r"Z:\does-not-matter"), STORAGE_CAP_MIN_GB - 1, plan_dir, (&|| true, no_keeps), (any_bin, no_files), |_| panic!("no recycle"))
        .expect_err("refused");
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
fn library_favourites_keyed_by_path_protect_the_clip_by_name() {
    let favs = favourites(br#"{"x:\\recall\\valorant__2026-10-22__14-05-09.mp4":{"favourite":true},"d:\\old\\x.mp4":{"title":"t"},"Apex__2026-10-01__10-00-00.mp4":{"favourite":true}}"#).unwrap();
    let mut names: Vec<_> = favs.into_iter().collect();
    names.sort();
    assert_eq!(names, ["apex__2026-10-01__10-00-00.mp4", "valorant__2026-10-22__14-05-09.mp4"]);
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

/// "Sort clips into game folders": clips one level down count and go with their own
/// thumbnails and sidecars; Recall's own subfolders and anything deeper never count.
#[test]
fn scan_counts_game_folder_clips_one_level_down() {
    struct Gone(PathBuf);
    impl Drop for Gone {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let dir = std::env::temp_dir().join(format!("recall-storage-games-{}-{}", std::process::id(), line!()));
    let _gone = Gone(dir.clone());
    let game = dir.join("Rocket League");
    for d in [game.join("thumbs"), game.join("old"), dir.join("screenshots"), dir.join("exports"), dir.join(".hidden")] {
        std::fs::create_dir_all(d).unwrap();
    }
    let top = "Desktop__2026-10-22__14-05-09.mp4";
    let rl = "Rocket-League__2026-10-22__15-00-00-275__ab12.mp4";
    std::fs::write(dir.join(top), vec![0u8; 10]).unwrap();
    std::fs::write(game.join(rl), vec![0u8; 20]).unwrap();
    std::fs::write(game.join("Rocket-League__2026-10-22__15-00-00-275__ab12.recall-marks.json"), b"{}").unwrap();
    std::fs::write(game.join("thumbs").join("Rocket-League__2026-10-22__15-00-00-275__ab12-1783463424-thumb.jpg"), b"j").unwrap();
    std::fs::write(game.join("old").join("G__2026-10-22__16-00-00.mp4"), vec![0u8; 99]).unwrap();
    std::fs::write(dir.join("screenshots").join("G__2026-10-22__16-00-00.mp4"), vec![0u8; 99]).unwrap();
    std::fs::write(dir.join("exports").join("G__2026-10-22__16-00-00.mp4"), vec![0u8; 99]).unwrap();
    std::fs::write(dir.join(".hidden").join("G__2026-10-22__16-00-00.mp4"), vec![0u8; 99]).unwrap();
    let l = scan(&dir).unwrap();
    let mut clips: Vec<(String, u64)> = l.clips.iter().map(|c| (c.name.clone(), c.bytes)).collect();
    clips.sort();
    assert_eq!(clips, vec![(top.into(), 10), (format!("Rocket League/{rl}"), 20)]);
    let rel = format!("Rocket League/{rl}");
    assert_eq!(at(&dir, &rel), game.join(rl));
    assert_eq!(
        companions(&dir, &rel, &l),
        vec![
            game.join(rl),
            game.join("Rocket-League__2026-10-22__15-00-00-275__ab12.recall-marks.json"),
            game.join("thumbs").join("Rocket-League__2026-10-22__15-00-00-275__ab12-1783463424-thumb.jpg"),
        ]
    );
    assert_eq!(companions(&dir, top, &l), vec![dir.join(top)], "a game folder's thumbnails never go with a top-level clip");
}

#[test]
fn game_folder_clips_keep_favourites_and_edits() {
    let l = listing(
        vec![clip("A/G__2026-10-22__14-05-09.mp4", 3, 100), clip("B/G__2026-10-22__14-05-10.mp4", 3, 200), clip("G__2026-10-22__14-05-11.mp4", 3, 300), clip("G__2026-10-22__14-05-12.mp4", 3, 400)],
        &["B/G__2026-10-22__14-05-10.mp4.recall-edit.json"],
        &[],
    );
    let favs: HashSet<String> = ["g__2026-10-22__14-05-09.mp4".to_string()].into();
    let p = plan(&l, &favs, 3 * GIB, NOW);
    assert_eq!(p.remove, vec!["G__2026-10-22__14-05-11.mp4".to_string()], "A's clip is a favourite, B's has edits, the newest stays");
    assert!(is_game_folder("Rocket League") && is_game_folder("Desktop"));
    assert!(!is_game_folder("thumbs") && !is_game_folder("Screenshots") && !is_game_folder("exports") && !is_game_folder(".x") && !is_game_folder("$RECYCLE.BIN"));
}

#[test]
fn cleaned_event_shape() {
    let v: Value = serde_json::from_str(&cleaned_event(&Outcome { removed: 3, freed: 7 * GIB, through: Some(1_700_000_000), used: 90 * GIB, cap: 100 * GIB, next: Some(1_700_000_100), stuck: false, refused: None })).unwrap();
    assert_eq!(
        v,
        json!({"type":"storage_cleaned","removed":3,"freed_bytes":7 * GIB,"through":1_700_000_000u64,"used_bytes":90 * GIB,"cap_bytes":100 * GIB})
    );
}

#[test]
fn warning_and_preview_event_shapes() {
    let o = Outcome { removed: 0, freed: 0, through: None, used: 85 * GIB, cap: 100 * GIB, next: Some(500), stuck: false, refused: None };
    let v: Value = serde_json::from_str(&warning_event(80, &o)).unwrap();
    assert_eq!(v, json!({"type":"storage_warning","level":80,"used_bytes":85 * GIB,"cap_bytes":100 * GIB,"next_through":500}));
    let o = Outcome { next: None, ..o };
    let v: Value = serde_json::from_str(&warning_event(100, &o)).unwrap();
    assert_eq!(v["next_through"], Value::Null);
    let l = listing(vec![clip("a.mp4", 3, 100), clip("b.mp4", 3, 200), clip("c.mp4", 3, 300)], &[], &[]);
    let p = plan(&l, &HashSet::new(), 4 * GIB, NOW);
    let v: Value = serde_json::from_str(&preview_event(500, &p)).unwrap();
    assert_eq!(v, json!({"type":"storage_preview","cap_gb":500,"used_bytes":9 * GIB,"removed":2,"freed_bytes":6 * GIB,"through":200}));
}

#[test]
fn plan_names_the_cut_off_date_and_the_next_clip_out() {
    // 10 GiB, cap 6: a (100) and b (200) go -> "200 and earlier"; c (300) is next.
    let l = listing(vec![clip("d.mp4", 2, 400), clip("a.mp4", 2, 100), clip("c.mp4", 2, 300), clip("b.mp4", 2, 200), clip("e.mp4", 2, 500)], &[], &[]);
    let p = plan(&l, &HashSet::new(), 6 * GIB, NOW);
    assert_eq!(p.remove, vec!["a.mp4", "b.mp4"]);
    assert_eq!((p.through, p.next), (Some(200), Some(300)));
    assert!(!p.stuck(6 * GIB));
    // Under the cap: nothing goes, but the next clip out is still the oldest.
    let p = plan(&l, &HashSet::new(), 20 * GIB, NOW);
    assert_eq!((p.remove.len(), p.through, p.next), (0, None, Some(100)));
    // Favourites skipped: the next out is the oldest unprotected clip.
    let favs: HashSet<String> = ["a.mp4".to_string()].into();
    assert_eq!(plan(&l, &favs, 20 * GIB, NOW).next, Some(200));
}

#[test]
fn stuck_when_over_the_cap_and_nothing_may_go() {
    let favs: HashSet<String> = ["a.mp4".to_string(), "b.mp4".to_string()].into();
    // c is the newest (never goes); a and b are favourites.
    let l = listing(vec![clip("a.mp4", 5, 100), clip("b.mp4", 5, 200), clip("c.mp4", 5, 300)], &[], &[]);
    let p = plan(&l, &favs, 10 * GIB, NOW);
    assert!(p.remove.is_empty() && p.next.is_none());
    assert!(p.stuck(10 * GIB));
    assert!(!plan(&l, &favs, 20 * GIB, NOW).stuck(20 * GIB), "under the cap is never stuck");
}

#[test]
fn warner_warns_once_per_crossing_and_rearms_below_it() {
    let cap = 100 * GIB;
    let mut w = Warner::default();
    assert_eq!(w.update(70 * GIB, cap, false, false), None);
    assert_eq!(w.update(80 * GIB, cap, false, false), Some(80));
    assert_eq!(w.update(85 * GIB, cap, false, false), None, "same level: quiet");
    assert_eq!(w.update(91 * GIB, cap, false, false), Some(90));
    assert_eq!(w.update(99 * GIB, cap, false, false), None);
    // A prune toasts on its own; the level is recorded without a second toast.
    assert_eq!(w.update(100 * GIB, cap, false, true), None);
    assert_eq!(w.update(87 * GIB, cap, false, false), None, "3 points under 90: not re-armed");
    assert_eq!(w.update(92 * GIB, cap, false, false), None);
    assert_eq!(w.update(84 * GIB, cap, false, false), None, "re-armed at 80");
    assert_eq!(w.update(90 * GIB, cap, false, false), Some(90), "warns again after falling well below");
    // Stuck over the cap warns once; leaving that state re-arms it.
    assert_eq!(w.update(110 * GIB, cap, true, false), Some(LEVEL_STUCK));
    assert_eq!(w.update(110 * GIB, cap, true, false), None);
    assert_eq!(w.update(95 * GIB, cap, false, true), None);
    assert_eq!(w.update(105 * GIB, cap, true, false), Some(LEVEL_STUCK));
    // A new cap starts over, so choosing a limit tells where usage stands.
    assert_eq!(w.update(95 * GIB, 110 * GIB, false, false), Some(80));
    assert_eq!(w.update(95 * GIB, 0, false, false), None, "no limit: no warnings");
}

/// The dry run (`storage_preview`) chooses clips but moves, renames and rewrites nothing.
#[test]
fn a_dry_run_leaves_the_folder_as_it_was() {
    let dir = std::env::temp_dir().join(format!("recall-hdr-preview-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("thumbs")).unwrap();
    let names = ["G__2026-01-01__10-00-00.mp4", "G__2026-01-02__10-00-00.mp4", "G__2026-01-03__10-00-00.mp4"];
    for (i, n) in names.iter().enumerate() {
        let f = std::fs::File::create(dir.join(n)).unwrap();
        f.set_len(100).unwrap();
        f.set_modified(UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000 + i as u64 * 60)).unwrap();
    }
    std::fs::write(dir.join("thumbs").join("G__2026-01-01__10-00-00-thumb.jpg"), b"j").unwrap();
    let snap = |d: &Path| {
        let mut v: Vec<(String, u64, SystemTime)> = std::fs::read_dir(d)
            .unwrap()
            .chain(std::fs::read_dir(d.join("thumbs")).unwrap())
            .flatten()
            // Files only: NTFS updates a folder's own time lazily after the setup writes.
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .map(|e| {
                let m = e.metadata().unwrap();
                (e.path().display().to_string(), m.len(), m.modified().unwrap())
            })
            .collect();
        v.sort();
        v
    };
    let before = snap(&dir);
    let (plan, _) = plan_listing(&dir, &HashSet::new(), 150).unwrap();
    assert_eq!(plan.remove.len(), 2, "the dry run would move the two oldest");
    assert_eq!(snap(&dir), before, "every file, size and time unchanged");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- One Recycle Bin call per clip, the setting re-read between calls (no real files) ----

use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Mutex;

const A: &str = "G__2026-01-01__00-00-00.mp4";
const B: &str = "G__2026-01-02__00-00-00.mp4";
const C: &str = "G__2026-01-03__00-00-00.mp4";
const A_THUMB: &str = "G__2026-01-01__00-00-00-1767225600-thumb.jpg";

/// Three old clips to go; A has a thumbnail and both sidecars.
fn plan_three(_: &Path, _: u32) -> Result<(Plan, Listing), String> {
    let clips = vec![clip(A, 1, 100), clip(B, 1, 200), clip(C, 1, 300)];
    let names = [A, B, C, "G__2026-01-01__00-00-00.recall-edit.json", "G__2026-01-01__00-00-00.mp4.recall-marks.json"];
    let listing = Listing { clips, names: names.map(String::from).to_vec(), thumbs: vec![A_THUMB.into()] };
    let plan = Plan { remove: vec![A.into(), B.into(), C.into()], freed: 3 * GIB, total: 600 * GIB, through: Some(300), next: None };
    Ok((plan, listing))
}

fn names_of(files: &[PathBuf]) -> Vec<String> {
    files.iter().map(|f| f.file_name().unwrap().to_string_lossy().into_owned()).collect()
}

static FLIP_CALLS: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());
static LIMIT_ON: AtomicBool = AtomicBool::new(true);

/// Records the call, then the user turns the limit off (between clip 1 and clip 2).
fn recycle_then_limit_off(files: &[PathBuf]) -> Result<(), crate::record_storage::RecycleError> {
    FLIP_CALLS.lock().unwrap().push(names_of(files));
    LIMIT_ON.store(false, SeqCst);
    Ok(())
}

#[test]
fn turning_the_limit_off_between_clips_stops_the_run() {
    let wanted = || LIMIT_ON.load(SeqCst);
    let out = run_once(Path::new(r"E:\Clips"), 500, plan_three, (&wanted, no_keeps), (any_bin, no_files), recycle_then_limit_off).unwrap();
    let calls = FLIP_CALLS.lock().unwrap().clone();
    let clips: Vec<&String> = calls.iter().flatten().filter(|n| n.ends_with(".mp4")).collect();
    assert_eq!(clips, vec![A], "exactly one clip recycled: {calls:?}");
    assert_eq!((out.removed, out.freed, out.through), (1, GIB, Some(100)));
}

static GROUP_CALLS: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());

fn recycle_recorded(files: &[PathBuf]) -> Result<(), crate::record_storage::RecycleError> {
    GROUP_CALLS.lock().unwrap().push(names_of(files));
    Ok(())
}

#[test]
fn a_clip_and_its_companions_go_in_one_call() {
    let out = run_once(Path::new(r"E:\Clips"), 500, plan_three, (&|| true, no_keeps), (any_bin, no_files), recycle_recorded).unwrap();
    let mut calls = GROUP_CALLS.lock().unwrap().clone();
    for call in &mut calls {
        call.sort();
    }
    let mut a_call = vec![A.to_string(), "G__2026-01-01__00-00-00.recall-edit.json".into(), "G__2026-01-01__00-00-00.mp4.recall-marks.json".into(), A_THUMB.into()];
    a_call.sort();
    assert_eq!(calls, vec![a_call, vec![B.to_string()], vec![C.to_string()]], "one call per clip, companions with their clip");
    assert_eq!(out.removed, 3);
}

// ---- Never a permanent delete: a clip that cannot go to the Recycle Bin stops the run ----

const BIG: &str = "G__2026-01-02__00-00-00.mp4";

/// A (1 GiB), then B (60 GiB, bigger than the bin), then C (1 GiB).
fn plan_big_middle(_: &Path, _: u32) -> Result<(Plan, Listing), String> {
    let clips = vec![clip(A, 1, 100), clip(BIG, 60, 200), clip(C, 1, 300)];
    let listing = Listing { clips, names: [A, BIG, C].map(String::from).to_vec(), thumbs: Vec::new() };
    let plan = Plan { remove: vec![A.into(), BIG.into(), C.into()], freed: 62 * GIB, total: 600 * GIB, through: Some(300), next: None };
    Ok((plan, listing))
}

/// A bin that holds at most 10 GiB.
fn ten_gib_bin(_: &Path, bytes: u64) -> Result<(), String> {
    if bytes > 10 * GIB { Err("the clip is bigger than the Recycle Bin".into()) } else { Ok(()) }
}

static BIG_CALLS: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());
fn recycle_big(files: &[PathBuf]) -> Result<(), crate::record_storage::RecycleError> {
    BIG_CALLS.lock().unwrap().push(names_of(files));
    Ok(())
}

#[test]
fn a_clip_too_big_for_the_bin_is_skipped_and_the_run_stops() {
    let out = run_once(Path::new(r"E:\Clips"), 500, plan_big_middle, (&|| true, no_keeps), (ten_gib_bin, no_files), recycle_big).unwrap();
    assert_eq!(BIG_CALLS.lock().unwrap().clone(), vec![vec![A.to_string()]], "A went, B was refused, C was never tried");
    assert_eq!(out.removed, 1);
    assert!(out.refused.as_deref().is_some_and(|r| r.contains("bigger than the Recycle Bin")), "{out:?}");
}

/// What `win_recycle::facts` reports for a `\\nas` share: no volume GUID.
fn share_bin(_: &Path, bytes: u64) -> Result<(), String> {
    let share = crate::win_volume::Volume { id: r"\\nas\clips\".into(), label: r"\\nas\clips".into() };
    crate::win_recycle::decide(&crate::win_recycle::BinFacts { subst: false, resolved_unc: false, volume: Some(share), drive_type: 4, total_bytes: None, bin_used: None, settings: Default::default() }, bytes)
}

static SHARE_CALLS: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());
fn recycle_share(files: &[PathBuf]) -> Result<(), crate::record_storage::RecycleError> {
    SHARE_CALLS.lock().unwrap().push(names_of(files));
    Ok(())
}

#[test]
fn clips_on_a_share_are_never_sent() {
    // A local-looking folder: the share is what `share_bin` reports (no network lookup in a test).
    let out = run_once(Path::new(r"E:\Clips"), 500, plan_three, (&|| true, no_keeps), (share_bin, no_files), recycle_share).unwrap();
    assert!(SHARE_CALLS.lock().unwrap().is_empty(), "no shell call: on a share it would delete for good");
    assert_eq!(out.removed, 0);
    assert!(out.refused.is_some());
}

// ---- A delete the IFileOperation sink refused (it would not recycle) is a refusal ----

#[test]
fn sink_refusals_and_cancels_are_refusals() {
    use crate::win_fileop::Outcome;
    assert_eq!(from_outcome(Outcome::Recycled), Ok(()));
    assert!(matches!(from_outcome(Outcome::Refused("would delete for good".into())), Err(RecycleError::Refused(_))));
    assert!(matches!(from_outcome(Outcome::Failed("access denied".into())), Err(RecycleError::Failed(_))));
}

static DECLINED_CALLS: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());

/// The nuke prompt appeared (no owner window, maybe over a game) and was declined.
fn recycle_declined(files: &[PathBuf]) -> Result<(), RecycleError> {
    DECLINED_CALLS.lock().unwrap().push(names_of(files));
    Err(RecycleError::Refused("the delete was declined".into()))
}

#[test]
fn a_declined_shell_call_stops_the_run_like_a_refusal() {
    let out = run_once(Path::new(r"E:\Clips"), 500, plan_three, (&|| true, no_keeps), (any_bin, no_files), recycle_declined).expect("a refusal, not an error");
    assert_eq!(DECLINED_CALLS.lock().unwrap().len(), 1, "no second clip after a declined call");
    assert_eq!(out.removed, 0);
    assert!(out.refused.as_deref().is_some_and(|r| r.contains("declined")), "{out:?}");
}

/// Sizes of files that are not on disk in these tests: 0 (the clip's own size comes from the listing).
fn no_files(_: &Path) -> Option<u64> {
    Some(0)
}

#[path = "record_storage_bin_tests.rs"]
mod bin_tests;
