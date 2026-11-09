//! Markers: texts, binding to the exact folder, the `saved` and `writing` lists, the cap, the
//! lock. Temp folders only (`%TEMP%`, NTFS here: bound by ID too).
use super::super::{adopt, has_marker, is_game_folder, make, write_marker};
use super::*;
use std::path::PathBuf;

struct Gone(PathBuf);
impl Drop for Gone {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp(tag: &str) -> (PathBuf, Gone) {
    let dir = std::env::temp_dir().join(format!("recall-marker-{tag}-{}-{:?}", std::process::id(), std::thread::current().id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    (dir.clone(), Gone(dir))
}

const CLIP: &str = "G__2026-11-10__04-00-00-000__ab.mp4";

/// A made folder `G` in `dir` with `CLIP` saved by Recall.
fn saved_folder(dir: &Path) -> PathBuf {
    let game = dir.join("G");
    make(&game).unwrap();
    std::fs::write(game.join(CLIP), b"clip").unwrap();
    record_saved(&game.join(CLIP), None).unwrap();
    game
}

fn owns(dir: &Path, name: &str) -> bool {
    game_folder_marker(dir).is_some_and(|m| m.owns(name))
}

#[test]
fn marker_texts() {
    let legacy = Marker::new(Kind::Made, None);
    for text in ["", r#"{"version":1}"#, "Recall made this game folder. Recall counts and manages clips here.\r\n", r#"{"version":1,"saved":["G.mp4"]}"#] {
        assert_eq!(Marker::parse(text), Some(legacy.clone()), "{text:?}: legacy, owns nothing");
    }
    assert!(!Marker::parse(r#"{"version":1,"saved":["G.mp4"]}"#).unwrap().owns("g.mp4"), "an unbound list means nothing");
    let id = Some(FileId { volume: 7, index: 1 << 40 });
    let bound = Marker { saved: vec!["g.mp4".into()], writing: vec!["g__rec.mp4.part".into()], ..Marker::new(Kind::Made, Some(Binding { folder: r"C:\R\G".into(), id })) };
    assert_eq!(Marker::parse(&bound.to_json()), Some(bound.clone()));
    assert!(bound.owns("G.MP4") && !bound.owns("g__rec.mp4.part") && bound.is_writing("G__rec.mp4.part"));
    let adopted = Marker { existing: vec!["a.mp4".into()], ..Marker::new(Kind::Adopted, Some(Binding { folder: r"C:\R\V".into(), id: None })) };
    assert_eq!(Marker::parse(&adopted.to_json()), Some(adopted.clone()));
    assert!(!adopted.owns("a.mp4"), "there at adoption: the user's");
    for bad in ["garbage", "{", r#"{"version":2}"#, r#"{"adopted":true}"#, r#"{"version":1,"adopted":true}"#, r#"{"version":1,"adopted":"yes"}"#, r#"{"version":1,"folder":3}"#, r#"{"version":1,"folder":"C:\\G","id":"x"}"#, r#"{"version":1,"folder":"C:\\G","saved":[2]}"#, r#"{"version":1,"folder":"C:\\G","id":{"volume":1}}"#] {
        assert_eq!(Marker::parse(bad), None, "{bad}");
    }
}

/// (R1) A copy, a renamed folder and a folder swapped in under the same name carry a marker that
/// names another folder: not game folders, nothing owned. The original stays Recall's.
#[test]
fn a_marker_belongs_to_its_own_folder_only() {
    let (dir, _gone) = temp("bound");
    let game = saved_folder(&dir);
    assert!(owns(&game, CLIP), "bound, saved: Recall's");
    assert!(game_folder_marker(&game).unwrap().binding.unwrap().id.is_some(), "NTFS: bound by ID too");
    let copy = dir.join("G - Copy");
    std::fs::create_dir_all(&copy).unwrap();
    for name in [MARKER, CLIP] {
        std::fs::copy(game.join(name), copy.join(name)).unwrap();
    }
    assert!(game_folder_marker(&copy).is_none() && !is_game_folder(&copy), "a copy");
    let renamed = dir.join("G old");
    std::fs::rename(&game, &renamed).unwrap();
    assert!(game_folder_marker(&renamed).is_none(), "renamed");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::copy(renamed.join(MARKER), game.join(MARKER)).unwrap();
    assert!(game_folder_marker(&game).is_none(), "same path, another folder (ID)");
    assert!(read_marker(&game).is_some(), "the marker itself still parses");
}

/// (R1) A volume without a stable ID: the marker stores `"id": null` and is bound by path only.
#[test]
fn without_an_id_the_marker_is_bound_by_path_only() {
    let (dir, _gone) = temp("pathonly");
    let game = dir.join("G");
    std::fs::create_dir_all(&game).unwrap();
    let binding = bind_with(&game, &|_| None);
    assert_eq!(binding.id, None);
    assert!(take_notes().iter().any(|l| l.contains("bound by path only")), "said once");
    write_new_marker(&game, &Marker::new(Kind::Made, Some(binding))).unwrap();
    assert!(std::fs::read_to_string(game.join(MARKER)).unwrap().contains(r#""id":null"#));
    let marker = read_marker(&game).unwrap();
    assert!(bound_to(&marker, &game, &|_| None) && bound_to(&marker, &game, &folder_id), "any ID: the path decides");
    assert!(!bound_to(&marker, &dir.join("G2"), &|_| None), "another path");
    assert!(game_folder_marker(&game).is_some());
    let stored = Marker::new(Kind::Made, Some(Binding { folder: game.to_string_lossy().into_owned(), id: Some(FileId { volume: 1, index: 2 }) }));
    assert!(!bound_to(&stored, &game, &|_| None), "an ID was stored and none is found now");
}

/// (R2) A legacy marker owns nothing, also clips that sit there; the next save binds it and only
/// what is saved from then on is Recall's.
#[test]
fn a_legacy_marker_owns_nothing_until_a_save_binds_it() {
    let (dir, _gone) = temp("legacy");
    let game = dir.join("G");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::write(game.join(MARKER), b"").unwrap();
    std::fs::write(game.join(CLIP), b"old").unwrap();
    let marker = game_folder_marker(&game).expect("still a game folder");
    assert!(marker.binding.is_none() && !marker.owns(CLIP));
    let new = game.join("G__2026-11-10__05-00-00-000__cd.mp4");
    std::fs::write(&new, b"new").unwrap();
    record_saved(&new, None).unwrap();
    let marker = game_folder_marker(&game).unwrap();
    assert!(marker.binding.is_some(), "bound by the save");
    assert!(marker.owns("G__2026-11-10__05-00-00-000__cd.mp4") && !marker.owns(CLIP));
}

/// (R2) Owned = saved after the final rename, in made and adopted folders alike: a file the user
/// drops in, a name recorded before its file exists, and a `.part` are never owned; the `.part`
/// of a long recording is only on `writing`, and its final name and `rec-NN` parts are saved.
#[test]
fn only_names_saved_after_their_rename_are_owned() {
    let (dir, _gone) = temp("saved");
    let game = saved_folder(&dir);
    let dropped = "G__2026-11-10__06-00-00-000__ef.mp4";
    std::fs::write(game.join(dropped), b"the user's").unwrap();
    assert!(owns(&game, CLIP) && !owns(&game, dropped));
    let early = game.join("G__2026-11-10__07-00-00-000__aa.mp4");
    record_saved(&early, None).unwrap();
    assert!(!owns(&game, "G__2026-11-10__07-00-00-000__aa.mp4"), "no file yet: not saved");
    let part = game.join("G__2026-11-10__08-00-00__rec.mp4.part");
    std::fs::write(&part, b"rec").unwrap();
    assert_eq!(record_saved(&part, None).unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
    record_writing(&part).unwrap();
    let marker = game_folder_marker(&game).unwrap();
    assert!(marker.is_writing("G__2026-11-10__08-00-00__rec.mp4.part") && !marker.owns("G__2026-11-10__08-00-00__rec.mp4.part"));
    let done = game.join("G__2026-11-10__08-00-00__rec.mp4");
    std::fs::rename(&part, &done).unwrap();
    record_saved(&done, Some(&part)).unwrap();
    let second = game.join("G__2026-11-10__08-00-00__rec-2.mp4");
    std::fs::write(&second, b"part 2").unwrap();
    record_saved(&second, None).unwrap();
    let marker = game_folder_marker(&game).unwrap();
    assert!(marker.owns("G__2026-11-10__08-00-00__rec.mp4") && marker.owns("G__2026-11-10__08-00-00__rec-2.mp4"));
    assert!(marker.writing.is_empty(), "the part left `writing`");
    // Adopted: the same rule.
    let user = dir.join("V");
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(user.join(CLIP), b"there before").unwrap();
    adopt(&user).unwrap();
    let ours = user.join("G__2026-11-10__09-00-00-000__bb.mp4");
    std::fs::write(&ours, b"saved").unwrap();
    record_saved(&ours, None).unwrap();
    assert!(!owns(&user, CLIP) && owns(&user, "G__2026-11-10__09-00-00-000__bb.mp4"));
    // A folder with no marker: nothing written.
    record_saved(&dir.join("x.mp4"), None).unwrap();
    assert!(!has_marker(&dir));
    let names: Vec<String> = std::fs::read_dir(&game).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert!(names.iter().all(|n| !n.contains(".tmp-")), "no temp file left: {names:?}");
}

/// (R2) The replace is atomic: a failed write leaves the old marker whole, the clip unowned, and
/// the folder refused for the rest of the run (said once).
#[test]
fn a_failed_write_keeps_the_old_marker_and_refuses_the_folder() {
    let (dir, _gone) = temp("atomic");
    let game = saved_folder(&dir);
    let before = std::fs::read(game.join(MARKER)).unwrap();
    // The temp file's name is taken by a folder: the write fails before the rename.
    std::fs::create_dir_all(game.join(format!("{MARKER}.tmp-{}", std::process::id()))).unwrap();
    let next = game.join("G__2026-11-10__05-00-00-000__cd.mp4");
    std::fs::write(&next, b"clip").unwrap();
    assert!(record_saved(&next, None).is_err());
    assert_eq!(std::fs::read(game.join(MARKER)).unwrap(), before, "the old marker, whole");
    assert!(owns(&game, CLIP) && !owns(&game, "G__2026-11-10__05-00-00-000__cd.mp4"));
    assert!(save_refused(&game));
    assert!(take_notes().iter().any(|l| l.contains("new clips for it go to the clip folder")));
    std::fs::remove_dir(game.join(format!("{MARKER}.tmp-{}", std::process::id()))).unwrap();
    assert!(record_saved(&next, None).is_err(), "refused for the run, no retry");
}

/// (A3, R2) Over 4 MiB a marker is not read; a save that would pass the cap is refused and the
/// clip stays unowned.
#[test]
fn the_marker_cap() {
    let (dir, _gone) = temp("cap");
    let game = dir.join("G");
    std::fs::create_dir_all(&game).unwrap();
    let mut text = br#"{"version":1}"#.to_vec();
    text.resize(MARKER_MAX as usize + 1, b' ');
    std::fs::write(game.join(MARKER), &text).unwrap();
    assert_eq!(read_marker(&game), None);
    assert!(!is_game_folder(&game));
    text.truncate(MARKER_MAX as usize);
    std::fs::write(game.join(MARKER), &text).unwrap();
    assert!(read_marker(&game).is_some(), "at the cap: read");
    // An adopted marker close to the cap (its `existing` list is never pruned).
    let user = dir.join("V");
    std::fs::create_dir_all(&user).unwrap();
    let mut big = Marker::new(Kind::Adopted, Some(bind(&user)));
    big.existing = vec![String::new()];
    let base = big.to_json().len();
    big.existing[0] = "z".repeat(MARKER_MAX as usize - base - 30);
    write_new_marker(&user, &big).unwrap();
    let clip = user.join("G__2026-11-10__05-00-00-000__cd.mp4");
    std::fs::write(&clip, b"clip").unwrap();
    assert!(record_saved(&clip, None).is_err(), "would pass 4 MiB");
    assert!(!owns(&user, "G__2026-11-10__05-00-00-000__cd.mp4") && save_refused(&user));
}

/// (R2) Two threads saving into one folder at once lose no names (one process-wide lock).
#[test]
fn concurrent_saves_lose_no_names() {
    let (dir, _gone) = temp("threads");
    let game = dir.join("G");
    make(&game).unwrap();
    let names: Vec<Vec<PathBuf>> = (0..2).map(|t| (0..40).map(|i| game.join(format!("G__2026-11-10__04-00-{t}{i:02}-000__ab.mp4"))).collect()).collect();
    for path in names.iter().flatten() {
        std::fs::write(path, b"c").unwrap();
    }
    std::thread::scope(|s| {
        for list in &names {
            s.spawn(move || list.iter().for_each(|p| record_saved(p, None).unwrap()));
        }
    });
    let marker = game_folder_marker(&game).unwrap();
    assert_eq!(marker.saved.len(), 80);
    assert!(names.iter().flatten().all(|p| marker.owns(&p.file_name().unwrap().to_string_lossy())));
}

/// `write_marker` keeps a marker that is there.
#[test]
fn write_marker_keeps_what_is_there() {
    let (dir, _gone) = temp("keep");
    std::fs::write(dir.join(MARKER), b"").unwrap();
    write_marker(&dir).unwrap();
    assert_eq!(std::fs::read(dir.join(MARKER)).unwrap(), b"");
}
