//! Durable small-file writes and corrupt-file backups (deep-audit lane 1, round 2).
//!
//! - [`write_durable`]: write `<name>.<pid>.tmp`, `sync_all`, then rename over the
//!   file, so a power cut can't leave an empty or NUL-filled file after the
//!   rename (NTFS journals the rename, not the cached data).
//! - [`quarantine`]: a damaged file is kept as `<name>.bad-YYYY-MM-DD` (UTC); a
//!   second break the same day gets `-2`, `-3`, ... An older backup is never
//!   overwritten. A copy of bytes the newest backup already holds is skipped.
//!   The backup's mtime is set to now, so a reader can tell that it is newer
//!   than any file written before the damage was found.
//! - [`sweep_temps`]: removes the temp files a crash between the write and the
//!   rename left behind.
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Backups of one file on one day; past that the damaged file stays where it is.
const PER_DAY: u32 = 20;
/// A temp file this old is a crash leftover, not a write in progress.
const STALE_TEMP: Duration = Duration::from_secs(600);

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

/// Today's first free backup name: `<name>.bad-YYYY-MM-DD`, then `-2`, `-3`, ...
fn free_bad_path(path: &Path, now: SystemTime) -> Option<PathBuf> {
    let first = bad_path(path, now);
    if !first.exists() {
        return Some(first);
    }
    let day = utc_day(now);
    (2..=PER_DAY).map(|n| sibling(path, &format!(".bad-{day}-{n}"))).find(|p| !p.exists())
}

/// The newest `<name>.bad*` beside `path`, by modified time.
fn newest_bad(path: &Path) -> Option<PathBuf> {
    let prefix = format!("{}.bad", path.file_name()?.to_string_lossy());
    fs::read_dir(path.parent()?)
        .ok()?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max_by_key(|(at, _)| *at)
        .map(|(_, p)| p)
}

/// Keeps the damaged `path` as a backup (see the module notes). Returns the
/// backup, or None when nothing could be kept.
pub fn quarantine(path: &Path, now: SystemTime, keep: Keep) -> Option<PathBuf> {
    if keep == Keep::Copy {
        // The same bad file loaded again (another start, another day): one backup is enough.
        let same = newest_bad(path).filter(|newest| fs::read(newest).ok().is_some_and(|kept| fs::read(path).ok().is_some_and(|current| current == kept)));
        if same.is_some() {
            return same;
        }
    }
    let bad = free_bad_path(path, now)?;
    let kept = match keep {
        Keep::Move => fs::rename(path, &bad).is_ok() || fs::copy(path, &bad).is_ok(),
        Keep::Copy => fs::copy(path, &bad).is_ok(),
    };
    if !kept {
        return None;
    }
    if let Ok(file) = OpenOptions::new().write(true).open(&bad) {
        let _ = file.set_modified(now);
    }
    Some(bad)
}

/// Removes `<name>.<pid>.tmp` (and an older build's `<name>.tmp`) beside `path` once
/// they are [`STALE_TEMP`] old: a crash between the write and the rename left them.
pub fn sweep_temps(path: &Path, now: SystemTime) {
    let (Some(name), Some(dir)) = (path.file_name().map(|n| n.to_string_lossy().into_owned()), path.parent()) else { return };
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let file = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = file.strip_prefix(&name).and_then(|r| r.strip_suffix(".tmp")) else { continue };
        let ours = rest.is_empty() || rest.strip_prefix('.').is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()));
        let stale = entry.metadata().and_then(|m| m.modified()).ok().and_then(|at| now.duration_since(at).ok()).is_some_and(|age| age >= STALE_TEMP);
        if ours && stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("recall-hdr-durable-{tag}-{}", std::process::id()));
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
        let bad2 = quarantine(&path, day1 + Duration::from_secs(60), Keep::Move).unwrap();
        assert_eq!(bad2.file_name().unwrap(), "x.json.bad-2026-10-30-2", "a second break the same day");
        assert_eq!((fs::read(&bad1).unwrap(), fs::read(&bad2).unwrap()), (b"first".to_vec(), b"second".to_vec()), "both kept");
        assert!(!path.exists());
        let mtime = fs::metadata(&bad2).unwrap().modified().unwrap();
        assert_eq!(mtime, day1 + Duration::from_secs(60), "stamped with the time it was found");
        fs::write(&path, b"third").unwrap();
        let bad3 = quarantine(&path, day1 + Duration::from_secs(86_400), Keep::Copy).unwrap();
        assert_eq!(fs::read(&bad3).unwrap(), b"third");
        assert!(path.exists(), "a copy leaves the file");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unchanged_damaged_file_is_copied_once() {
        let dir = temp("same");
        let path = dir.join("x.toml");
        fs::write(&path, b"bad = = value").unwrap();
        let day1 = UNIX_EPOCH + Duration::from_secs(1_793_318_400);
        for day in 0..3 {
            quarantine(&path, day1 + Duration::from_secs(day * 86_400), Keep::Copy).unwrap();
        }
        let backups = fs::read_dir(&dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains(".bad")).count();
        assert_eq!(backups, 1, "the same bytes are backed up once");
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

    #[test]
    fn old_temp_files_are_swept_and_a_live_one_is_kept() {
        let dir = temp("sweep");
        let path = dir.join("x.json");
        let now = SystemTime::now();
        for name in ["x.json.12.tmp", "x.json.tmp", "x.json.34.tmp", "x.json.bad.tmp", "y.json.12.tmp"] {
            fs::write(dir.join(name), b"partial").unwrap();
        }
        sweep_temps(&path, now + Duration::from_secs(3_600));
        let mut names: Vec<_> = fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, vec!["x.json.bad.tmp", "y.json.12.tmp"], "only this file's own leftovers go");
        fs::write(dir.join("x.json.56.tmp"), b"live").unwrap();
        sweep_temps(&path, SystemTime::now());
        assert!(dir.join("x.json.56.tmp").exists(), "a write in progress is left alone");
        let _ = fs::remove_dir_all(&dir);
    }
}
