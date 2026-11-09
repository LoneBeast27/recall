use super::*;

struct Gone(PathBuf);
impl Drop for Gone {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp(tag: &str) -> (PathBuf, Gone) {
    let dir = std::env::temp_dir().join(format!("recall-gamedir-{tag}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    (dir.clone(), Gone(dir))
}

fn text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[test]
fn a_new_game_folder_is_made_with_the_marker_and_reused() {
    let (out, _gone) = temp("new");
    let dir = resolve_game_dir(&text(&out), "Rocket League®");
    assert_eq!(dir, out.join("Rocket League"));
    assert!(rule::is_game_folder(&dir));
    assert_eq!(resolve_game_dir(&text(&out), "Rocket League®"), dir, "cached");
    let missing = out.join("missing");
    assert_eq!(resolve_game_dir(&text(&missing), "VALORANT"), missing, "no clip folder: no game folder");
    assert!(!missing.exists());
}

#[test]
fn an_unmarked_folder_is_never_adopted() {
    let (out, _gone) = temp("unmarked");
    let user = out.join("VALORANT");
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(user.join("VALORANT__2026-10-22__14-05-09.mp4"), b"x").unwrap();
    let dir = resolve_game_dir(&text(&out), "VALORANT");
    assert_eq!(dir, out.join("VALORANT (Recall)"));
    assert!(rule::is_game_folder(&dir), "made with its marker");
    assert!(!rule::has_marker(&user), "the user's folder stays untouched");
    assert_eq!(std::fs::read_dir(&user).unwrap().count(), 1);
    // Both names taken by the user: the clip folder itself.
    std::fs::create_dir_all(out.join("THE FINALS")).unwrap();
    std::fs::create_dir_all(out.join("THE FINALS (Recall)")).unwrap();
    assert_eq!(resolve_game_dir(&text(&out), "THE FINALS"), out);
    // A file with the game's name is not adopted either.
    std::fs::write(out.join("Desktop"), b"a file").unwrap();
    assert_eq!(resolve_game_dir(&text(&out), ""), out.join("Desktop (Recall)"));
}

#[test]
fn a_linked_game_folder_is_never_adopted() {
    let (out, _gone) = temp("link");
    let target = out.join("elsewhere");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join(rule::MARKER), b"").unwrap();
    if !rule::junction_for_test(&target, &out.join("Apex Legends")) {
        eprintln!("skipped: could not make a junction");
        return;
    }
    assert_eq!(resolve_game_dir(&text(&out), "Apex Legends"), out.join("Apex Legends (Recall)"));
    assert!(matches!(check_game_dir(&out, &out.join("Apex Legends")), Err(GameDirProblem::Link)));
}

#[test]
fn a_path_near_max_path_uses_the_clip_folder() {
    let (root, _gone) = temp("long");
    // `<out>\VALORANT\<longest clip name>` reaches the limit; `<out>\<clip>` still fits.
    let slug = "VALORANT".len();
    let pad = PATH_LIMIT - units(&root) - 1 - (1 + slug) - 1 - slug - LONGEST_TAIL + 1;
    let out = root.join("d".repeat(pad));
    std::fs::create_dir_all(&out).unwrap();
    assert!(units(&out) + 1 + slug + LONGEST_TAIL < PATH_LIMIT, "a clip straight in the folder fits");
    assert_eq!(resolve_game_dir(&text(&out), "VALORANT"), out);
    assert!(!out.join("VALORANT").exists());
    let short = root.join("s");
    std::fs::create_dir_all(&short).unwrap();
    assert_eq!(resolve_game_dir(&text(&short), "VALORANT"), short.join("VALORANT"));
}

#[test]
fn check_game_dir_remakes_a_missing_one_only() {
    let (out, _gone) = temp("check");
    let game = out.join("Rocket League");
    check_game_dir(&out, &game).unwrap();
    assert!(rule::is_game_folder(&game), "made again with its marker");
    check_game_dir(&out, &out).unwrap();
    let lost = out.join("gone");
    check_game_dir(&lost, &lost.join("Game")).unwrap();
    assert!(!lost.exists(), "a missing clip folder is never made");
    check_game_dir(&out, &out.join("a").join("b")).unwrap();
    assert!(!out.join("a").exists(), "two levels down is not a game folder");
}
