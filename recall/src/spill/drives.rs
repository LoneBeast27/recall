//! Where the spill file goes: a fixed drive on a different physical disk from the
//! game (one with room for the whole buffer first, then SSD before HDD), the game's
//! own disk last, and never one left with less than
//! `KEEP_FREE`. Shared PCIe lanes are not detected: at 25-50 Mbps the spill writes
//! 3-6 MB/s, far below any lane's bandwidth; what competes is the game streaming
//! from the same disk, so "same physical disk" is the rule.

use std::path::{Path, PathBuf};

/// A chosen spill folder and file size.
pub(super) struct Place {
    pub dir: PathBuf,
    pub cap: u64,
    /// For the log: why this drive.
    pub why: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Drive {
    /// `"E:\\"`.
    pub root: String,
    /// Physical disk number (`None` for a volume across several disks, or unknown).
    pub disk: Option<u32>,
    /// Solid state (no seek penalty); `None` when the drive did not say.
    pub ssd: Option<bool>,
    /// Bytes free for this user.
    pub free: u64,
    /// The Windows drive (page file, temp, the OS's own I/O).
    pub system: bool,
}

/// The best drive for a spill file of up to `want` bytes, and its size there.
pub(super) fn rank(drives: &[Drive], game_disk: Option<u32>, want: u64, keep_free: u64, min_file: u64) -> Option<(usize, u64)> {
    drives
        .iter()
        .enumerate()
        .filter_map(|(i, d)| {
            let cap = want.min(d.free.saturating_sub(keep_free));
            (cap >= min_file).then_some((i, cap, d))
        })
        .min_by_key(|(_, cap, d)| {
            let same_disk = match (d.disk, game_disk) {
                (Some(a), Some(b)) if a == b => 2,
                (Some(_), Some(_)) => 0,
                _ => 1,
            };
            let media = match d.ssd {
                Some(true) => 0,
                None => 1,
                Some(false) => 2,
            };
            (same_disk, *cap < want, media, d.system, std::cmp::Reverse(d.free))
        })
        .map(|(i, cap, _)| (i, cap))
}

/// The drive letter root of `path` (`"E:\\"`), when it has one.
pub(super) fn root_of(path: &Path) -> Option<String> {
    let text = path.to_string_lossy();
    let mut chars = text.chars();
    let letter = chars.next().filter(|c| c.is_ascii_alphabetic())?;
    (chars.next() == Some(':')).then(|| format!("{}:\\", letter.to_ascii_uppercase()))
}

/// The spill folder on `drive`: the user's temp folder on the Windows drive (its root
/// is not writable without admin), else `<root>\.recall-spill`. Never inside the clip
/// folder, where the library would see it.
pub(super) fn dir_on(drive: &Drive) -> PathBuf {
    let temp = std::env::temp_dir();
    if drive.system && root_of(&temp).as_deref() == Some(drive.root.as_str()) {
        return temp.join("recall-spill");
    }
    PathBuf::from(&drive.root).join(".recall-spill")
}

/// Pick the spill folder for a file of up to `want` bytes. `game` is the captured
/// game's exe, when one is known.
pub(super) fn choose(want: u64, keep_free: u64, min_file: u64, game: Option<&Path>) -> Result<Place, String> {
    let drives = sys::fixed_drives();
    let game_root = game.and_then(root_of);
    let game_disk = game_root.as_deref().and_then(|root| {
        drives.iter().find(|d| d.root == root).and_then(|d| d.disk)
    });
    let (i, cap) = rank(&drives, game_disk, want, keep_free, min_file)
        .ok_or_else(|| format!("no fixed drive has {} MiB free beyond the {} GiB kept free", min_file >> 20, keep_free >> 30))?;
    let d = &drives[i];
    let media = match d.ssd {
        Some(true) => "SSD",
        Some(false) => "HDD",
        None => "drive",
    };
    let relation = match (d.disk, game_disk) {
        (Some(a), Some(b)) if a == b => "the game's own disk: no other drive had room",
        (Some(_), Some(_)) => "not the game's disk",
        _ if game_root.is_none() => "no game exe known",
        _ => "disk layout unknown",
    };
    Ok(Place { dir: dir_on(d), cap, why: format!("{media} {}, {relation}", d.root) })
}

#[cfg(windows)]
pub(super) mod sys {
    //! kernel32 calls, declared here so the engine needs no extra `windows` features.
    use super::Drive;
    use std::ffi::c_void;

    // Same signature as audio/voice.rs declares, so the two kernel32 imports do not clash.
    type Handle = *mut c_void;
    const OPEN_EXISTING: u32 = 3;
    const FILE_SHARE_READ_WRITE: u32 = 0x1 | 0x2;
    const DRIVE_FIXED: u32 = 3;
    const IOCTL_STORAGE_GET_DEVICE_NUMBER: u32 = 0x002D_1080;
    const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
    const STORAGE_DEVICE_SEEK_PENALTY_PROPERTY: u32 = 7;
    const PROPERTY_STANDARD_QUERY: u32 = 0;

