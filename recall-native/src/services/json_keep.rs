//! A settings file this build rewrites keeps the keys it does not know (a newer build's), so
//! an update and a later downgrade never lose a setting (the maintainer's rule: "ensure updates
//! always save a user configuration"). `known` names every key this build writes, including the
//! ones it leaves out when unset: those are never brought back from the old file.
use std::path::Path;

use serde_json::{Map, Value};

/// `new` (this build's object) plus every key of the object at `path` outside `known`.
pub fn with_unknown_keys(path: &Path, new: Value, known: &[&str]) -> Value {
    let Value::Object(mut out) = new else { return new };
    let old: Map<String, Value> = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default();
    for (key, value) in old {
        if !known.contains(&key.as_str()) && !out.contains_key(&key) {
            out.insert(key, value);
        }
    }
    Value::Object(out)
}

/// [`with_unknown_keys`] of a serializable value, as pretty JSON bytes.
pub fn pretty_keeping(path: &Path, value: &impl serde::Serialize, known: &[&str]) -> Result<Vec<u8>, String> {
    let new = serde_json::to_value(value).map_err(|e| e.to_string())?;
    serde_json::to_vec_pretty(&with_unknown_keys(path, new, known)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_keys_stay_and_known_ones_follow_this_build() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        std::fs::write(&path, r#"{"budget":5,"first_run":true,"future":{"a":1}}"#).unwrap();
        // This build cleared `first_run` (left out when unset): it must not come back.
        let out = with_unknown_keys(&path, serde_json::json!({"budget": 7}), &["budget", "first_run"]);
        assert_eq!(out, serde_json::json!({"budget": 7, "future": {"a": 1}}));
        assert_eq!(with_unknown_keys(&dir.path().join("none.json"), serde_json::json!({"b": 1}), &["b"]), serde_json::json!({"b": 1}));
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(with_unknown_keys(&path, serde_json::json!({"b": 1}), &["b"]), serde_json::json!({"b": 1}), "a damaged file adds nothing");
    }
}
