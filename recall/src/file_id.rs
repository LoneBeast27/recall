//! Which file a path names, asked of Windows: the volume serial number plus the file index
//! (`GetFileInformationByHandle`). Path text cannot answer it: `\\nas\x` and `\\10.0.0.5\x`,
//! a junction, a hard link or a subst drive name one file under different text, and
//! `canonicalize` fails outright on some RAM disks and user-mode file systems (WinFsp, Dokan).
//!
//! An answer Windows cannot give is `None`, never "different": a caller about to delete a
//! file because it is "another copy" must stop on `None`.
//!
//! The one copy of these calls: the engine's mover and staging guard use it, and the app
//! (`recall-native`) includes this file by path for its cache guard.
use std::os::windows::ffi::OsStrExt;
use std::ffi::c_void;
use std::path::Path;

/// The same handle type as the other kernel32 declarations in the engine (one signature each).
type Handle = *mut c_void;

#[repr(C)]
#[derive(Default)]
struct ByHandleInfo {
    attributes: u32,
    created: [u32; 2],
    accessed: [u32; 2],
    written: [u32; 2],
    volume_serial: u32,
    size_high: u32,
    size_low: u32,
    links: u32,
    index_high: u32,
    index_low: u32,
}

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn CreateFileW(name: *const u16, access: u32, share: u32, security: *const c_void, disposition: u32, flags: u32, template: Handle) -> Handle;
    fn GetFileInformationByHandle(file: Handle, info: *mut ByHandleInfo) -> i32;
    fn CloseHandle(handle: Handle) -> i32;
}

/// Read, write and delete sharing: asking never gets in another program's way.
const SHARE_ALL: u32 = 0x7;
const OPEN_EXISTING: u32 = 3;
/// Lets the handle open a folder too (and follows junctions to their target).
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

/// One file on one volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileId {
    pub volume: u32,
    pub index: u64,
}

/// The identity of the file or folder at `path`; `None` when it cannot be opened or the file
/// system reports no index (some network and user-mode file systems answer 0).
pub fn file_id(path: &Path) -> Option<FileId> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    // SAFETY: a no-access handle (attributes only) on a NUL-terminated path, closed before
    // returning; the info struct is a plain repr(C) value Windows fills in.
    unsafe {
        let handle = CreateFileW(wide.as_ptr(), 0, SHARE_ALL, std::ptr::null(), OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, std::ptr::null_mut());
        // INVALID_HANDLE_VALUE is -1.
        if handle.is_null() || handle as isize == -1 {
            return None;
        }
        let mut info = ByHandleInfo::default();
        let read = GetFileInformationByHandle(handle, &mut info) != 0;
        CloseHandle(handle);
        let index = (u64::from(info.index_high) << 32) | u64::from(info.index_low);
        (read && index != 0).then_some(FileId { volume: info.volume_serial, index })
    }
}

/// `Some(true)`: one file; `Some(false)`: two files; `None`: Windows could not say for one.
pub fn same_file(a: &Path, b: &Path) -> Option<bool> {
    Some(file_id(a)? == file_id(b)?)
}

/// `Some(true)` when `inner` is the folder `outer` or lies inside it, by identity: each existing
/// folder on `inner`'s path is compared with `outer`. `None` when `outer` cannot be identified.
pub fn inside(inner: &Path, outer: &Path) -> Option<bool> {
    let target = file_id(outer)?;
    let absolute = std::path::absolute(inner).ok()?;
    Some(absolute.ancestors().filter_map(file_id).any(|id| id == target))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(tag: &str) -> std::path::PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".tmp").join("sandbox").join(format!("file-id-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_hard_link_is_the_same_file_and_a_copy_is_not() {
        let dir = sandbox("link");
        let (a, link, copy) = (dir.join("a.mp4"), dir.join("b.mp4"), dir.join("c.mp4"));
        std::fs::write(&a, b"clip").unwrap();
        std::fs::hard_link(&a, &link).unwrap();
        std::fs::copy(&a, &copy).unwrap();
        assert_eq!(same_file(&a, &link), Some(true), "two names, one file");
        assert_eq!(same_file(&a, &copy), Some(false), "same bytes, another file");
        assert_eq!(same_file(&a, &dir.join("missing.mp4")), None, "unknown is not different");
        assert_eq!(inside(&dir.join("sub").join("x"), &dir), Some(true), "a path not made yet, under the folder");
        assert_eq!(inside(&dir, &dir.join("missing")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
