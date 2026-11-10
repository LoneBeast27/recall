//! `compatibility`: settings files written by earlier releases (`compat/settings/v*/config.toml`)
//! load in this build with every value kept, or moved per `compat/rules.json`, or reset only in
//! an area a major release declared. Policy: CONTRIBUTING.md, "Settings compatibility".
//!
//! The rollback check (release workflow, `scripts/dev/compat-rollback.ps1`) mounts this same file
//! into the previous release's source: there `rollback_keeps_a_newer_file` reads the file this
//! build wrote (`write_this_builds_file`). Only `load_bytes`, `save_to` and `config_repairs::take`
//! are used, so keep it to names every supported release has.

use std::path::PathBuf;

use serde_json::Value;

use super::{load_bytes, Config};

#[path = "../../compat/check.rs"]
mod check;
use check::{Rules, Version};

const FILE: &str = "config.toml";

fn root() -> PathBuf {
    check::root(env!("CARGO_MANIFEST_DIR"))
}

fn fixtures() -> Vec<(Version, PathBuf)> {
    check::fixtures(&root())
}

fn as_json(toml_text: &str) -> Value {
    let table: toml::Value = toml::from_str(toml_text).expect("fixture is TOML");
    serde_json::to_value(table).unwrap()
}

/// `bytes` loaded the way this build loads config.toml, then saved over itself: the loaded
/// settings and the saved file, both as JSON, the repairs the load made, and the saved text.
fn round_trip(tag: &str, bytes: &[u8]) -> (Value, Value, Vec<String>, String) {
    let scratch = check::Scratch::new(tag);
    let path = scratch.0.join(FILE);
    std::fs::write(&path, bytes).unwrap();
    let cfg: Config = load_bytes(&path, bytes, std::time::SystemTime::now());
    let repairs = crate::config_repairs::take(&path);
    cfg.save_to(&path).expect("save");
    let loaded = serde_json::to_value(toml::Value::try_from(&cfg).unwrap()).unwrap();
    let saved_text = std::fs::read_to_string(&path).unwrap();
    (loaded, as_json(&saved_text), repairs, saved_text)
}

#[test]
fn the_rules_follow_the_policy() {
    let rules = Rules::load(&root().join("rules.json")).expect("compat/rules.json");
    let problems = rules.problems();
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn every_release_has_a_settings_fixture() {
    let found: Vec<String> = fixtures().iter().map(|(v, _)| v.to_string()).collect();
    for needed in ["0.1.0", "0.1.1"] {
        assert!(found.iter().any(|f| f == needed), "compat/settings/v{needed} is missing (found {found:?})");
    }
    for (version, dir) in fixtures() {
        assert!(dir.join(FILE).is_file(), "{} has no {FILE}", dir.display());
        assert!(dir.join("default.toml").is_file(), "{version}: default.toml (that release's defaults) is missing");
    }
}

#[test]
fn fixtures_change_every_setting_from_its_default() {
    if check::in_rollback().is_some() {
        return; // an older tree: only the rollback test runs
    }
    let rules = Rules::load(&root().join("rules.json")).unwrap();
    let mut problems = Vec::new();
    for (_, dir) in fixtures() {
        let fixture = as_json(&std::fs::read_to_string(dir.join(FILE)).unwrap());
        let default = as_json(&std::fs::read_to_string(dir.join("default.toml")).unwrap());
        problems.extend(check::unvaried(FILE, &fixture, &default, &rules));
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn old_settings_load_with_every_value_kept() {
    if check::in_rollback().is_some() {
        return;
    }
    let rules = Rules::load(&root().join("rules.json")).unwrap();
    let reader = Version::of_build();
    let mut failures = Vec::new();
    for (written, dir) in fixtures() {
        let bytes = std::fs::read(dir.join(FILE)).unwrap();
        let fixture = as_json(std::str::from_utf8(&bytes).unwrap());
        let (loaded, saved, repairs, _) = round_trip(&written.to_string(), &bytes);
        for note in repairs {
            if rules.reset_by(FILE, note.split_whitespace().next().unwrap_or(""), written, reader).is_none() {
                failures.push(format!("{FILE} {written}: the load repaired a value that release wrote: {note}"));
            }
        }
        let outcome = check::forward(FILE, written, reader, &fixture, &loaded, &saved, &rules);
        for line in &outcome.allowed {
            eprintln!("compatibility: {line}");
        }
        failures.extend(outcome.failures);
    }
    assert!(failures.is_empty(), "{} settings lost:\n{}", failures.len(), failures.join("\n"));
}

/// The release workflow (or `compat-rollback.ps1`) sets `RECALL_COMPAT_WRITE_DIR`: this build
/// writes the newest fixture as it saves it, for the previous release to read.
#[test]
fn write_this_builds_file() {
    let Some(out) = std::env::var_os("RECALL_COMPAT_WRITE_DIR").map(PathBuf::from) else { return };
    let (_, newest) = fixtures().pop().expect("a fixture");
    let (_, _, _, saved_text) = round_trip("write", &std::fs::read(newest.join(FILE)).unwrap());
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join(FILE), saved_text).unwrap();
}

/// Runs in the previous release's tree: the newer build's file loads here with every value this
/// release knows, and its own save keeps the keys it does not know.
#[test]
fn rollback_keeps_a_newer_file() {
    let Some(dir) = check::in_rollback() else { return };
    let bytes = std::fs::read(dir.join(FILE)).expect("the newer build's config.toml");
    let newer = as_json(std::str::from_utf8(&bytes).unwrap());
    let (loaded, saved, repairs, _) = round_trip("rollback", &bytes);
    let mut failures: Vec<String> = repairs.into_iter().map(|r| format!("{FILE}: the older build repaired {r}")).collect();
    failures.extend(check::rollback(FILE, &newer, &loaded, &saved).failures);
    assert!(failures.is_empty(), "{} settings lost on rollback:\n{}", failures.len(), failures.join("\n"));
}