    #[repr(C)]
    struct StorageDeviceNumber {
        device_type: u32,
        device_number: u32,
        partition_number: u32,
    }

    #[repr(C)]
    struct StoragePropertyQuery {
        property_id: u32,
        query_type: u32,
        additional: [u8; 4],
    }

    #[repr(C)]
    struct SeekPenaltyDescriptor {
        version: u32,
        size: u32,
        incurs_seek_penalty: u8,
    }

    #[link(name = "kernel32", kind = "raw-dylib")]
    extern "system" {
        fn GetLogicalDrives() -> u32;
        fn GetDriveTypeW(root: *const u16) -> u32;
        fn GetDiskFreeSpaceExW(dir: *const u16, free_to_caller: *mut u64, total: *mut u64, total_free: *mut u64) -> i32;
        fn CreateFileW(name: *const u16, access: u32, share: u32, security: *const c_void, disposition: u32, flags: u32, template: Handle) -> Handle;
        fn DeviceIoControl(
            device: Handle,
            code: u32,
            input: *const c_void,
            input_len: u32,
            output: *mut c_void,
            output_len: u32,
            returned: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Every fixed drive with its disk, media and free space.
    pub(in crate::spill) fn fixed_drives() -> Vec<Drive> {
        let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()).to_ascii_uppercase();
        // SAFETY: no arguments; returns a bitmask of drive letters.
        let mask = unsafe { GetLogicalDrives() };
        (0..26u8)
            .filter(|bit| mask & (1 << bit) != 0)
            .filter_map(|bit| {
                let letter = (b'A' + bit) as char;
                let root = format!("{letter}:\\");
                let root_w = wide(&root);
                // SAFETY: a NUL-terminated root path.
                if unsafe { GetDriveTypeW(root_w.as_ptr()) } != DRIVE_FIXED {
                    return None;
                }
                let mut free = 0u64;
                // SAFETY: NUL-terminated path; out pointers are valid or null.
                let ok = unsafe { GetDiskFreeSpaceExW(root_w.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
                if ok == 0 {
                    return None;
                }
                let (disk, ssd) = probe(letter);
                Some(Drive { root, disk, ssd, free, system: system.starts_with(letter) })
            })
            .collect()
    }

    /// (physical disk number, solid state) of a volume. Access 0 needs no admin.
    fn probe(letter: char) -> (Option<u32>, Option<bool>) {
        let name = wide(&format!("\\\\.\\{letter}:"));
        // SAFETY: NUL-terminated device path; query-only open with no access rights.
        let handle = unsafe { CreateFileW(name.as_ptr(), 0, FILE_SHARE_READ_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut()) };
        if handle as isize == -1 || handle.is_null() {
            return (None, None);
        }
        let mut number = StorageDeviceNumber { device_type: 0, device_number: 0, partition_number: 0 };
        let mut returned = 0u32;
        // SAFETY: output buffer is a StorageDeviceNumber of the stated size.
        let disk = unsafe {
            DeviceIoControl(
                handle,
                IOCTL_STORAGE_GET_DEVICE_NUMBER,
                std::ptr::null(),
                0,
                (&mut number as *mut StorageDeviceNumber).cast(),
                std::mem::size_of::<StorageDeviceNumber>() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        } != 0;
        let query = StoragePropertyQuery {
            property_id: STORAGE_DEVICE_SEEK_PENALTY_PROPERTY,
            query_type: PROPERTY_STANDARD_QUERY,
            additional: [0; 4],
        };
        let mut seek = SeekPenaltyDescriptor { version: 0, size: 0, incurs_seek_penalty: 0 };
        // SAFETY: input and output are the documented structs with their sizes.
        let media = unsafe {
            DeviceIoControl(
                handle,
                IOCTL_STORAGE_QUERY_PROPERTY,
                (&query as *const StoragePropertyQuery).cast(),
                std::mem::size_of::<StoragePropertyQuery>() as u32,
                (&mut seek as *mut SeekPenaltyDescriptor).cast(),
                std::mem::size_of::<SeekPenaltyDescriptor>() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        } != 0;
        // SAFETY: the handle came from CreateFileW above.
        unsafe { CloseHandle(handle) };
        (disk.then_some(number.device_number), media.then_some(seek.incurs_seek_penalty == 0))
    }
}

#[cfg(not(windows))]
pub(super) mod sys {
    pub(in crate::spill) fn fixed_drives() -> Vec<super::Drive> {
        Vec::new()
    }
}
