//! Durable small-file writes and corrupt-file backups (deep-audit lane 1).
//!
//! - [`write_durable`]: write `<name>.tmp`, `sync_all`, then rename over the
//!   file, so a power cut can't leave an empty or NUL-filled file after the
//!   rename (NTFS journals the rename, not the cached data).
//! - [`quarantine`]: a damaged file is kept as `<name>.bad-YYYY-MM-DD` (UTC),
//!   one per day. An older backup is never overwritten: when today's already
//!   exists, the damaged file stays where it is and today's backup is kept.
//!   The backup's mtime is set to now, so a reader can tell that it is newer
//!   than any file written before the damage was found.
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// `<path>` with `suffix` appended to the file name (`a.json` -> `a.json.tmp`).
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

/// Writes `bytes` to `path`: a temp sibling, flushed to disk, then renamed over it.
pub fn write_durable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let tmp = sibling(path, &format!(".{}.tmp", std::process::id()));
    let written = File::create(&tmp).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(format!("write {}: {e}", tmp.display()));
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("rename {} -> {}: {e}", tmp.display(), path.display())
    })
}

/// `YYYY-MM-DD` (UTC) of `now`.
pub fn utc_day(now: SystemTime) -> String {
    let days = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() / 86_400) as i64;
    // Civil-from-days (Howard Hinnant), valid for every date after 1970.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Today's backup name for `path`: `<name>.bad-YYYY-MM-DD`.
pub fn bad_path(path: &Path, now: SystemTime) -> PathBuf {
    sibling(path, &format!(".bad-{}", utc_day(now)))
}

/// How [`quarantine`] keeps the damaged file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    /// Move it aside (the caller starts without it).
    Move,
    /// Copy it (the caller rewrites the file with what it could read).
    Copy,
}

/// Keeps the damaged `path` as today's backup (see the module notes). Returns
/// the backup, or None when nothing could be kept.
pub fn quarantine(path: &Path, now: SystemTime, keep: Keep) -> Option<PathBuf> {
    let bad = bad_path(path, now);
    let kept = if bad.exists() {
        // One a day, never overwritten: the damaged file stays put this time.
        true
    } else {
        match keep {
            Keep::Move => fs::rename(path, &bad).is_ok() || fs::copy(path, &bad).is_ok(),
            Keep::Copy => fs::copy(path, &bad).is_ok(),
        }
    };
    if !kept {
        return None;
    }
    if let Ok(file) = OpenOptions::new().write(true).open(&bad) {
        let _ = file.set_modified(now);
    }
    Some(bad)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("recall-overlay-durable-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn days_are_utc_calendar_dates() {
        let at = |secs: u64| utc_day(UNIX_EPOCH + Duration::from_secs(secs));
        assert_eq!(at(0), "1970-01-01");
        assert_eq!(at(951_782_400), "2000-02-29");
        assert_eq!(at(1_793_318_399), "2026-10-29");
        assert_eq!(at(1_793_318_400), "2026-10-30");
    }

    #[test]
    fn one_backup_a_day_and_an_older_one_is_never_overwritten() {
        let dir = temp("bad");
        let path = dir.join("x.json");
        let day1 = UNIX_EPOCH + Duration::from_secs(1_793_318_400);
        fs::write(&path, b"first").unwrap();
        let bad1 = quarantine(&path, day1, Keep::Move).unwrap();
        assert_eq!(bad1.file_name().unwrap(), "x.json.bad-2026-10-30");
        assert!(!path.exists(), "moved aside");
        fs::write(&path, b"second").unwrap();
        assert_eq!(quarantine(&path, day1 + Duration::from_secs(60), Keep::Move), Some(bad1.clone()));
        assert_eq!(fs::read(&bad1).unwrap(), b"first", "today's backup is kept");
        assert_eq!(fs::read(&path).unwrap(), b"second", "the second damaged file stays put");
        let mtime = fs::metadata(&bad1).unwrap().modified().unwrap();
        assert_eq!(mtime, day1 + Duration::from_secs(60), "stamped with the time it was found");
        let bad2 = quarantine(&path, day1 + Duration::from_secs(86_400), Keep::Copy).unwrap();
        assert_eq!((fs::read(&bad1).unwrap(), fs::read(&bad2).unwrap()), (b"first".to_vec(), b"second".to_vec()));
        assert!(path.exists(), "a copy leaves the file");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_durable_write_replaces_the_file_and_leaves_no_temp() {
        let dir = temp("write");
        let path = dir.join("sub").join("x.toml");
        write_durable(&path, b"one").unwrap();
        write_durable(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        let names: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names.len(), 1, "{names:?}");
        let _ = fs::remove_dir_all(&dir);
    }
}
