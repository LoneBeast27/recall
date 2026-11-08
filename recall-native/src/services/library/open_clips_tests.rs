use super::*;

fn temp_file() -> PathBuf {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("recall-open-{}-{nonce}", std::process::id()));
    path_beside(&dir.join("library.json"))
}

fn read(file: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap()
}

#[test]
fn the_open_clip_is_published_beside_the_library_and_cleared() {
    let file = temp_file();
    assert_eq!(file.file_name().unwrap(), "open-clips.json");
    write_to(&file, 7, &[r"E:\Clips\G__a.mp4".into()], |_| true).unwrap();
    assert_eq!(read(&file)["paths"], serde_json::json!([r"E:\Clips\G__a.mp4"]), "the shape the recorder reads");
    write_to(&file, 7, &[], |_| true).unwrap();
    assert_eq!(read(&file)["paths"], serde_json::json!([]));
    assert!(!file.with_extension("json.7.tmp").exists());
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[test]
fn every_running_app_process_keeps_its_clips_and_a_dead_ones_go() {
    // A closed window's process (1) still exports X while the new window (2) opens Y.
    let file = temp_file();
    write_to(&file, 1, &[r"E:\Clips\X.mp4".into()], |_| true).unwrap();
    write_to(&file, 2, &[], |_| true).unwrap();
    assert_eq!(read(&file)["paths"], serde_json::json!([r"E:\Clips\X.mp4"]), "a new window's start keeps the export's clip");
    write_to(&file, 2, &[r"E:\Clips\Y.mp4".into()], |_| true).unwrap();
    assert_eq!(read(&file)["paths"], serde_json::json!([r"E:\Clips\X.mp4", r"E:\Clips\Y.mp4"]));
    write_to(&file, 2, &[r"E:\Clips\Y.mp4".into()], |pid| pid != 1).unwrap();
    assert_eq!(read(&file)["paths"], serde_json::json!([r"E:\Clips\Y.mp4"]), "process 1 ended: its clips go");
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

static STATE: Mutex<Own> = Mutex::new(Own { editor: None, exports: Vec::new() });
static WRITTEN: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());

fn record(paths: &[String]) {
    WRITTEN.lock().unwrap().push(paths.to_vec());
}

#[test]
fn an_export_source_is_open_until_its_export_ends() {
    change(&STATE, |own| own.editor = Some("Y.mp4".into()), record);
    change(&STATE, |own| own.exports.push("X.mp4".into()), record);
    let guard = Exporting { path: "X.mp4".into(), state: &STATE, write: record };
    // The window closes (editor cleared) while the export runs.
    change(&STATE, |own| own.editor = None, record);
    drop(guard);
    let written = WRITTEN.lock().unwrap().clone();
    let expected: Vec<Vec<String>> = vec![vec!["Y.mp4".into()], vec!["Y.mp4".into(), "X.mp4".into()], vec!["X.mp4".into()], vec![]];
    assert_eq!(written, expected);
}
