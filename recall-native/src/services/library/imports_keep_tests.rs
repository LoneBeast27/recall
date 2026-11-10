//! imports.json keeps the keys a newer build wrote, at the top and inside a file's entry.
use super::*;

#[test]
fn unknown_keys_survive_a_load_and_save() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("imports.json");
    let text = serde_json::json!({
        "version": 1,
        "future_index": {"built": 3},
        "files": {"d:\\old\\a.mp4": {"path": "D:\\Old\\a.mp4", "bytes": 5, "mtime": 6, "kind": "clip", "future_note": {"by": "x"}}}
    });
    std::fs::write(&path, text.to_string()).unwrap();
    let table = load_table(&path);
    save_table(&path, &table).unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["future_index"], serde_json::json!({"built": 3}), "top-level unknown key");
    assert_eq!(saved["files"]["d:\\old\\a.mp4"]["future_note"], serde_json::json!({"by": "x"}), "unknown key in an entry");
    assert_eq!(saved["files"]["d:\\old\\a.mp4"]["bytes"], 5);
}
