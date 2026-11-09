//! Adopting, making and linking game folders (the marker itself: `game_folder_marker_tests.rs`).
//! Temp folders only.
use super::*;

struct Gone(PathBuf);
impl Drop for Gone {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp(tag: &str) -> (PathBuf, Gone) {
    let dir = std::env::temp_dir().join(format!("recall-rule-{tag}-{}-{:?}", std::process::id(), std::thread::current().id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    (dir.clone(), Gone(dir))
}

#[test]
fn choices_and_keys() {
    assert_eq!(Choice::parse("merge"), Some(Choice::Merge));
    assert_eq!(Choice::parse(" separate "), Some(Choice::Separate));
    assert_eq!(Choice::parse(""), None);
    assert_eq!(Choice::parse("Merge"), None);
    assert_eq!(choice_key("Rocket League"), "rocket league");
    assert_eq!(choice_key(&game_folder_name("VALORANT")), "valorant");
}

/// (c) A marker that can't be parsed: not a game folder (nothing in it counted or owned).
#[test]
fn a_corrupt_marker_is_not_a_game_folder() {
    let (dir, _gone) = temp("corrupt");
    let game = dir.join("VALORANT");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::write(game.join(MARKER), b"\x00\x00\x00").unwrap();
    assert!(has_marker(&game), "the file is there");
    assert!(!is_game_folder(&game));
    assert_eq!(game_folder_marker(&game), None);
    assert!(game_folders(&dir).is_empty());
    assert_eq!(link_refusal(&game, &dir), None, "not a game folder: links like any folder");
    adopt(&game).unwrap();
    assert!(!is_game_folder(&game), "adopt keeps a marker already there");
}

#[test]
fn adopting_lists_what_is_there_and_touches_nothing_else() {
    let (dir, _gone) = temp("adopt");
    let game = dir.join("VALORANT");
    std::fs::create_dir_all(game.join("sub")).unwrap();
    std::fs::write(game.join("VALORANT__2025-01-01__10-00-00.mp4"), b"old clip").unwrap();
    std::fs::write(game.join("ShadowPlay 2025.01.01.mp4"), b"theirs").unwrap();
    adopt(&game).unwrap();
    let marker = game_folder_marker(&game).expect("a game folder now");
    assert!(!marker.owns("VALORANT__2025-01-01__10-00-00.mp4"), "there before: the user's");
    assert!(!marker.owns("shadowplay 2025.01.01.mp4"));
    assert!(!marker.owns("VALORANT__2026-11-10__04-00-00-000__ab.mp4"), "not saved by Recall yet: not Recall's");
    assert_eq!(std::fs::read(game.join("VALORANT__2025-01-01__10-00-00.mp4")).unwrap(), b"old clip");
    let mut names: Vec<String> = std::fs::read_dir(&game).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, vec![MARKER.to_string(), "ShadowPlay 2025.01.01.mp4".into(), "VALORANT__2025-01-01__10-00-00.mp4".into(), "sub".into()], "no temp file left");
    // A second adopt keeps the first marker; a clip the user drops in stays the user's.
    std::fs::write(game.join("VALORANT__2026-11-10__05-00-00-000__cd.mp4"), b"new").unwrap();
    adopt(&game).unwrap();
    assert!(!game_folder_marker(&game).unwrap().owns("VALORANT__2026-11-10__05-00-00-000__cd.mp4"));
    let made = dir.join("Rocket League");
    make(&made).unwrap();
    let marker = game_folder_marker(&made).unwrap();
    assert_eq!((marker.kind, marker.saved.len()), (Kind::Made, 0), "made: bound, nothing saved");
    assert_eq!(marker.binding.map(|b| path_key(Path::new(&b.folder))), Some(path_key(&made)));
}

/// (A2) An entry that can't be read fails the adoption; no marker.
#[test]
fn an_unreadable_entry_fails_the_adoption() {
    let (dir, _gone) = temp("badentry");
    let failing = std::iter::once(Err(std::io::Error::other("entry unreadable")));
    assert!(adopt_listing(&dir, failing).is_err());
    assert!(!has_marker(&dir));
}

#[cfg(windows)]
#[test]
fn links_and_hidden_folders_are_never_adopted() {
    let (dir, _gone) = temp("noadopt");
    let target = dir.join("target");
    std::fs::create_dir_all(&target).unwrap();
    let link = dir.join("Apex Legends");
    if junction_for_test(&target, &link) {
        assert!(adopt(&link).is_err());
        assert!(!has_marker(&target), "nothing written through the link");
    } else {
        eprintln!("skipped: could not make a junction");
    }
    let hidden = dir.join("Hidden");
    std::fs::create_dir_all(&hidden).unwrap();
    if hide_for_test(&hidden) {
        assert!(adopt(&hidden).is_err());
        assert!(!has_marker(&hidden));
    }
}

/// (R3) Only a game folder of the current recordings folder is refused as a link; a marked copy
/// elsewhere links like any folder.
#[test]
fn only_the_recordings_folders_own_game_folders_refuse_a_link() {
    let (dir, _gone) = temp("linkonly");
    let recordings = dir.join("Recall");
    std::fs::create_dir_all(&recordings).unwrap();
    let game = recordings.join("Apex Legends");
    make(&game).unwrap();
    assert_eq!(link_refusal(&game, &recordings), Some(LINK_REFUSAL));
    let backup = dir.join("Backup").join("Apex Legends");
    std::fs::create_dir_all(&backup).unwrap();
    std::fs::copy(game.join(MARKER), backup.join(MARKER)).unwrap();
    assert_eq!(link_refusal(&backup, &dir.join("Backup")), None, "a copy: its marker names another folder");
    assert_eq!(link_refusal(&backup, &recordings), None, "not inside the recordings folder");
    assert_eq!(link_refusal(&game, &dir), None, "another recordings folder");
}
