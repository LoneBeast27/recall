//! The settings compatibility check shared by both crates' `compatibility` tests (and by the
//! rollback check, which mounts the same harness into the previous release's source). Needs only
//! `serde_json`, so it compiles against any release's dependencies. Rules: `compat/rules.json`;
//! the policy: `CONTRIBUTING.md`, "Settings compatibility".
//!
//! A settings file is compared as leaves: `recording.audio_sources[mic].gain_db = 3.0`. An array
//! of tables is keyed by each element's `id`, `exe` or `hotkey` (else its position), so a reorder
//! is not a change.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

/// A release version (`0.1.1`), or the unreleased build (newer than every release).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Version {
    Released(u32, u32, u32),
    Unreleased,
}

impl Version {
    /// `0.1.1` or `v0.1.1`.
    pub fn parse(text: &str) -> Option<Version> {
        let mut parts = text.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u32>().ok());
        let v = Version::Released(parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(v)
    }

    /// This build: `RECALL_VERSION` (the release workflow sets it), else unreleased.
    pub fn of_build() -> Version {
        std::env::var("RECALL_VERSION").ok().and_then(|v| Version::parse(&v)).unwrap_or(Version::Unreleased)
    }

    pub fn is_major(self) -> bool {
        matches!(self, Version::Released(_, 0, 0))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Version::Released(a, b, c) => write!(f, "{a}.{b}.{c}"),
            Version::Unreleased => write!(f, "unreleased"),
        }
    }
}

/// Every leaf of `value`, by path.
pub fn leaves(value: &Value) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    walk(value, String::new(), &mut out);
    out
}

fn walk(value: &Value, at: String, out: &mut BTreeMap<String, Value>) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (key, inner) in map {
                let next = if at.is_empty() { key.clone() } else { format!("{at}.{key}") };
                walk(inner, next, out);
            }
        }
        Value::Array(items) if !items.is_empty() && items.iter().all(Value::is_object) => {
            for (i, item) in items.iter().enumerate() {
                walk(item, format!("{at}[{}]", element_key(item, i)), out);
            }
        }
        _ => {
            out.insert(at, value.clone());
        }
    }
}

fn element_key(item: &Value, index: usize) -> String {
    ["id", "exe", "hotkey"]
        .iter()
        .find_map(|k| item.get(*k).and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| format!("#{index}"))
}

