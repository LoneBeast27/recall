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

use std::cell::RefCell;

/// Resolves with no linked folders, the given answer for every folder, and the questions
/// recorded in `asked`.
fn resolve_answering(out: &Path, source: &str, choice: Option<Choice>, asked: &RefCell<Vec<PathBuf>>) -> PathBuf {
    let ask = |folder: &Path| asked.borrow_mut().push(folder.to_path_buf());
    resolve_with(&text(out), source, &Inputs { linked: &[], choose: &|_| choice, ask: &ask })
}

fn users_valorant(out: &Path) -> PathBuf {
    let user = out.join("VALORANT");
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(user.join("VALORANT__2025-01-01__10-00-00.mp4"), b"old").unwrap();
    std::fs::write(user.join("ShadowPlay.mp4"), b"theirs").unwrap();
    user
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

/// No answer yet: the clip goes into the clip folder itself (no extra folder), the question is
/// asked once per run, and nothing is cached, so the next save checks again.
#[test]
fn an_unanswered_folder_saves_flat_and_asks_once() {
    let (out, _gone) = temp("ask");
    let user = users_valorant(&out);
    let asked = RefCell::new(Vec::new());
    assert_eq!(resolve_answering(&out, "VALORANT", None, &asked), out);
    assert_eq!(resolve_answering(&out, "VALORANT", None, &asked), out);
    assert_eq!(*asked.borrow(), vec![user.clone()], "once per game per run");
    assert!(!out.join("VALORANT (Recall)").exists(), "no extra folder");
    assert!(!rule::has_marker(&user));
    // Answered later in the run: the next save follows the answer.
    assert_eq!(resolve_answering(&out, "VALORANT", Some(Choice::Separate), &asked), out.join("VALORANT (Recall)"));
    // Both names taken by the user's folders, "separate": the clip folder itself.
    std::fs::create_dir_all(out.join("THE FINALS")).unwrap();
    std::fs::create_dir_all(out.join("THE FINALS (Recall)")).unwrap();
    assert_eq!(resolve_answering(&out, "THE FINALS", Some(Choice::Separate), &asked), out);
    // A file with the game's name is never asked about.
    std::fs::write(out.join("Desktop"), b"a file").unwrap();
    assert_eq!(resolve_answering(&out, "", None, &asked), out.join("Desktop (Recall)"));
    assert_eq!(asked.borrow().len(), 1);
}

/// "Use this folder": the marker is written (with what was there), the folder is used, and
/// nothing else in it changes or moves.
#[test]
fn merge_adopts_the_users_folder_and_touches_nothing_else() {
    let (out, _gone) = temp("merge");
    let user = users_valorant(&out);
    let before = listing(&user);
    let asked = RefCell::new(Vec::new());
    assert_eq!(resolve_answering(&out, "VALORANT", Some(Choice::Merge), &asked), user);
    assert!(asked.borrow().is_empty());
    let marker = rule::game_folder_marker(&user).expect("adopted");
    assert!(!marker.owns("VALORANT__2025-01-01__10-00-00.mp4"), "there before: the user's");
    let mut after = listing(&user);
    after.retain(|n| n != rule::MARKER);
    assert_eq!(after, before, "contents untouched");
    assert_eq!(std::fs::read(user.join("ShadowPlay.mp4")).unwrap(), b"theirs");
    assert!(!out.join("VALORANT (Recall)").exists());
}

#[test]
fn separate_uses_the_recall_folder() {
    let (out, _gone) = temp("separate");
    let user = users_valorant(&out);
    let asked = RefCell::new(Vec::new());
    let dir = resolve_answering(&out, "VALORANT", Some(Choice::Separate), &asked);
    assert_eq!(dir, out.join("VALORANT (Recall)"));
    assert!(rule::is_game_folder(&dir), "made with its marker");
    assert!(!rule::has_marker(&user), "the user's folder stays untouched");
    assert!(asked.borrow().is_empty());
}

/// (d) A junction, a hidden folder or a linked (Folder Mapper) folder is never adopted and
/// never asked about, even with "merge": "(Recall)".
#[test]
fn links_hidden_and_linked_folders_are_never_adopted_or_asked_about() {
    let (out, _gone) = temp("never");
    let asked = RefCell::new(Vec::new());
    let target = out.join("elsewhere");
    std::fs::create_dir_all(&target).unwrap();
    if rule::junction_for_test(&target, &out.join("Apex Legends")) {
        assert_eq!(resolve_answering(&out, "Apex Legends", Some(Choice::Merge), &asked), out.join("Apex Legends (Recall)"));
        assert!(!rule::has_marker(&target), "nothing written through the link");
    } else {
        eprintln!("skipped: could not make a junction");
    }
    let hidden = out.join("Hidden Game");
    std::fs::create_dir_all(&hidden).unwrap();
    if rule::hide_for_test(&hidden) {
        assert_eq!(resolve_answering(&out, "Hidden Game", Some(Choice::Merge), &asked), out.join("Hidden Game (Recall)"));
        assert_eq!(resolve_answering(&out, "Hidden Game", None, &asked), out.join("Hidden Game (Recall)"));
        assert!(!rule::has_marker(&hidden));
    }
    let linked = out.join("Rocket League");
    std::fs::create_dir_all(&linked).unwrap();
    let links = [text(&linked)];
    let ask = |folder: &Path| asked.borrow_mut().push(folder.to_path_buf());
    for choice in [None, Some(Choice::Merge)] {
        let dir = resolve_with(&text(&out), "Rocket League", &Inputs { linked: &links, choose: &|_| choice, ask: &ask });
        assert_eq!(dir, out.join("Rocket League (Recall)"));
    }
    assert!(!rule::has_marker(&linked));
    assert!(asked.borrow().is_empty(), "never asked: {:?}", asked.borrow());
}

#[test]
fn answers_come_from_the_environment_or_live() {
    use super::super::game_choices::parse_choices;
    let parsed = parse_choices(r#"{"VALORANT":"merge","apex legends":"separate","x":"maybe","y":""}"#);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed.get("valorant"), Some(&Choice::Merge), "keys lowercased");
    assert_eq!(parsed.get("apex legends"), Some(&Choice::Separate));
    assert!(parse_choices("not json").is_empty());
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
    let asked = RefCell::new(Vec::new());
    assert_eq!(resolve_answering(&out, "Apex Legends", Some(Choice::Merge), &asked), out.join("Apex Legends (Recall)"));
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

#[test]
fn check_game_dir_refuses_a_folder_that_is_not_ours_now() {
    let (out, _gone) = temp("notours");
    let game = out.join("VALORANT");
    std::fs::create_dir_all(&game).unwrap();
    assert!(matches!(check_game_dir(&out, &game), Err(GameDirProblem::NotOurs)));
    assert!(!rule::has_marker(&game), "never marked by the check");
    rule::write_marker(&game).unwrap();
    check_game_dir(&out, &game).unwrap();
}

#[test]
fn the_engine_reads_the_live_answers_command() {
    let cmd: crate::protocol::Command = serde_json::from_str(r#"{"cmd":"set_game_folder_choices","choices":{"valorant":"merge"}}"#).unwrap();
    assert!(matches!(cmd, crate::protocol::Command::SetGameFolderChoices { choices } if choices.get("valorant").map(String::as_str) == Some("merge")));
    let event = serde_json::to_value(crate::protocol::Outbound::GameFolderExists { game: "VALORANT".into(), folder: r"V:\R\VALORANT".into() }).unwrap();
    assert_eq!(event, serde_json::json!({"type": "game_folder_exists", "game": "VALORANT", "folder": r"V:\R\VALORANT"}));
}
