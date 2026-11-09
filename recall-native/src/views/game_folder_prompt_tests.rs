use super::*;

fn temp(tag: &str) -> (PathBuf, crate::test_temp::TempDir) {
    let dir = crate::test_temp::TempDir::sandbox(format!("gfprompt-{tag}"));
    (dir.to_path_buf(), dir)
}

fn text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[test]
fn the_prompt_text_names_the_game_and_the_folder() {
    assert_eq!(title("VALORANT"), "Use your existing VALORANT folder?");
    let [first, second] = body("Rocket League®", "Rocket League");
    assert!(first.starts_with("Recordings already has a folder called Rocket League (maybe from ShadowPlay or another app)."), "{first}");
    assert!(first.contains("save new Rocket League® clips into it, or keep them in a separate \"Rocket League (Recall)\" folder."), "{first}");
    assert!(first.ends_with("Recall-named clips in a folder it uses count toward your storage limit."), "{first}");
    assert_eq!(second, "Recall's storage limit only applies to new clips it saves there; your existing files are never removed.");
    assert_eq!((USE_FOLDER, KEEP_SEPARATE), ("Use this folder", "Keep separate"));
}

#[test]
fn choice_keys_and_answers() {
    assert_eq!(choice_key(r"V:\Videos\VALORANT").as_deref(), Some("valorant"));
    assert_eq!(answer_value(r"V:\Videos\Rocket League", true), Some(json!({"rocket league": "merge"})));
    assert_eq!(answer_value(r"V:\Videos\VALORANT", false), Some(json!({"valorant": "separate"})));
    let raw = json!({"valorant": "merge", "apex legends": "separate", "junk": "maybe", "cleared": "", "n": 1});
    let got = choices(Some(&raw));
    assert_eq!(got.keys().collect::<Vec<_>>(), vec!["apex legends", "valorant"]);
    assert_eq!(reset_value(&got), json!({"apex legends": "", "valorant": ""}));
    assert!(choices(None).is_empty());
}

#[test]
fn one_question_per_folder() {
    let mut list = Vec::new();
    assert!(add(&mut list, Pending { game: "VALORANT".into(), folder: r"V:\R\VALORANT".into() }));
    assert!(!add(&mut list, Pending { game: "Valorant".into(), folder: r"v:\r\valorant".into() }), "same folder");
    assert!(add(&mut list, Pending { game: "Apex".into(), folder: r"V:\R\Apex".into() }));
    assert_eq!(list.len(), 2);
    let (dir, _gone) = temp("persist");
    let path = dir.join("p.json");
    save_to(&path, &list).unwrap();
    assert_eq!(load_from(&path), list);
    assert!(load_from(&dir.join("missing.json")).is_empty());
}

#[test]
fn only_a_plain_unmarked_unanswered_unlinked_folder_is_asked_about() {
    let (rec, _gone) = temp("open");
    let game = rec.join("VALORANT");
    std::fs::create_dir_all(&game).unwrap();
    let p = Pending { game: "VALORANT".into(), folder: text(&game) };
    let none = BTreeMap::new();
    assert!(still_open(&p, &rec, &[], &none));
    let answered: BTreeMap<String, String> = [("valorant".to_string(), "merge".to_string())].into();
    assert!(!still_open(&p, &rec, &[], &answered), "answered");
    assert!(!still_open(&p, &rec, &[text(&game)], &none), "a linked (Folder Mapper) folder");
    assert!(!still_open(&p, &rec.join("elsewhere"), &[], &none), "not in Recordings");
    let target = rec.join("target");
    std::fs::create_dir_all(&target).unwrap();
    let link = rec.join("Apex Legends");
    if rule::junction_for_test(&target, &link) {
        assert!(!still_open(&Pending { game: "Apex Legends".into(), folder: text(&link) }, &rec, &[], &none), "a junction");
    } else {
        eprintln!("skipped: could not make a junction");
    }
    let hidden = rec.join("Hidden Game");
    std::fs::create_dir_all(&hidden).unwrap();
    if rule::hide_for_test(&hidden) {
        assert!(!still_open(&Pending { game: "Hidden Game".into(), folder: text(&hidden) }, &rec, &[], &none), "hidden");
    }
    rule::adopt(&game).unwrap();
    assert!(!still_open(&p, &rec, &[], &none), "adopted: marked");
}

/// Gate A2: a failed adoption is reported and saves no "merge" answer; "Keep separate" never adopts.
#[test]
fn a_failed_adoption_saves_no_answer() {
    let failed = answer(r"V:\Videos\VALORANT", true, |_| Err(std::io::Error::other("entry unreadable")));
    assert_eq!(failed, Err("entry unreadable".to_string()));
    let mut adopted = false;
    let ok = answer(r"V:\Videos\VALORANT", true, |_| {
        adopted = true;
        Ok(())
    });
    assert_eq!(ok, Ok(Some(json!({"valorant": "merge"}))));
    assert!(adopted);
    let separate = answer(r"V:\Videos\VALORANT", false, |_| panic!("not adopted"));
    assert_eq!(separate, Ok(Some(json!({"valorant": "separate"}))));
}

/// Gate A4: the prompts file follows its override, then the sandboxed library file, then app data;
/// every result passes the test guard (a real profile path panics).
#[test]
fn the_prompts_file_follows_the_overrides() {
    let (dir, _gone) = temp("path");
    let own = dir.join("own.json");
    let library = dir.join("meta").join("library.json");
    let appdata = dir.join(r"AppData\Roaming");
    assert_eq!(prompts_path_from(Some(own.clone().into()), Some(library.clone().into()), Some(appdata.clone())), Some(own));
    assert_eq!(prompts_path_from(None, Some(library.clone().into()), Some(appdata.clone())), Some(dir.join("meta").join("game-folder-prompts.json")));
    assert_eq!(prompts_path_from(Some("".into()), None, Some(appdata.clone())), Some(appdata.join("Recall").join("game-folder-prompts.json")));
    assert_eq!(prompts_path_from(None, None, None), None);
    let real = std::env::var_os("APPDATA").map(PathBuf::from).filter(|p| !p.as_os_str().is_empty());
    if let Some(real) = real {
        let hit = std::panic::catch_unwind(|| prompts_path_from(None, None, Some(real)));
        assert!(hit.is_err(), "the real %APPDATA% is refused in tests");
    }
}
