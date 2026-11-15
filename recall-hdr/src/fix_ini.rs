//! Line edits in a game's own INI settings file for the fixer: read one `key=value` in one
//! `[section]`, set it, or remove it. Every other line, the line endings and the order are kept.
//! Keys and sections match without case, as the games read them.

use std::path::Path;

/// The file as text; `None` when it is not there.
pub fn read(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes).map(Some).map_err(|_| "the settings file is not plain text".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("the settings file could not be read: {e}")),
    }
}

/// Written whole to a temp file beside it, then renamed over it.
pub fn write(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("ini.recall-tmp");
    std::fs::write(&tmp, text.as_bytes()).map_err(|e| e.to_string())?;
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

/// The line range (start, end) of `[section]`'s body: after its header, up to the next header.
fn body(lines: &[&str], section: &str) -> Option<(usize, usize)> {
    let start = lines.iter().position(|l| header(l).is_some_and(|h| h.eq_ignore_ascii_case(section)))? + 1;
    let end = lines[start..].iter().position(|l| header(l).is_some()).map_or(lines.len(), |i| start + i);
    Some((start, end))
}

fn split(text: &str) -> (Vec<&str>, &'static str, bool) {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let trailing = text.ends_with('\n');
    let lines: Vec<&str> = text.strip_suffix('\n').unwrap_or(text).split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    (lines, eol, trailing)
}

pub fn has_section(text: &str, section: &str) -> bool {
    body(&split(text).0, section).is_some()
}

/// The first `key=` in `[section]`.
pub fn get(text: &str, section: &str, key: &str) -> Option<String> {
    let (lines, _, _) = split(text);
    let (start, end) = body(&lines, section)?;
    lines[start..end].iter().filter_map(|l| key_of(l)).find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.to_owned())
}

/// `[section]` with `key` set to `value` (its first line rewritten, or a new line after the
/// section's last line), or the key's first line removed for `None`. `None` back when there is
/// no such section.
pub fn set(text: &str, section: &str, key: &str, value: Option<&str>) -> Option<String> {
    let (lines, eol, trailing) = split(text);
    let (start, end) = body(&lines, section)?;
    let mut out: Vec<String> = lines.iter().map(|l| (*l).to_owned()).collect();
    let found = (start..end).find(|&i| key_of(lines[i]).is_some_and(|(k, _)| k.eq_ignore_ascii_case(key)));
    match (found, value) {
        (Some(i), Some(v)) => {
            let name = key_of(lines[i]).map_or(key, |(k, _)| k);
            out[i] = format!("{name}={v}");
        }
        (Some(i), None) => {
            out.remove(i);
        }
        (None, Some(v)) => {
            // After the section's last non-blank line, so a blank separator line stays last.
            let at = (start..end).rev().find(|&i| !lines[i].trim().is_empty()).map_or(start, |i| i + 1);
            out.insert(at, format!("{key}={v}"));
        }
        (None, None) => {}
    }
    let mut joined = out.join(eol);
    if trailing {
        joined.push_str(eol);
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
    fn set_rewrites_one_line_and_keeps_every_other_byte() {
        let out = set(INI, "SystemSettings", "Fullscreen", Some("False")).unwrap();
        assert_eq!(out, INI.replacen("Fullscreen=True", "Fullscreen=False", 1));
        assert!(out.ends_with("[SystemSettingsMobile]\r\nFullscreen=True\r\n"), "the other section is untouched");
    }

    #[test]
    fn a_missing_key_goes_after_the_sections_last_line_and_remove_takes_it_out_again() {
        let added = set(INI, "SystemSettings", "NewKey", Some("1")).unwrap();
        assert!(added.contains("Borderless=True\r\nNewKey=1\r\n\r\n[SystemSettingsMobile]"), "{added:?}");
        assert_eq!(set(&added, "SystemSettings", "NewKey", None).unwrap(), INI);
        assert_eq!(set(INI, "Nope", "K", Some("v")), None, "no such section: nothing written");
    }

    #[test]
    fn lf_files_and_comments_stay_as_they_were() {
        let lf = "; note=1\n[S]\n;Key=old\nKey = a\n";
        assert_eq!(get(lf, "S", "Key").as_deref(), Some("a"), "a commented line is not the key");
        assert_eq!(set(lf, "S", "Key", Some("b")).unwrap(), "; note=1\n[S]\n;Key=old\nKey=b\n");
    }
}