/// `path` is `area` or lies inside it (`recording.audio_sources` covers `recording.audio_sources[mic].gain_db`).
pub fn in_area(path: &str, area: &str) -> bool {
    path == area || path.strip_prefix(area).is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['))
}

/// A setting that moved: the old leaf's value now lives at `to` (same value), or a named test
/// proves the mapping (`checked_by`) when the value changes shape.
#[derive(Debug, Clone)]
pub struct Moved {
    pub file: String,
    pub from: String,
    pub to: Option<String>,
    pub checked_by: Option<String>,
}

/// A major release that may reset `areas` of `file` for settings written before it.
#[derive(Debug, Clone)]
pub struct Major {
    pub version: Version,
    pub file: String,
    pub areas: Vec<String>,
    pub note: String,
}

#[derive(Debug, Clone, Default)]
pub struct Rules {
    pub moved: Vec<Moved>,
    pub major: Vec<Major>,
    /// Leaves a fixture keeps at that version's default, with why (`file -> path -> reason`).
    pub unvaried: BTreeMap<String, BTreeMap<String, String>>,
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

impl Rules {
    /// Reads `rules.json`; Err names what is malformed.
    pub fn load(path: &Path) -> Result<Rules, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let doc: Value = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        let list = |key: &str| doc.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
        let mut rules = Rules::default();
        for m in list("moved") {
            let (Some(file), Some(from)) = (text(&m, "file"), text(&m, "from")) else {
                return Err(format!("moved entry needs file and from: {m}"));
            };
            let (to, checked_by) = (text(&m, "to"), text(&m, "checked_by"));
            if to.is_none() && checked_by.is_none() {
                return Err(format!("moved entry {file}:{from} needs to or checked_by"));
            }
            rules.moved.push(Moved { file, from, to, checked_by });
        }
        for m in list("major") {
            let version = text(&m, "version").and_then(|v| Version::parse(&v));
            let (Some(version), Some(file), Some(note)) = (version, text(&m, "file"), text(&m, "note")) else {
                return Err(format!("major entry needs version, file and note: {m}"));
            };
            let areas: Vec<String> = m.get("areas").and_then(Value::as_array).into_iter().flatten().filter_map(|a| a.as_str().map(str::to_owned)).collect();
            rules.major.push(Major { version, file, areas, note });
        }
        if let Some(Value::Object(files)) = doc.get("unvaried") {
            for (file, paths) in files {
                let Value::Object(paths) = paths else { return Err(format!("unvaried.{file} must map paths to reasons")) };
                let entry = rules.unvaried.entry(file.clone()).or_default();
                for (path, why) in paths {
                    entry.insert(path.clone(), why.as_str().unwrap_or_default().to_owned());
                }
            }
        }
        Ok(rules)
    }

    /// The policy itself: a reset is declared only by a major (x.0.0) release, for named areas
    /// (never a whole file), with a note for the release notes.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        for m in &self.major {
            if !m.version.is_major() {
                out.push(format!("{}: settings may reset only in a major release (x.0.0), not {}", m.file, m.version));
            }
            if m.areas.is_empty() || m.areas.iter().any(|a| a.trim().is_empty()) {
                out.push(format!("{} {}: name the areas that reset; a whole file never resets", m.version, m.file));
            }
            if m.note.trim().is_empty() {
                out.push(format!("{} {}: the note for the release notes is empty", m.version, m.file));
            }
        }
        out
    }

    /// The major release that may reset `path` of `file` written by `written` and read by `reader`.
    pub fn reset_by(&self, file: &str, path: &str, written: Version, reader: Version) -> Option<&Major> {
        self.major.iter().find(|m| m.file == file && written < m.version && m.version <= reader && m.areas.iter().any(|a| in_area(path, a)))
    }
}

/// What a check found: failures, and the declared resets it allowed (for the run's log).
#[derive(Debug, Default)]
pub struct Outcome {
    pub failures: Vec<String>,
    pub allowed: Vec<String>,
}

/// Forward: `fixture` (written by release `written`) was loaded by this build (`loaded` is the
/// loaded settings, serialized) and saved again (`saved` is the file after the save). Every
/// fixture value must arrive, or have moved per the rules, or lie in an area a major release
/// between the two versions declared.
pub fn forward(file: &str, written: Version, reader: Version, fixture: &Value, loaded: &Value, saved: &Value, rules: &Rules) -> Outcome {
    let (fixture, loaded, saved) = (leaves(fixture), leaves(loaded), leaves(saved));
    let mut out = Outcome::default();
    for (path, value) in &fixture {
        if let Some(moved) = rules.moved.iter().find(|m| m.file == file && in_area(path, &m.from)) {
            if let Some(to) = &moved.to {
                let target = format!("{to}{}", &path[moved.from.len()..]);
                if loaded.get(&target) != Some(value) && rules.reset_by(file, &target, written, reader).is_none() {
                    out.failures.push(format!("{file} {written}: `{path}` = {value} moved to `{target}`, which reads {}", show(loaded.get(&target))));
                }
            }
            continue;
        }
        let kept_on_load = loaded.get(path) == Some(value);
        let kept_on_save = saved.get(path) == Some(value);
        if kept_on_load && kept_on_save {
            continue;
        }
        if let Some(major) = rules.reset_by(file, path, written, reader) {
            out.allowed.push(format!("{file} {written}: `{path}` reset by {} ({})", major.version, major.note));
            continue;
        }
        let what = if !kept_on_load { format!("loads as {}", show(loaded.get(path))) } else { format!("is saved as {}", show(saved.get(path))) };
        out.failures.push(format!(
            "{file} written by {written}: `{path}` = {value} {what}. Map it (compat/rules.json \"moved\" + a migration), \
             or declare the area in a major release (\"major\"); see CONTRIBUTING.md"
        ));
    }
    out
}

