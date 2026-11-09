use super::*;

const CLIP: &str = "Rocket-League__2026-10-22__14-05-09-275__ab.mp4";

fn marked(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    rule::write_marker(dir).unwrap();
    dir.to_path_buf()
}

fn text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[test]
fn game_folder_clips_are_recall_named_one_level_down_in_marked_folders() {
    let guard = crate::test_temp::TempDir::sandbox("games-named");
    let rec = guard.join("Raw");
    let game = marked(&rec.join("Rocket League"));
    marked(&rec.join("VALORANT"));
    std::fs::create_dir_all(rec.join("Keepers")).unwrap();
    assert!(in_game_folder(&game.join(CLIP), &rec));
    let upper = PathBuf::from(text(&rec).to_uppercase()).join("valorant").join("VALORANT__2026-10-22__14-05-09__rec.MP4");
    assert!(in_game_folder(&upper, &rec), "case-insensitive");
    assert!(!in_game_folder(&rec.join(CLIP), &rec), "top level is not a game folder");
    assert!(!in_game_folder(&game.join("holiday.mp4"), &rec), "other videos stay out");
    assert!(!in_game_folder(&rec.join("Keepers").join(CLIP), &rec), "a folder Recall did not make");
    assert!(!in_game_folder(&rec.join("Gone").join(CLIP), &rec), "a missing folder");
    assert!(!in_game_folder(&game.join("old").join(CLIP), &rec), "one level only");
    assert!(!in_game_folder(&game.join(format!("{CLIP}.part")), &rec));
}

#[test]
fn game_folders_lists_only_marked_real_visible_folders() {
    let guard = crate::test_temp::TempDir::sandbox("games-list");
    let dir = guard.to_path_buf();
    marked(&dir.join("Rocket League"));
    marked(&dir.join("VALORANT"));
    marked(&dir.join("thumbs"));
    for d in ["Keepers", "screenshots", ".x"] {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    std::fs::write(dir.join("a.mp4"), b"x").unwrap();
    assert_eq!(game_folders(&dir, &[]), vec![dir.join("Rocket League"), dir.join("VALORANT")]);
    assert_eq!(game_folders(&dir, &[text(&dir.join("VALORANT"))]), vec![dir.join("Rocket League")], "a linked one is the link's");
    assert_eq!(game_folders(&dir, &[text(&dir)]).len(), 2, "a link at the recordings folder changes nothing");
    assert!(game_folders(&dir.join("missing"), &[]).is_empty());
    if rule::hide_for_test(&dir.join("VALORANT")) {
        assert_eq!(game_folders(&dir, &[]), vec![dir.join("Rocket League")], "hidden");
    } else {
        eprintln!("skipped the hidden case: could not hide the folder");
    }
}

#[test]
fn under_link_spares_links_above_the_recordings_folder() {
    let videos = Path::new(r"X:\Videos");
    let rec = videos.join("Recall");
    let game = rec.join("VALORANT");
    assert!(!under_link(&game.join(CLIP), &rec, &[text(videos)]), "Videos linked: Recall's clips stay Recall's");
    assert!(!under_link(&game.join(CLIP), &rec, &[text(&rec)]));
    assert!(under_link(&game.join(CLIP), &rec, &[text(&game)]));
    assert!(under_link(&game, &rec, &[r"x:\videos\recall\valorant\".to_string()]), "any spelling");
    assert!(!under_link(&rec.join("VALORANT 2").join(CLIP), &rec, &[text(&game)]));
}
