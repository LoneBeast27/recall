//! `compatibility`: the update settings (`%LOCALAPPDATA%\Recall\updates\settings.json`, "Check
//! for updates automatically") written by earlier releases load with every value kept. Same
//! checker and fixtures as `library/compatibility_tests.rs` (`compat/`).

use serde_json::Value;

use super::prefs::{load_auto, save_auto};

#[path = "../../../compat/check.rs"]
mod check;

const FILE: &str = "update-settings.json";

fn root() -> std::path::PathBuf {
    check::root(env!("CARGO_MANIFEST_DIR"))
}

/// What this build reads of the file, then the file after its own save of that value.
fn round_trip(tag: &str, bytes: &[u8]) -> (Value, Value) {
    let scratch = check::Scratch::new(tag);
    let path = scratch.0.join("settings.json");
    std::fs::write(&path, bytes).unwrap();
    let on = load_auto(Some(&path));
    save_auto(&path, on).expect("save update settings");
    let loaded = serde_json::json!({ "check_automatically": on });
    (loaded, serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap())
}

#[test]
fn old_update_settings_load_with_every_value_kept() {
    if check::in_rollback().is_some() {
        return;
    }
    let failures = check::forward_all(&root(), FILE, &round_trip);
    assert!(failures.is_empty(), "{} settings lost:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn write_this_builds_file() {
    let Some(out) = std::env::var_os("RECALL_COMPAT_WRITE_DIR").map(std::path::PathBuf::from) else { return };
    check::write_newest(&root(), &out, FILE, &|bytes| serde_json::to_vec(&round_trip("write", bytes).1).unwrap());
}

#[test]
fn rollback_keeps_a_newer_file() {
    let Some(dir) = check::in_rollback() else { return };
    let failures = check::rollback_file(&dir, FILE, &round_trip);
    assert!(failures.is_empty(), "{} settings lost on rollback:\n{}", failures.len(), failures.join("\n"));
}
