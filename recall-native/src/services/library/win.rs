//! Win32-backed pieces: local wall-clock time, Recycle Bin moves, volume space.
use std::collections::HashMap;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{FILETIME, HWND, SYSTEMTIME};
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows::Win32::UI::Shell::{FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW, SHFileOperationW};
use windows::core::{BOOL, PCWSTR};

use super::LocalStamp;
use super::calendar::stamp_from_unix_offset;

/// Seconds between 1601-01-01 (FILETIME epoch) and 1970-01-01.
const FILETIME_UNIX_OFFSET: u64 = 11_644_473_600;

/// Uses the machine's current time-zone rules (IST for this user); UTC if Win32 fails.
pub(super) fn local_from_unix(secs: u64) -> LocalStamp {
    let ticks = secs.checked_add(FILETIME_UNIX_OFFSET).and_then(|s| s.checked_mul(10_000_000)).filter(|t| *t < i64::MAX as u64);
    if let Some(ticks) = ticks {
        let filetime = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
        let mut utc = SYSTEMTIME::default();
        let mut local = SYSTEMTIME::default();
        let converted = unsafe { FileTimeToSystemTime(&filetime, &mut utc).is_ok() && SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_ok() };
        if converted {
            return LocalStamp {
                year: i32::from(local.wYear),
                month: u32::from(local.wMonth),
                day: u32::from(local.wDay),
                hour: u32::from(local.wHour),
                minute: u32::from(local.wMinute),
                second: u32::from(local.wSecond),
            };
        }
    }
    stamp_from_unix_offset(secs.min(i64::MAX as u64) as i64, 0)
}

pub(super) fn today() -> LocalStamp {
    local_from_unix(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
}

/// `<stem>-<unix epoch>-thumb.jpg` → stem (same rule as the backend's thumbnail index).
fn recorder_thumb_stem(name: &str) -> Option<&str> {
    let name = name.strip_suffix(".jpg")?.strip_suffix("-thumb")?;
    let (stem, epoch) = name.rsplit_once('-')?;
    (!stem.is_empty() && epoch.len() >= 9 && epoch.chars().all(|c| c.is_ascii_digit())).then_some(stem)
}

/// Recorder thumbnails per lowercase stem, one listing per `thumbs` folder.
fn thumbs_by_stem(folder: &Path) -> HashMap<String, Vec<PathBuf>> {
    let mut thumbs: HashMap<String, Vec<PathBuf>> = HashMap::new();
    let Ok(listing) = std::fs::read_dir(folder.join("thumbs")) else { return thumbs };
    for item in listing.flatten() {
        let path = item.path();
        let Some(stem) = path.file_name().and_then(|n| n.to_str()).and_then(recorder_thumb_stem) else { continue };
        if path.is_file() {
            thumbs.entry(stem.to_lowercase()).or_default().push(path);
        }
    }
    thumbs
}

/// Plain absolute path; SHFileOperation rejects `\\?\` verbatim paths.
fn shell_path(path: &Path) -> Option<PathBuf> {
    let absolute = std::path::absolute(path).ok()?;
    let text = absolute.to_string_lossy();
    Some(text.strip_prefix(r"\\?\").map(PathBuf::from).unwrap_or(absolute))
}

/// Each existing clip plus its recorder thumbnails and edit sidecars. Returns (clips, all files).
pub(super) fn recycle_targets(clips: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut thumb_cache: HashMap<PathBuf, HashMap<String, Vec<PathBuf>>> = HashMap::new();
    let mut moved_clips = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    for clip in clips {
        let Some(clip) = shell_path(clip).filter(|p| p.is_file()) else { continue };
        let folder = clip.parent().map(Path::to_path_buf).unwrap_or_default();
        let stem = clip.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
        let thumbs = thumb_cache.entry(folder.clone()).or_insert_with(|| thumbs_by_stem(&folder));
        files.push(clip.clone());
        files.extend(thumbs.get(&stem).into_iter().flatten().cloned());
        files.extend(super::sidecar_candidates(&clip).into_iter().filter(|p| p.is_file()));
        moved_clips.push(clip);
    }
    files.sort();
    files.dedup();
    (moved_clips, files)
}

pub(super) fn recycle(clips: &[PathBuf]) -> Result<usize, String> {
    let (clips, files) = recycle_targets(clips);
    if files.is_empty() {
        return Ok(0);
    }
    // Double-NUL-terminated list of NUL-separated paths.
    let mut list: Vec<u16> = Vec::new();
    for file in &files {
        list.extend(file.as_os_str().encode_wide());
        list.push(0);
    }
    list.push(0);
    let mut operation = SHFILEOPSTRUCTW {
        hwnd: HWND(std::ptr::null_mut()),
        wFunc: FO_DELETE,
        pFrom: PCWSTR(list.as_ptr()),
        pTo: PCWSTR::null(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI).0 as u16,
        fAnyOperationsAborted: BOOL(0),
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: PCWSTR::null(),
    };
    let code = unsafe { SHFileOperationW(&mut operation) };
    // Count what actually left the folder; a partial move returns the smaller count.
    let moved = clips.iter().filter(|clip| !clip.exists()).count();
    if moved == 0 && !clips.is_empty() {
        return Err(format!("Recycle Bin move failed (shell error {code:#x})"));
    }
    Ok(moved)
}

/// Nearest existing directory at or above `path` (a file's folder, or an ancestor of a
/// path that does not exist yet).
fn existing_dir(path: &Path) -> Option<PathBuf> {
    let absolute = shell_path(path)?;
    absolute.ancestors().find(|dir| dir.is_dir()).map(Path::to_path_buf)
}

pub(super) fn drive_space(path: &Path) -> Option<(u64, u64)> {
    let dir = existing_dir(path)?;
    let mut wide: Vec<u16> = dir.as_os_str().encode_wide().collect();
    if wide.last() != Some(&u16::from(b'\\')) {
        wide.push(u16::from(b'\\'));
    }
    wide.push(0);
    let (mut free, mut total) = (0u64, 0u64);
    unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut free as *mut u64), Some(&mut total as *mut u64), None) }.ok()?;
    (total > 0).then_some((free, total))
}
