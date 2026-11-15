//! Line edits in a game's own INI settings file for the fixer: read one `key=value` in one
//! `[section]`, set it, or remove it. Every other byte is kept: each line's own ending, a UTF-8
//! byte-order mark, and on the edited line its key and the spacing around `=` (so Undo gives
//! back `Fullscreen = True` exactly). Keys and sections match without case, as games read them.

use std::io::Write;
use std::path::{Path, PathBuf};

const BOM: char = '\u{feff}';

/// The file as text; `None` when it is not there.
pub fn read(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes).map(Some).map_err(|_| "the settings file is not plain text".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("the settings file could not be read: {e}")),
    }
}

/// `<file>.recall-bak`: the file as it was before Recall first changed it.
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".recall-bak");
    path.with_file_name(name)
}

/// Keeps one backup of the file as it was before Recall's first change (never overwritten).
pub fn backup_once(path: &Path) -> Result<(), String> {
    let bak = backup_path(path);
    if bak.exists() {
        return Ok(());
    }
    std::fs::copy(path, &bak).map(|_| ()).map_err(|e| format!("the settings file could not be backed up: {e}"))
}

/// Crash-safe: a new temp file beside it (a unique name, never one already there), flushed to
/// disk, then renamed over the file. A power cut leaves the old file or the new one, whole.
pub fn write(path: &Path, text: &str) -> Result<(), String> {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    name.push(format!(".{}-{stamp}.recall-tmp", std::process::id()));
    let tmp = path.with_file_name(name);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| f.write_all(text.as_bytes()).and_then(|()| f.sync_all()));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

fn header(line: &str) -> Option<&str> {
    let t = line.trim();
    t.strip_prefix('[').and_then(|r| r.strip_suffix(']')).map(str::trim)
}

fn key_of(line: &str) -> Option<(&str, &str)> {
    let t = line.trim_start();
    if t.starts_with(';') || t.starts_with('#') || header(line).is_some() {
        return None;
    }
    let (k, v) = t.split_once('=')?;
    Some((k.trim(), v.trim()))
}

/// Lines with their own endings ("\r\n", "\n" or "" for the last); a leading BOM apart.
fn split(text: &str) -> (bool, Vec<(&str, &str)>) {
    let bom = text.starts_with(BOM);
    let body = if bom { &text[BOM.len_utf8()..] } else { text };
    let mut out = Vec::new();
    let mut rest = body;
    while !rest.is_empty() {
        let (line, ending, next) = match rest.find('\n') {
            Some(i) if i > 0 && rest.as_bytes()[i - 1] == b'\r' => (&rest[..i - 1], "\r\n", &rest[i + 1..]),
            Some(i) => (&rest[..i], "\n", &rest[i + 1..]),
            None => (rest, "", ""),
        };
        out.push((line, ending));
        rest = next;
    }
    (bom, out)
}

/// The line range (start, end) of `[section]`'s body: after its header, up to the next header.
fn body(lines: &[(&str, &str)], section: &str) -> Option<(usize, usize)> {
    let start = lines.iter().position(|(l, _)| header(l).is_some_and(|h| h.eq_ignore_ascii_case(section)))? + 1;
    let end = lines[start..].iter().position(|(l, _)| header(l).is_some()).map_or(lines.len(), |i| start + i);
    Some((start, end))
}

pub fn has_section(text: &str, section: &str) -> bool {
    body(&split(text).1, section).is_some()
}

/// The first `key=` in `[section]`.
pub fn get(text: &str, section: &str, key: &str) -> Option<String> {
    let (_, lines) = split(text);
    let (start, end) = body(&lines, section)?;
    lines[start..end].iter().filter_map(|(l, _)| key_of(l)).find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.to_owned())
}

/// `line` with its value replaced, keeping everything up to and after `=` but the value.
fn with_value(line: &str, value: &str) -> String {
    let Some(eq) = line.find('=') else { return line.to_owned() };
    let after = &line[eq + 1..];
    let lead = after.len() - after.trim_start().len();
    let trail = after.trim_end().len();
    format!("{}{}{}", &line[..eq + 1 + lead], value, &after[trail.max(lead)..])
}

