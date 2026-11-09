//! The settings backup Recall Setup made before the last update
//! (`%LOCALAPPDATA%\Recall\updates\config-backup-<version>-<yyyyMMdd-HHmmss>\roaming\recall-hdr\config.toml`,
//! `scripts/setup/SetupUserState.cs`). When config.toml has a value this build cannot read, the
//! value comes from there instead of the default, and the app is told (`config_repairs`); a file
//! that is not TOML at all, with no last good copy, is read from there whole.

use std::path::{Path, PathBuf};

use toml::Table;

use crate::log;

const PREFIX: &str = "config-backup-";
const INSIDE: &str = r"roaming\recall-hdr\config.toml";

/// The newest backup's config.toml.
pub struct Backup {
    pub path: PathBuf,
    pub table: Table,
}

/// `%LOCALAPPDATA%\Recall\updates` (the injected test root's in unit tests).
fn updates_dir() -> Option<PathBuf> {
    let local = crate::user_dirs::known(crate::user_dirs::Known::LocalAppData)?;
    Some(crate::user_dirs::checked(local.join("Recall").join("updates")))
}

/// "yyyyMMddHHmmss" + a 4-digit same-second number, from a backup folder's name (the order
/// Setup keeps them in), else None.
pub fn stamp(name: &str) -> Option<String> {
    let rest = name.strip_prefix(PREFIX)?;
    let parts: Vec<&str> = rest.split('-').collect();
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    (0..parts.len().saturating_sub(1)).find(|&i| digits(parts[i], 8) && digits(parts[i + 1], 6)).map(|i| {
        let n = parts.get(i + 2).filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())).map_or("0".into(), |p| p.to_string());
        format!("{}{}{:0>4}", parts[i], parts[i + 1], n)
    })
}

/// The newest backup holding a config.toml that parses as TOML.
pub fn newest() -> Option<Backup> {
    newest_in(&updates_dir()?)
}

pub fn newest_in(updates: &Path) -> Option<Backup> {
    let mut dirs: Vec<(String, PathBuf)> = std::fs::read_dir(updates)
        .ok()?
        .flatten()
        .filter_map(|e| Some((stamp(&e.file_name().to_string_lossy())?, e.path())))
        .collect();
    dirs.sort();
    dirs.into_iter().rev().find_map(|(_, dir)| {
        let path = dir.join(INSIDE);
        let table = std::fs::read_to_string(&path).ok()?.parse::<Table>().ok()?;
        Some(Backup { path, table })
    })
}

/// The notes for the keys that did not read (`lost`): restored from `backup`, or reset.
pub fn notes(lost: &[String], backup: Option<&Backup>) -> Vec<String> {
    if lost.is_empty() {
        return Vec::new();
    }
    let keys = lost.join(", ");
    vec![match backup {
        Some(b) => format!("config.toml: {keys} could not be read; restored from the settings backup made before the update ({})", b.path.display()),
        None => format!("config.toml: {keys} could not be read; reset to the default"),
    }]
}

/// A config.toml that is not TOML at all and has no last good copy: the backup's, whole.
pub fn whole_file() -> Option<(crate::config::Config, String, Vec<String>)> {
    let backup = newest()?;
    let text = std::fs::read_to_string(&backup.path).ok()?;
    let cfg = toml::from_str(&text).ok()?;
    log!("config: using the settings backup {}", backup.path.display());
    let note = format!("config.toml could not be read; restored from the settings backup made before the update ({})", backup.path.display());
    Some((cfg, text, vec![note]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backups_are_ordered_by_the_time_in_their_names() {
        assert_eq!(stamp("config-backup-0.1.0-20261110-120000").as_deref(), Some("202611101200000000"));
        assert_eq!(stamp("config-backup-0.1.0-20261110-120000-2").as_deref(), Some("202611101200000002"));
        assert_eq!(stamp("config-backup-old-20261110-120000").as_deref(), Some("202611101200000000"));
        assert_eq!(stamp("config-backup-0.1.0"), None);
        assert_eq!(stamp("download"), None);
    }

    #[test]
    fn the_newest_readable_backup_is_used() {
        let root = crate::config_extra_tests_dir("backups");
        let put = |name: &str, text: &str| {
            let p = root.join(name).join(INSIDE);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        put("config-backup-0.1.0-20261101-100000", "[storage]\ncap_gb = 10\n");
        put("config-backup-0.1.1-20261105-100000", "[storage]\ncap_gb = 20\n");
        put("config-backup-0.1.2-20261109-100000", "not = [toml");
        let b = newest_in(&root).unwrap();
        assert_eq!(b.table["storage"]["cap_gb"].as_integer(), Some(20), "the newest that parses");
        assert!(notes(&["storage.cap_gb".into()], Some(&b))[0].contains("restored from the settings backup"));
        assert!(notes(&["storage.cap_gb".into()], None)[0].contains("reset to the default"));
        assert!(notes(&[], Some(&b)).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
