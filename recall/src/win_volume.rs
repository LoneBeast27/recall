//! Volume questions asked of Windows, never of the path text: which volume a
//! folder really lives on ([`volume_of`]) and its free space ([`free_bytes`]).
//! Junctions, symlinks and subst drives resolve to their target first, so a
//! clip folder `C:\Clips` that is a junction to `V:\Clips` counts as `V:`.
//!
//! The one copy of these kernel32 calls: the engine uses it for the spill drive
//! choice, the save free-space check and staging; the controller (`recall-hdr`)
//! includes this file by path for its free-space warning.

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf, Prefix};
use std::time::{Duration, Instant};

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

/// `fn(existing folder) -> its final path` (`std::fs::canonicalize`; injectable for tests).
pub type Canon = fn(&Path) -> std::io::Result<PathBuf>;

/// `dir`, or its nearest existing ancestor (a folder not made yet), with
/// junctions, symlinks and subst drives resolved. When resolving fails
/// (ImDisk, VeraCrypt, cloud volumes, a denied parent) the folder as written
/// is used, so its free space is still read. `None`: nothing on the path exists.
pub fn resolve(dir: &Path) -> Option<PathBuf> {
    resolve_with(dir, |p| std::fs::canonicalize(p))
}

pub fn resolve_with(dir: &Path, canon: Canon) -> Option<PathBuf> {
    let abs = std::path::absolute(dir).ok()?;
    let found = abs.ancestors().find(|p| p.is_dir())?;
    Some(canon(found).map_or_else(|_| found.to_path_buf(), |real| plain(&real)))
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
// Uncached: the controller's storage thread uses it; the engine uses the cached call.
#[allow(dead_code)]
pub fn volume_of(dir: &Path) -> Option<Volume> {
    volume_at(&resolve(dir)?)
}

/// The volume of an already resolved folder.
fn volume_at(real: &Path) -> Option<Volume> {
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
    free_at(&resolve(dir)?)
}

/// GetDiskFreeSpaceExW on an existing folder: the one call site in Recall.
fn free_at(real: &Path) -> Option<u64> {
    let mut free = 0u64;
    // SAFETY: NUL-terminated path; the out pointer is valid, the others null.
    let ok = unsafe { GetDiskFreeSpaceExW(wide(real.as_os_str()).as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(free)
}

/// `RECALL_FAKE_FREE_BYTES=<n>` (checks only: the low-disk fixture; the app's
/// drive info reads the same variable).
fn fake_env() -> Option<u64> {
    std::env::var("RECALL_FAKE_FREE_BYTES").ok().and_then(|v| v.trim().parse::<u64>().ok())
}

/// A real reading capped by the fake one; the fake stands alone when the real read failed.
pub fn with_fake(real: Option<u64>, fake: Option<u64>) -> Option<u64> {
    match (real, fake) {
        (Some(free), Some(fake)) => Some(free.min(fake)),
        (real, fake) => real.or(fake),
    }
}

/// [`free_bytes_raw`] with `RECALL_FAKE_FREE_BYTES` applied.
// Uncached: the controller's storage thread uses it; the engine uses the cached call.
#[allow(dead_code)]
pub fn free_bytes(dir: &Path) -> Option<u64> {
    with_fake(free_bytes_raw(dir), fake_env())
}

/// What a folder resolves to, kept by [`VolumeCache`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub real: Option<PathBuf>,
    pub volume: Option<Volume>,
}

/// `fn(folder) -> Resolved` (the real one is [`lookup`]).
pub type Lookup = fn(&Path) -> Resolved;

pub fn lookup(dir: &Path) -> Resolved {
    let real = resolve(dir);
    let volume = real.as_deref().and_then(volume_at);
    Resolved { real, volume }
}

/// Re-resolve a folder after this long (a volume remounted elsewhere is seen within it).
pub const CACHE_TTL: Duration = Duration::from_secs(60);
const CACHE_MAX: usize = 64;

/// Per-folder resolve results, so a thread that saves often (the saver, the
/// mover) resolves each save folder once per [`CACHE_TTL`], not once per save.
pub struct VolumeCache {
    lookup: Lookup,
    ttl: Duration,
    entries: HashMap<PathBuf, (Instant, Resolved)>,
}

impl VolumeCache {
    pub fn new(lookup: Lookup, ttl: Duration) -> Self {
        VolumeCache { lookup, ttl, entries: HashMap::new() }
    }

    pub fn get(&mut self, dir: &Path, now: Instant) -> Resolved {
        if let Some((at, found)) = self.entries.get(dir) {
            if now.saturating_duration_since(*at) < self.ttl {
                return found.clone();
            }
        }
        let found = (self.lookup)(dir);
        if self.entries.len() >= CACHE_MAX {
            self.entries.clear();
        }
        self.entries.insert(dir.to_path_buf(), (now, found.clone()));
        found
    }
}

thread_local! {
    static CACHE: RefCell<VolumeCache> = RefCell::new(VolumeCache::new(lookup, CACHE_TTL));
}

fn cached(dir: &Path) -> Resolved {
    CACHE.with(|c| c.borrow_mut().get(dir, Instant::now()))
}

/// [`volume_of`] through this thread's [`VolumeCache`].
pub fn volume_of_cached(dir: &Path) -> Option<Volume> {
    cached(dir).volume
}

/// [`free_bytes`] with the folder resolved through this thread's [`VolumeCache`]
/// (free space itself is read every time).
pub fn free_bytes_cached(dir: &Path) -> Option<u64> {
    with_fake(cached(dir).real.as_deref().and_then(free_at), fake_env())
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

    fn denied(_: &Path) -> std::io::Result<PathBuf> {
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
    }

    #[test]
    fn a_folder_that_cannot_be_canonicalized_is_still_read_as_written() {
        // ImDisk / VeraCrypt / cloud volumes / a denied parent: canonicalize fails.
        let temp = std::path::absolute(std::env::temp_dir()).unwrap();
        let missing = temp.join("recall-win-volume-not-made");
        assert_eq!(resolve_with(&missing, denied).as_deref(), Some(temp.as_path()), "the nearest existing folder, as written");
        let real = resolve_with(&missing, denied).and_then(|p| free_at(&p));
        assert!(real.is_some_and(|b| b > 0), "free space still read");
    }

    #[test]
    fn the_fake_free_space_applies_even_when_the_real_read_fails() {
        assert_eq!(with_fake(Some(900), Some(5)), Some(5));
        assert_eq!(with_fake(Some(3), Some(5)), Some(3));
        assert_eq!(with_fake(Some(900), None), Some(900));
        assert_eq!(with_fake(None, Some(5)), Some(5), "the low-disk fixture works where reads fail");
        assert_eq!(with_fake(None, None), None);
    }

    static LOOKUPS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn counting(dir: &Path) -> Resolved {
        LOOKUPS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Resolved { real: Some(dir.to_path_buf()), volume: None }
    }

    #[test]
    fn a_save_folder_is_resolved_once_per_ttl() {
        let mut cache = VolumeCache::new(counting, Duration::from_secs(60));
        let t0 = Instant::now();
        let dir = Path::new(r"E:\Clips");
        cache.get(dir, t0);
        cache.get(dir, t0 + Duration::from_secs(30));
        assert_eq!(LOOKUPS.load(std::sync::atomic::Ordering::SeqCst), 1, "second save reuses it");
        cache.get(dir, t0 + Duration::from_secs(61));
        assert_eq!(LOOKUPS.load(std::sync::atomic::Ordering::SeqCst), 2, "re-resolved after the TTL");
    }
}