/// `[section]` with `key` set to `value` (the value on its first line replaced, or a new line
/// after the section's last line), or the key's first line removed for `None`. `None` back when
/// there is no such section.
pub fn set(text: &str, section: &str, key: &str, value: Option<&str>) -> Option<String> {
    let (bom, lines) = split(text);
    let (start, end) = body(&lines, section)?;
    let mut out: Vec<(String, &str)> = lines.iter().map(|(l, e)| ((*l).to_owned(), *e)).collect();
    let found = (start..end).find(|&i| key_of(lines[i].0).is_some_and(|(k, _)| k.eq_ignore_ascii_case(key)));
    match (found, value) {
        (Some(i), Some(v)) => out[i].0 = with_value(lines[i].0, v),
        (Some(i), None) => {
            out.remove(i);
        }
        (None, Some(v)) => {
            // After the section's last non-blank line, with that line's own ending.
            let at = (start..end).rev().find(|&i| !lines[i].0.trim().is_empty()).map_or(start, |i| i + 1);
            let ending = lines.get(at.saturating_sub(1)).map_or("\r\n", |(_, e)| if e.is_empty() { "\r\n" } else { e });
            if at > 0 && out[at - 1].1.is_empty() {
                out[at - 1].1 = ending;
            }
            out.insert(at, (format!("{key}={v}"), if at == out.len() { "" } else { ending }));
        }
        (None, None) => {}
    }
    let mut joined = String::with_capacity(text.len() + 32);
    if bom {
        joined.push(BOM);
    }
    for (line, ending) in &out {
        joined.push_str(line);
        joined.push_str(ending);
    }
    Some(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INI: &str = "[SystemSettings]\r\nFullscreen=True\r\nResX=2560\r\nBorderless=True\r\n\r\n[SystemSettingsMobile]\r\nFullscreen=True\r\n";

    #[test]
    fn get_reads_only_the_named_section() {
        assert_eq!(get(INI, "SystemSettings", "fullscreen").as_deref(), Some("True"));
        assert_eq!(get(INI, "SystemSettingsMobile", "Fullscreen").as_deref(), Some("True"));
        assert_eq!(get(INI, "SystemSettings", "Missing"), None);
        assert_eq!(get(INI, "Nope", "Fullscreen"), None);
    }

    #[test]
    fn set_rewrites_one_value_and_keeps_every_other_byte() {
        let out = set(INI, "SystemSettings", "Fullscreen", Some("False")).unwrap();
        assert_eq!(out, INI.replacen("Fullscreen=True", "Fullscreen=False", 1));
        assert_eq!(set(&out, "SystemSettings", "Fullscreen", Some("True")).unwrap(), INI, "and back, exactly");
    }

    #[test]
    fn a_missing_key_goes_after_the_sections_last_line_and_remove_takes_it_out_again() {
        let added = set(INI, "SystemSettings", "NewKey", Some("1")).unwrap();
        assert!(added.contains("Borderless=True\r\nNewKey=1\r\n\r\n[SystemSettingsMobile]"), "{added:?}");
        assert_eq!(set(&added, "SystemSettings", "NewKey", None).unwrap(), INI);
        assert_eq!(set(INI, "Nope", "K", Some("v")), None, "no such section: nothing written");
    }

    #[test]
    fn spacing_mixed_endings_comments_and_a_bom_stay_as_they_were() {
        let odd = "\u{feff}; note=1\n[S]\r\n;Key=old\nKey = a  \r\nOther=1";
        assert!(has_section(odd, "S"), "a BOM does not hide the first section");
        assert_eq!(get(odd, "S", "Key").as_deref(), Some("a"), "a commented line is not the key");
        let b = set(odd, "S", "Key", Some("b")).unwrap();
        assert_eq!(b, "\u{feff}; note=1\n[S]\r\n;Key=old\nKey = b  \r\nOther=1");
        assert_eq!(set(&b, "S", "Key", Some("a")).unwrap(), odd, "Undo gives the original bytes back");
    }

    #[test]
    fn the_write_is_whole_and_the_backup_is_kept_once() {
        let dir = std::env::temp_dir().join(format!("recall-fix-ini-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("TASystemSettings.ini");
        std::fs::write(&file, INI).unwrap();
        backup_once(&file).unwrap();
        write(&file, "changed").unwrap();
        backup_once(&file).unwrap();
        assert_eq!(std::fs::read_to_string(backup_path(&file)).unwrap(), INI, "the first backup is never overwritten");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "changed");
        let leftovers = std::fs::read_dir(&dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".recall-tmp")).count();
        assert_eq!(leftovers, 0, "no temp file left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
