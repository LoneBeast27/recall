//! Markers: made, adopted (the files already there stay the user's), unreadable (not a game
//! folder). Temp folders only.
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

#[test]
fn marker_texts() {
    assert_eq!(Marker::parse(""), Some(Marker::Made));
    assert_eq!(Marker::parse(r#"{"version":1}"#), Some(Marker::Made));
    assert_eq!(Marker::parse("Recall made this game folder. Recall counts and manages clips here.\r\n"), Some(Marker::Made), "the first build's text");
    let adopted = Marker::parse(r#"{"version":1,"adopted":true,"existing":["B.mp4","a.mp4"]}"#).unwrap();
    assert_eq!(adopted, Marker::Adopted { existing: vec!["a.mp4".into(), "b.mp4".into()] });
    assert!(!adopted.owns("A.MP4") && !adopted.owns("b.mp4"));
    assert!(adopted.owns("VALORANT__2026-11-10__04-00-00-000__ab.mp4"));
    assert_eq!(Marker::parse(&adopted.to_json()), Some(adopted));
    for bad in ["garbage", "{", r#"{"version":2}"#, r#"{"adopted":true}"#, r#"{"version":1,"adopted":true}"#, r#"{"version":1,"adopted":true,"existing":[1]}"#, r#"{"version":1,"adopted":"yes"}"#] {
        assert_eq!(Marker::parse(bad), None, "{bad}");
    }
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
    assert_eq!(link_refusal(&game), Some(LINK_REFUSAL), "still never linked");
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
    assert!(marker.owns("VALORANT__2026-11-10__04-00-00-000__ab.mp4"), "saved after: Recall's");
    assert_eq!(std::fs::read(game.join("VALORANT__2025-01-01__10-00-00.mp4")).unwrap(), b"old clip");
    let mut names: Vec<String> = std::fs::read_dir(&game).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, vec![MARKER.to_string(), "ShadowPlay 2025.01.01.mp4".into(), "VALORANT__2025-01-01__10-00-00.mp4".into(), "sub".into()], "no temp file left");
    // A second adopt keeps the first list.
    std::fs::write(game.join("VALORANT__2026-11-10__05-00-00-000__cd.mp4"), b"new").unwrap();
    adopt(&game).unwrap();
    assert!(game_folder_marker(&game).unwrap().owns("VALORANT__2026-11-10__05-00-00-000__cd.mp4"));
    let made = dir.join("Rocket League");
    make(&made).unwrap();
    assert_eq!(game_folder_marker(&made), Some(Marker::Made));
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
