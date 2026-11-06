//! Volume questions asked of Windows, never of the path text: which volume a
//! folder really lives on ([`volume_of`]) and its free space ([`free_bytes`]).
//! Junctions, symlinks and subst drives resolve to their target first, so a
//! clip folder `C:\Clips` that is a junction to `V:\Clips` counts as `V:`.
//!
//! The one copy of these kernel32 calls: the engine uses it for the spill drive
//! choice, the save free-space check and staging; the controller (`recall-hdr`)
//! includes this file by path for its free-space warning.

use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf, Prefix};

#[link(name = "kernel32", kind = "raw-dylib")]
extern "system" {
    fn GetDiskFreeSpaceExW(dir: *const u16, free_to_caller: *mut u64, total: *mut u64, total_free: *mut u64) -> i32;
    fn GetVolumePathNameW(path: *const u16, out: *mut u16, len: u32) -> i32;
    fn GetVolumeNameForVolumeMountPointW(mount: *const u16, out: *mut u16, len: u32) -> i32;
}

/// The volume a folder lives on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    /// `\\?\Volume{GUID}\`, or the lowercased mount path where Windows has no
    /// GUID (network shares). Two folders are on one volume when ids match.
    pub id: String,
    /// What the user knows the drive as: `C:`, `\\nas\clips`, or the folder a
    /// volume is mounted on (`D:\mnt\fast`).
    pub label: String,
}

fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
    text.encode_wide().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// `\\?\C:\x` -> `C:\x`, `\\?\UNC\server\share\x` -> `\\server\share\x`.
pub fn plain(path: &Path) -> PathBuf {
    let mut parts = path.components();
    let root = match parts.next() {
        Some(Component::Prefix(p)) => match p.kind() {
            Prefix::VerbatimDisk(d) => format!("{}:\\", d as char),
            Prefix::VerbatimUNC(server, share) => format!(r"\\{}\{}\", server.to_string_lossy(), share.to_string_lossy()),
            _ => return path.to_path_buf(),
        },
        _ => return path.to_path_buf(),
    };
    let mut out = PathBuf::from(root);
    out.extend(parts.filter(|c| !matches!(c, Component::RootDir)));
    out
}

/// `dir`, or its nearest existing ancestor (a folder not made yet), with
/// junctions, symlinks and subst drives resolved. `None`: nothing on the path exists.
pub fn resolve(dir: &Path) -> Option<PathBuf> {
    let abs = std::path::absolute(dir).ok()?;
    let found = abs.ancestors().find(|p| p.is_dir())?;
    Some(plain(&std::fs::canonicalize(found).ok()?))
}

/// `C:\` -> `C:`, `\\NAS\Clips\` -> `\\nas\clips`, `D:\mnt\fast\` -> `D:\mnt\fast`.
pub fn label_of(mount: &str) -> String {
    let trimmed = mount.trim_end_matches('\\');
    if trimmed.starts_with(r"\\") {
        return trimmed.to_lowercase();
    }
    let mut chars = trimmed.chars();
    match chars.next() {
        Some(letter) => format!("{}{}", letter.to_ascii_uppercase(), chars.as_str()),
        None => mount.to_owned(),
    }
}

/// The volume `dir` really lives on (`None` when Windows cannot say).
pub fn volume_of(dir: &Path) -> Option<Volume> {
    let real = resolve(dir)?;
    let mut mount = [0u16; 1024];
    // SAFETY: NUL-terminated path; `mount` is writable for the length given.
    if unsafe { GetVolumePathNameW(wide(real.as_os_str()).as_ptr(), mount.as_mut_ptr(), mount.len() as u32) } == 0 {
        return None;
    }
    let mount = from_wide(&mount);
    let mut guid = [0u16; 64];
    // SAFETY: NUL-terminated mount path ending in `\`; `guid` is writable for the length given.
    let named = unsafe { GetVolumeNameForVolumeMountPointW(wide(std::ffi::OsStr::new(&mount)).as_ptr(), guid.as_mut_ptr(), guid.len() as u32) } != 0;
    let id = if named { from_wide(&guid) } else { mount.to_lowercase() };
    Some(Volume { id, label: label_of(&mount) })
}

/// Free bytes for the caller on the volume `dir` really lives on.
pub fn free_bytes_raw(dir: &Path) -> Option<u64> {
    let real = resolve(dir)?;
    let mut free = 0u64;
    // SAFETY: NUL-terminated path; the out pointer is valid, the others null.
    let ok = unsafe { GetDiskFreeSpaceExW(wide(real.as_os_str()).as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(free)
}

/// [`free_bytes_raw`] capped by `RECALL_FAKE_FREE_BYTES=<n>` (checks only: the
/// low-disk fixture; the app's drive info reads the same variable).
pub fn free_bytes(dir: &Path) -> Option<u64> {
    let fake = std::env::var("RECALL_FAKE_FREE_BYTES").ok().and_then(|v| v.trim().parse::<u64>().ok());
    free_bytes_raw(dir).map(|free| fake.map_or(free, |fake| fake.min(free)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_paths_become_plain() {
        assert_eq!(plain(Path::new(r"\\?\C:\Users\x")), PathBuf::from(r"C:\Users\x"));
        assert_eq!(plain(Path::new(r"\\?\UNC\nas\clips\a")), PathBuf::from(r"\\nas\clips\a"));
        assert_eq!(plain(Path::new(r"E:\Clips")), PathBuf::from(r"E:\Clips"));
    }

    #[test]
    fn mount_paths_become_labels() {
        assert_eq!(label_of(r"c:\"), "C:");
        assert_eq!(label_of(r"\\NAS\Clips\"), r"\\nas\clips");
        assert_eq!(label_of(r"D:\mnt\fast\"), r"D:\mnt\fast");
    }

    #[test]
    fn the_temp_folder_has_a_named_volume_and_free_space() {
        let missing = std::env::temp_dir().join("recall-win-volume-not-made").join("deeper");
        let v = volume_of(&missing).expect("temp resolves");
        assert!(v.id.starts_with(r"\\?\Volume{"), "{v:?}");
        assert_eq!(volume_of(&std::env::temp_dir()), Some(v));
        assert!(free_bytes_raw(&missing).is_some_and(|b| b > 0));
    }
}
