//! Keys a save keeps although this build does not read them: a newer build's key, a hand-added
//! section, a field inside one `[[overrides]]` entry or one audio source. Found by test, at
//! every depth, never by a list: a key is unknown when the file parses to the same config with
//! it taken out. A known key at its default value passes that test too; putting it back is
//! harmless, because it is put back only where the new file has no such key (a field serde
//! skips at its default), with the value the old file had, which is that same default.
//!
//! Array elements (`[[overrides]]`, `[[recording.audio_sources]]`) are matched by position when
//! the old and new arrays have the same length, else by the `exe` / `id` key both carry.

use toml::{Table, Value};

use super::Config;

/// A key path: table keys, with array positions as `#N`.
type At = Vec<String>;

/// Every key of `disk` (the file being replaced) the parse ignores, with its value.
pub(super) fn unknown(disk: &Table, retired: &[&str]) -> Vec<(At, Value)> {
    let Ok(base) = Value::Table(disk.clone()).try_into::<Config>() else { return Vec::new() };
    let mut out = Vec::new();
    walk(disk, &mut Vec::new(), &mut |at, value| {
        if at.len() == 1 && retired.contains(&at[0].as_str()) {
            return true; // read only to migrate, never carried (nor anything inside it)
        }
        let mut trial = Value::Table(disk.clone());
        remove(&mut trial, at);
        let same = trial.try_into::<Config>().is_ok_and(|c| c == base);
        if same {
            out.push((at.to_vec(), value.clone()));
        }
        same // an unknown key is kept whole: its insides are not looked at
    });
    out
}

/// Calls `f` for every key (tables and arrays of tables entered when `f` says false).
fn walk(table: &Table, at: &mut At, f: &mut dyn FnMut(&[String], &Value) -> bool) {
    for (key, value) in table {
        at.push(key.clone());
        if !f(at, value) {
            match value {
                Value::Table(inner) => walk(inner, at, f),
                Value::Array(items) => {
                    for (i, item) in items.iter().enumerate() {
                        if let Value::Table(inner) = item {
                            at.push(format!("#{i}"));
                            walk(inner, at, f);
                            at.pop();
                        }
                    }
                }
                _ => {}
            }
        }
        at.pop();
    }
}

fn remove(value: &mut Value, at: &[String]) {
    match at {
        [] => {}
        [last] => {
            if let Value::Table(t) = value {
                t.remove(last);
            }
        }
        [key, rest @ ..] => {
            if let Some(next) = step(value, key) {
                remove(next, rest);
            }
        }
    }
}

fn step<'a>(cur: &'a mut Value, key: &str) -> Option<&'a mut Value> {
    match (cur, key.strip_prefix('#').and_then(|n| n.parse::<usize>().ok())) {
        (Value::Array(items), Some(i)) => items.get_mut(i),
        (Value::Table(t), None) => t.get_mut(key),
        _ => None,
    }
}

/// Puts each unknown key of the old file into `out` (the new file), where `out` has none.
/// Returns how many went in (none: the new file is written exactly as serialized).
pub(super) fn merge(out: &mut Table, disk: &Table, extra: Vec<(At, Value)>) -> usize {
    let mut added = 0;
    for (at, value) in extra {
        let Some(target) = resolve(out, disk, &at) else { continue };
        let (parents, last) = target.split_at(target.len() - 1);
        let mut cur = Value::Table(std::mem::take(out));
        if let Some(slot) = descend_creating(&mut cur, parents) {
            if !slot.contains_key(&last[0]) {
                slot.insert(last[0].clone(), value);
                added += 1;
            }
        }
        if let Value::Table(t) = cur {
            *out = t;
        }
    }
    added
}

/// The path in the new file that `at` (a path in the old one) names: array positions are
/// mapped to the matching element; None when there is none.
fn resolve(out: &Table, disk: &Table, at: &[String]) -> Option<Vec<String>> {
    let (mut o, mut d) = (Some(Value::Table(out.clone())), Value::Table(disk.clone()));
    let mut path = Vec::new();
    for key in at {
        if let Some(i) = key.strip_prefix('#').and_then(|n| n.parse::<usize>().ok()) {
            let (Some(Value::Array(new)), Value::Array(old)) = (&o, &d) else { return None };
            let j = matching(new, old, i)?;
            path.push(format!("#{j}"));
            (o, d) = (Some(new[j].clone()), old[i].clone());
        } else {
            path.push(key.clone());
            let next_d = d.get(key)?.clone();
            o = o.and_then(|v| v.get(key).cloned());
            d = next_d;
        }
    }
    Some(path)
}

/// The new element that old element `i` became.
fn matching(new: &[Value], old: &[Value], i: usize) -> Option<usize> {
    if new.len() == old.len() {
        return Some(i);
    }
    let key = |v: &Value| ["exe", "id"].iter().find_map(|k| v.get(*k).and_then(Value::as_str).map(|s| (*k, s.to_string())));
    let want = key(&old[i])?;
    new.iter().position(|v| key(v).as_ref() == Some(&want))
}

fn descend_creating<'a>(cur: &'a mut Value, at: &[String]) -> Option<&'a mut Table> {
    let mut cur = cur;
    for key in at {
        cur = if key.starts_with('#') {
            step(cur, key)?
        } else {
            let Value::Table(t) = cur else { return None };
            t.entry(key.clone()).or_insert_with(|| Value::Table(Table::new()))
        };
    }
    match cur {
        Value::Table(t) => Some(t),
        _ => None,
    }
}

#[cfg(test)]
#[path = "config_extra_tests.rs"]
mod tests;
