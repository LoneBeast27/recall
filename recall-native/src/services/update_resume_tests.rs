use super::*;

fn recording() -> RecorderState {
    RecorderState { running: true, recording: true, long_on: true, game: true }
}

#[test]
fn the_resume_file_round_trips_and_keeps_unknown_keys() {
    let dir = tempfile::tempdir().unwrap();
    let mut resume = Resume::new("editor", Some(r"D:\Clips\a.mp4".into()), recording());
    resume.extra.insert("from_a_newer_setup".into(), serde_json::json!({"x": 1}));
    let path = resume.write_in(dir.path()).unwrap();
    assert_eq!(path, dir.path().join(FILE));
    let back = Resume::read(&path, dir.path()).expect("read back");
    assert_eq!(back, resume);
    assert_eq!(back.extra["from_a_newer_setup"], serde_json::json!({"x": 1}));
}

#[test]
fn only_the_updates_folders_resume_file_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let path = Resume::new("library", None, RecorderState::default()).write_in(other.path()).unwrap();
    assert_eq!(Resume::read(&path, dir.path()), None, "a resume file elsewhere is ignored");
    let stray = dir.path().join("other.json");
    std::fs::write(&stray, "{}").unwrap();
    assert_eq!(Resume::read(&stray, dir.path()), None, "another file name is ignored");
}

#[test]
fn setups_outcome_becomes_the_reopened_apps_notices() {
    let mut r = Resume::new("library", None, recording());
    assert!(r.notices().is_empty(), "no outcome yet: nothing to say");
    r.outcome = Some("failed".into());
    r.still_on = Some("0.1.0".into());
    assert_eq!(r.notices(), vec![(false, "Update failed, still on v0.1.0".to_owned())]);
    let unknown = Resume { outcome: Some("failed".into()), ..Resume::default() };
    assert_eq!(unknown.notices(), vec![(false, "Update failed; Recall was not changed".to_owned())], "a resume file Setup could not read");
    r.outcome = Some("updated".into());
    r.recovered = Some(r"D:\Clips\G__rec.mp4".into());
    let notes = r.notices();
    assert!(notes[0].0 && notes[0].1.starts_with("Recall is updated"));
    assert_eq!(notes[1], (false, CUT_SHORT.to_owned()));
    assert_eq!(CUT_SHORT, "The last recording was cut short during the update and was recovered");
}

#[test]
fn setups_json_is_read_as_written_by_setup() {
    // SetupUpdateFlow.cs writes the outcome with JavaScriptSerializer: compact, nulls included.
    let text = r#"{"version":1,"route":"editor","clip":"C:\\clip.mp4","from_version":"0.1.0","recorder":{"running":true,"long_on":true},"outcome":"failed","error":"install rolled back","still_on":"0.1.0","recovered":null,"config_backup":"X","setup_version":"0.2.0"}"#;
    let r: Resume = serde_json::from_str(text).unwrap();
    assert_eq!(r.route, "editor");
    assert!(r.recorder.long_on && !r.recorder.game);
    assert_eq!(r.still_on.as_deref(), Some("0.1.0"));
    assert_eq!(r.extra["setup_version"], "0.2.0");
}

#[test]
fn the_resume_argument_is_found() {
    let args = ["--resume", r"C:\x\resume.json"].map(std::ffi::OsString::from);
    assert_eq!(resume_arg(args), Some(PathBuf::from(r"C:\x\resume.json")));
    assert_eq!(resume_arg(["--other"].map(std::ffi::OsString::from)), None);
}