/// Rollback: `newer` (written by a newer build) was loaded by this older build and saved again.
/// A value this build reads must arrive unchanged; a key it does not read must survive its save,
/// so moving forward again finds it.
pub fn rollback(file: &str, newer: &Value, loaded: &Value, saved: &Value) -> Outcome {
    let (newer, loaded, saved) = (leaves(newer), leaves(loaded), leaves(saved));
    let mut out = Outcome::default();
    for (path, value) in &newer {
        let known = loaded.contains_key(path);
        if known && loaded.get(path) != Some(value) {
            out.failures.push(format!("{file}: `{path}` = {value} loads in the older build as {}", show(loaded.get(path))));
        } else if saved.get(path) != Some(value) {
            out.failures.push(format!("{file}: `{path}` = {value} is saved by the older build as {}", show(saved.get(path))));
        }
    }
    out
}

/// Leaves of `fixture` still at `default` that the rules do not list: a fixture that keeps a
/// default proves nothing about that setting.
pub fn unvaried(file: &str, fixture: &Value, default: &Value, rules: &Rules) -> Vec<String> {
    let (fixture, default) = (leaves(fixture), leaves(default));
    let listed = rules.unvaried.get(file);
    default
        .iter()
        .filter(|(path, value)| fixture.get(*path) == Some(*value))
        .filter(|(path, _)| !listed.is_some_and(|l| l.keys().any(|a| in_area(path, a))))
        .map(|(path, value)| format!("{file}: fixture keeps `{path}` at its default {value}; change it, or list it under \"unvaried\" with why"))
        .collect()
}

/// `compat/` beside the crate at `manifest_dir`, or `RECALL_COMPAT_ROOT` (the rollback check
/// mounts a harness into an older tree, which has no fixtures of its own).
pub fn root(manifest_dir: &str) -> std::path::PathBuf {
    std::env::var_os("RECALL_COMPAT_ROOT").map(Into::into).unwrap_or_else(|| Path::new(manifest_dir).join("..").join("compat"))
}

/// Set in an older tree by the rollback check: only the rollback tests run there.
pub fn in_rollback() -> Option<std::path::PathBuf> {
    std::env::var_os("RECALL_COMPAT_ROLLBACK_DIR").map(Into::into)
}

/// Every release fixture folder under `root/settings` (`v0.1.0`), oldest first.
pub fn fixtures(root: &Path) -> Vec<(Version, std::path::PathBuf)> {
    let mut out: Vec<(Version, std::path::PathBuf)> = std::fs::read_dir(root.join("settings"))
        .expect("compat/settings")
        .filter_map(|e| e.ok())
        .filter_map(|e| Version::parse(&e.file_name().to_string_lossy()).map(|v| (v, e.path())))
        .collect();
    out.sort();
    out
}

/// A private folder under %TEMP% (never the user's settings), removed on drop.
pub struct Scratch(pub std::path::PathBuf);

