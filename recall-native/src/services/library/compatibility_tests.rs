//! `compatibility`: clip names and favourites (`library.json`), watched folders (`folders.json`)
//! and the editor's remembered choices (`editor.json`) written by earlier releases
//! (`compat/settings/v*/`) load in this build with every value kept. Policy: CONTRIBUTING.md,
//! "Settings compatibility"; the checker: `compat/check.rs`.
//!
//! The rollback check mounts this file into the previous release's source too, so it uses only
//! `MetaStore::load_from` / `persist`, `Folders::load_from` / `save_to` and `EditorPrefs`.

use serde_json::Value;

use super::folders::Folders;
use super::MetaStore;
use crate::services::editor_prefs::{use_test_file, EditorPrefs};

#[path = "../../../../compat/check.rs"]
mod check;

fn root() -> std::path::PathBuf {
    check::root(env!("CARGO_MANIFEST_DIR"))
}

/// `library.json` loaded by this build, then written whole again: (clips, file after the write).
fn library(tag: &str, bytes: &[u8]) -> (Value, Value) {
    let scratch = check::Scratch::new(tag);
    let path = scratch.0.join("library.json");
    std::fs::write(&path, bytes).unwrap();
    let store = MetaStore::load_from(path.clone());
    let loaded = serde_json::to_value(&store.clips).unwrap();
    store.persist().expect("write library.json");
    let saved = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    (loaded, saved)
}

fn folders(tag: &str, bytes: &[u8]) -> (Value, Value) {
    let scratch = check::Scratch::new(tag);
    let path = scratch.0.join("folders.json");
    std::fs::write(&path, bytes).unwrap();
    let folders = Folders::load_from(&path);
    let loaded = serde_json::to_value(&folders).unwrap();
    folders.save_to(&path).expect("save folders.json");
    (loaded, serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap())
}

fn editor(tag: &str, bytes: &[u8]) -> (Value, Value) {
    let scratch = check::Scratch::new(tag);
    let path = scratch.0.join("editor.json");
    std::fs::write(&path, bytes).unwrap();
    use_test_file(Some(path.clone()));
    let prefs = EditorPrefs::load();
    let loaded = serde_json::to_value(&prefs).unwrap();
    let saved = prefs.save();
    use_test_file(None);
    saved.expect("save editor.json");
    (loaded, serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap())
}

type RoundTrip = fn(&str, &[u8]) -> (Value, Value);
const FILES: [(&str, RoundTrip); 3] = [("library.json", library), ("folders.json", folders), ("editor.json", editor)];

#[test]
fn old_app_settings_load_with_every_value_kept() {
    if check::in_rollback().is_some() {
        return;
    }
    let mut failures = Vec::new();
    for (file, round_trip) in FILES {
        failures.extend(check::forward_all(&root(), file, &round_trip));
    }
    assert!(failures.is_empty(), "{} settings lost:\n{}", failures.len(), failures.join("\n"));
}

/// Names and favourites read through the library's own lookup, not only as file contents.
#[test]
fn old_clip_names_and_favourites_reach_the_library() {
    if check::in_rollback().is_some() {
        return;
    }
    for (version, dir) in check::fixtures(&root()) {
        let Ok(bytes) = std::fs::read(dir.join("library.json")) else { continue };
        let fixture: serde_json::Map<String, Value> = serde_json::from_slice(&bytes).unwrap();
        let scratch = check::Scratch::new("names");
        let path = scratch.0.join("library.json");
        std::fs::write(&path, &bytes).unwrap();
        let store = MetaStore::load_from(path);
        let (mut titles, mut stars) = (0, 0);
        for (key, entry) in &fixture {
            let meta = store.meta_of(key, "").unwrap_or_else(|| panic!("{version}: clip {key} lost"));
            if let Some(title) = entry.get("title").and_then(Value::as_str) {
                assert_eq!(meta.title.as_deref(), Some(title), "{version}: name of {key}");
                titles += 1;
            }
            let star = entry.get("favourite").and_then(Value::as_bool).unwrap_or(false);
            assert_eq!(meta.favourite, star, "{version}: favourite of {key}");
            stars += usize::from(star);
        }
        assert!(titles >= 2 && stars >= 2, "{version}: the fixture must name and star clips ({titles} names, {stars} stars)");
    }
}

#[test]
fn every_release_has_app_settings_fixtures() {
    for (version, dir) in check::fixtures(&root()) {
        for (file, _) in FILES {
            assert!(dir.join(file).is_file(), "compat/settings/v{version}/{file} is missing");
        }
    }
}

/// `RECALL_COMPAT_WRITE_DIR`: this build's save of the newest fixtures, for the previous release.
#[test]
fn write_this_builds_files() {
    let Some(out) = std::env::var_os("RECALL_COMPAT_WRITE_DIR").map(std::path::PathBuf::from) else { return };
    for (file, round_trip) in FILES {
        check::write_newest(&root(), &out, file, &|bytes| {
            let (_, saved) = round_trip("write", bytes);
            serde_json::to_vec_pretty(&saved).unwrap()
        });
    }
}

/// Runs in the previous release's tree: the newer build's files load here unchanged, and this
/// release's own save keeps what it does not read.
#[test]
fn rollback_keeps_newer_files() {
    let Some(dir) = check::in_rollback() else { return };
    let mut failures = Vec::new();
    for (file, round_trip) in FILES {
        failures.extend(check::rollback_file(&dir, file, &round_trip));
    }
    assert!(failures.is_empty(), "{} settings lost on rollback:\n{}", failures.len(), failures.join("\n"));
}
