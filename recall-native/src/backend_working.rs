//! Where the SSD working copy of a clip lives. Name, size and write time alone are shared by a
//! byte-identical copy (Explorer keeps the write time), so two copies edited in turn overwrote
//! each other's working copy. The NTFS file id (volume serial + file index) tells them apart and
//! stays with a clip moved within its volume. Without an id (network share, FAT) the key is the
//! old one; a move to another volume is a new id and loads the sidecar beside the clip.
use super::*;

/// (volume serial, file index).
pub(super) type FileId = (u32, u64);

/// The clip's file id; None when it cannot be read.
pub(super) fn file_id(path: &Path) -> Option<FileId> {
    #[cfg(test)]
    if let Some(id) = tests::injected(path) {
        return id;
    }
    read_file_id(path)
}

fn read_file_id(path: &Path) -> Option<FileId> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};
    let file = std::fs::File::open(path).ok()?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: a valid handle owned by `file` for the call; `info` is written by the call.
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }.ok()?;
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    // FAT and some shares report no index: no id rather than one every file shares.
    (index != 0).then_some((info.dwVolumeSerialNumber, index))
}

/// The pre-id key: name, size and write time.
pub(super) fn legacy_path(info: &ClipInfo) -> Result<PathBuf, String> {
    let name = Path::new(&info.path).file_name().ok_or("source has no filename")?.to_string_lossy().into_owned();
    Ok(storage::working_copy_path(&storage::cache_root(), &name, info.source_bytes, info.source_modified_ms / 1000))
}

/// The working copy written for `info` with file id `id`.
pub(super) fn keyed_path(info: &ClipInfo, id: Option<FileId>) -> Result<PathBuf, String> {
    let legacy = legacy_path(info)?;
    let Some((volume, index)) = id else { return Ok(legacy) };
    let stem = legacy.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(legacy.with_file_name(format!("{stem}-{volume:08x}-{index:016x}.json")))
}

/// Where saves go.
pub(super) fn working_path(info: &ClipInfo) -> Result<PathBuf, String> {
    keyed_path(info, file_id(Path::new(&info.path)))
}

/// Where a load reads: the id-keyed copy, else one saved before ids were in the key.
pub(super) fn load_path(info: &ClipInfo) -> Result<PathBuf, String> {
    let keyed = working_path(info)?;
    if keyed.exists() {
        return Ok(keyed);
    }
    let legacy = legacy_path(info)?;
    Ok(if legacy.exists() { legacy } else { keyed })
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    thread_local! {
        static IDS: RefCell<HashMap<String, Option<FileId>>> = RefCell::new(HashMap::new());
    }

    /// Tests set the file id a path reports (None = cannot be read).
    pub(crate) fn inject(path: &str, id: Option<FileId>) {
        IDS.with(|ids| ids.borrow_mut().insert(path.to_lowercase(), id));
    }

    pub(super) fn injected(path: &Path) -> Option<Option<FileId>> {
        IDS.with(|ids| ids.borrow().get(&path.to_string_lossy().to_lowercase()).copied())
    }
}