impl Scratch {
    pub fn new(tag: &str) -> Scratch {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("recall-compat-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs a forward and a rollback check over JSON settings files, given how this build round-trips
/// one (`round_trip(tag, bytes) -> (loaded, saved)`). Used by the app's harnesses.
pub fn forward_all(root: &Path, file: &str, round_trip: &dyn Fn(&str, &[u8]) -> (Value, Value)) -> Vec<String> {
    let rules = Rules::load(&root.join("rules.json")).expect("compat/rules.json");
    let reader = Version::of_build();
    let mut failures = Vec::new();
    for (written, dir) in fixtures(root) {
        let Ok(bytes) = std::fs::read(dir.join(file)) else { continue };
        let fixture: Value = serde_json::from_slice(&bytes).expect("fixture is JSON");
        let (loaded, saved) = round_trip(&written.to_string(), &bytes);
        let outcome = forward(file, written, reader, &fixture, &loaded, &saved, &rules);
        for line in &outcome.allowed {
            eprintln!("compatibility: {line}");
        }
        failures.extend(outcome.failures);
    }
    failures
}

/// Rollback of one JSON file from `dir` (absent there: nothing to check).
pub fn rollback_file(dir: &Path, file: &str, round_trip: &dyn Fn(&str, &[u8]) -> (Value, Value)) -> Vec<String> {
    let Ok(bytes) = std::fs::read(dir.join(file)) else { return Vec::new() };
    let newer: Value = serde_json::from_slice(&bytes).expect("newer file is JSON");
    let (loaded, saved) = round_trip("rollback", &bytes);
    rollback(file, &newer, &loaded, &saved).failures
}

/// The newest fixture of `file`, as this build saves it, into `out` (for the previous release).
pub fn write_newest(root: &Path, out: &Path, file: &str, save: &dyn Fn(&[u8]) -> Vec<u8>) {
    let Some(bytes) = fixtures(root).into_iter().rev().find_map(|(_, dir)| std::fs::read(dir.join(file)).ok()) else { return };
    std::fs::create_dir_all(out).unwrap();
    std::fs::write(out.join(file), save(&bytes)).unwrap();
}

fn show(v: Option<&Value>) -> String {
    v.map_or_else(|| "(missing)".into(), Value::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn leaves_key_arrays_by_id_so_a_reorder_is_no_change() {
        let a = leaves(&json!({"s": [{"id": "mic", "g": 1}, {"id": "game", "g": 2}]}));
        let b = leaves(&json!({"s": [{"id": "game", "g": 2}, {"id": "mic", "g": 1}]}));
        assert_eq!(a, b);
        assert_eq!(a.get("s[mic].g"), Some(&json!(1)));
    }

    #[test]
    fn a_changed_value_fails_unless_a_major_release_declares_its_area() {
        let old = json!({"audio": {"gain": 3}, "video": {"fps": 144}});
        let new = json!({"audio": {"gain": 0}, "video": {"fps": 60}});
        let v011 = Version::parse("0.1.1").unwrap();
        let none = Rules::default();
        assert_eq!(forward("f", v011, Version::Unreleased, &old, &new, &new, &none).failures.len(), 2);
        let major = Major { version: Version::parse("2.0.0").unwrap(), file: "f".into(), areas: vec!["audio".into()], note: "new mixer".into() };
        let rules = Rules { major: vec![major], ..Rules::default() };
        let out = forward("f", v011, Version::parse("2.0.0").unwrap(), &old, &new, &new, &rules);
        assert_eq!(out.failures.len(), 1, "video is outside the declared area: {:?}", out.failures);
        assert!(out.failures[0].contains("video.fps"));
        assert_eq!(out.allowed.len(), 1);
        // The same break in a 1.x release is not allowed.
        assert_eq!(forward("f", v011, Version::parse("1.4.0").unwrap(), &old, &new, &new, &rules).failures.len(), 2);
    }

    #[test]
    fn the_policy_refuses_a_reset_outside_a_major_release_or_of_a_whole_file() {
        let bad = |version: &str, areas: Vec<String>| Rules {
            major: vec![Major { version: Version::parse(version).unwrap(), file: "f".into(), areas, note: "n".into() }],
            ..Rules::default()
        };
        assert_eq!(bad("1.2.0", vec!["audio".into()]).problems().len(), 1);
        assert_eq!(bad("2.0.0", vec![]).problems().len(), 1);
        assert!(bad("2.0.0", vec!["audio".into()]).problems().is_empty());
    }

    #[test]
    fn a_moved_value_must_arrive_at_its_new_key() {
        let old = json!({"a": {"x": 5}});
        let rules = Rules { moved: vec![Moved { file: "f".into(), from: "a.x".into(), to: Some("b.x".into()), checked_by: None }], ..Rules::default() };
        let v = Version::parse("0.1.0").unwrap();
        assert!(forward("f", v, Version::Unreleased, &old, &json!({"b": {"x": 5}}), &json!({}), &rules).failures.is_empty());
        assert_eq!(forward("f", v, Version::Unreleased, &old, &json!({"b": {"x": 6}}), &json!({}), &rules).failures.len(), 1);
    }

    #[test]
    fn rollback_needs_known_values_kept_and_unknown_keys_saved() {
        let newer = json!({"a": 1, "new": true});
        assert!(rollback("f", &newer, &json!({"a": 1}), &json!({"a": 1, "new": true})).failures.is_empty());
        assert_eq!(rollback("f", &newer, &json!({"a": 1}), &json!({"a": 1})).failures.len(), 1, "unknown key lost on save");
        assert_eq!(rollback("f", &newer, &json!({"a": 2}), &newer).failures.len(), 1, "known value changed");
    }
}
