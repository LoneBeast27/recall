//! Restore policy P1-P4 and (e) (app side): the recovered-recording prompt's text, Resume
//! recording as the one command it sends, End it here, the open questions across a restart,
//! and that a recovery alone never starts a recording.
use super::*;

fn temp(tag: &str) -> (PathBuf, crate::test_temp::TempDir) {
    let dir = crate::test_temp::TempDir::sandbox(format!("recovered-prompt-{tag}"));
    (dir.to_path_buf(), dir)
}

fn pending(path: &str, seconds: Option<f64>, repaired: bool) -> Pending {
    Pending { path: path.to_string(), seconds, repaired }
}

/// P1: "<video name> (recovered)" with its length, or "(recovered, length unknown)".
#[test]
fn the_prompt_names_the_recording_and_says_what_was_recovered() {
    let p = pending(r"E:\c\VALORANT__2026-11-10__13-00-00__rec.mp4", Some(754.0), true);
    assert_eq!(title(&p), "VALORANT__2026-11-10__13-00-00__rec.mp4 (recovered)");
    let lines = body(&p);
    assert_eq!(lines[0], "Recall closed before it finished. The last few seconds may be missing.");
    assert_eq!(lines[1], "Length: 12:34");
    let q = pending(r"E:\c\VALORANT__2026-11-10__13-00-00__rec.mp4", None, false);
    assert_eq!(title(&q), "VALORANT__2026-11-10__13-00-00__rec.mp4 (recovered, length unknown)");
    assert!(body(&q).contains(&"It plays, but seeking may be slow.".to_string()));
    assert_eq!((RESUME, END), ("Resume recording", "End it here"));
}

/// P2 / P3: Resume recording is the one command (continue this file); End it here sends none.
/// Either answer closes the question.
#[test]
fn resume_sends_the_one_command_and_end_sends_none() {
    let (a, b) = (r"E:\c\A__2026-11-10__13-00-00__rec.mp4", r"E:\c\B__2026-11-10__13-05-00__rec.mp4");
    let mut list = Vec::new();
    assert!(add(&mut list, pending(a, Some(60.0), true)));
    assert!(add(&mut list, pending(b, Some(30.0), true)));
    assert!(!add(&mut list, pending(&a.replace('\\', "/"), None, false)), "asked once per file");
    assert_eq!(answer(&mut list, a, true), Some(DaemonCommand::RecordResume { path: a.to_string() }));
    assert_eq!(answer(&mut list, b, false), None, "End it here starts nothing");
    assert!(list.is_empty());
    assert_eq!(answer(&mut list, a, true), None, "an answered question sends nothing again");
}

/// P4: questions still open survive a restart (the file), and the path never resolves into the
/// real user's folders under test.
#[test]
fn open_questions_survive_a_restart() {
    let (dir, _guard) = temp("restart");
    let path = prompts_path_from(None, None, Some(dir.join("AppData"))).unwrap();
    assert!(path.ends_with(r"Recall\recovered-prompts.json"));
    let list = vec![pending(r"E:\c\A__2026-11-10__13-00-00__rec.mp4", Some(60.0), true)];
    save_to(&path, &list).unwrap();
    assert_eq!(load_from(&path), list);
}

/// (e): a recovered recording only asks: adding the question sends nothing, and nothing but
/// [`answer`] with Resume makes a command.
#[test]
fn a_recovery_alone_never_starts_a_recording() {
    let mut list = Vec::new();
    add(&mut list, pending(r"E:\c\A__2026-11-10__13-00-00__rec.mp4", Some(60.0), true));
    assert_eq!(list.len(), 1, "asked, nothing sent");
    assert_eq!(answer(&mut list, r"E:\c\A__2026-11-10__13-00-00__rec.mp4", false), None);
}
