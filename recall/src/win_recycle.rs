//! Can a file really go to the Recycle Bin? Recall never deletes a user clip
//! for good, so before a clip is sent to the Recycle Bin (the storage limit in
//! `recall-hdr`, the library's delete in `recall-native`, which both include
//! this file by path) its volume must have a Recycle Bin that will take it:
//!
//! - a local fixed volume with a volume GUID (a `\\nas` share, a mapped
//!   network drive or a removable drive has no Recycle Bin: Windows would
//!   delete for good, or ask whether to). A `subst` drive letter is refused
//!   before anything is resolved (`QueryDosDeviceW` names a `\??\` path:
//!   Windows deletes there for good, measured 2026-11-07), and so is a folder
//!   that resolves to a UNC path;
//! - the bin not turned off: neither the `NoRecycleFiles` policy (HKLM, else
//!   HKCU `Software\Microsoft\Windows\CurrentVersion\Policies\Explorer`) nor
//!   the volume's own `NukeOnDelete` (HKCU `...\Explorer\BitBucket\Volume\
//!   {GUID}`) is 1. All read only, never written;
//! - the clip and its companions no bigger than the bin's maximum size:
//!   the `RecycleBinSize` policy (percent of the volume; HKLM, else HKCU)
//!   when set, else the volume's `MaxCapacity` (MB), else Windows' default
//!   for a volume that has no `BitBucket\Volume` key yet ([`default_max_mb`]).
//!   A bigger item is the one Windows offers to delete permanently. The item
//!   must also fit the bin's FREE room (that size minus what the volume's bin
//!   already holds, `SHQueryRecycleBinW`): otherwise Windows purges older bin
//!   items to make room, which deletes those earlier files for good. The
//!   room must exceed the item by [`margin`] (min(1 GiB, 10% of the bin)):
//!   Windows' accounting rounds and reports `used` late right after a move.
//!   The check and the move run under one lock shared by every Recall
//!   process (`win_recycle_lock.rs`). An unreadable bin size refuses.
//!
//! A refusal stops the caller, which tells the user, and nothing is deleted.
//! This is the second guard: the call itself (`win_fileop.rs`) goes through
//! `IFileOperation` with a progress sink that refuses any delete Windows
//! would not recycle.

use std::path::Path;

use crate::win_volume::Volume;

const DRIVE_FIXED: u32 = 3;
const MIB: u64 = 1 << 20;

/// Windows' default Recycle Bin size for a volume of `total_bytes` (the
/// caller's quota, as `GetDiskFreeSpaceExW` reports it), in MB (MiB): "ten
/// percent of the first 40 GB of quota, and five percent of any quota above
/// 40 GB" (Windows Vista and later; Raymond Chen, The Old New Thing,
/// 2014-07-01). On this machine it reproduces the stored `MaxCapacity` of V:,
/// B:, L: and G: exactly (C: and R: differ by under 0.03%; measured 2026-11-07).
pub fn default_max_mb(total_bytes: u64) -> u64 {
    let total_mib = total_bytes / MIB;
    if total_mib <= 40 * 1024 { total_mib / 10 } else { 4096 + (total_mib - 40 * 1024) / 20 }
}

/// What Windows says about one volume's Recycle Bin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinFacts {
    /// The path's drive letter is a `subst` onto a folder (`QueryDosDeviceW` gives `\??\...`).
    pub subst: bool,
    /// The folder resolves to a UNC path (a share, wherever it is mounted).
    pub resolved_unc: bool,
    pub volume: Option<Volume>,
    /// `GetDriveTypeW` of the volume's root.
    pub drive_type: u32,
    /// Volume size (`None`: unknown).
    pub total_bytes: Option<u64>,
    /// Bytes the volume's Recycle Bin already holds (`None`: could not be read).
    pub bin_used: Option<u64>,
    pub settings: Settings,
}

/// The registry values that decide a volume's bin.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    /// `BitBucket\Volume\{GUID}` `NukeOnDelete` (`None`: no key yet: Windows' default, 0).
    pub nuke: Option<u32>,
    /// `BitBucket\Volume\{GUID}` `MaxCapacity` in MB (`None`: Windows' default size).
    pub max_mb: Option<u32>,
    /// The `NoRecycleFiles` policy is 1: every delete is permanent.
    pub policy_off: bool,
    /// The `RecycleBinSize` policy: the bin's size in percent of the volume.
    pub policy_pct: Option<u32>,
}

/// Which registry hive a value is read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hive {
    User,
    Machine,
}

/// `fn(hive, sub key, value) -> DWORD` (read only; tests inject one).
pub type RegRead = fn(Hive, &str, &str) -> Option<u32>;

const POLICY_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer";

/// The settings for the volume `{GUID}` (braces included). A machine policy
/// (HKLM) wins over the user's (HKCU), as Group Policy applies them; a size
/// policy wins over the volume's own `MaxCapacity` (it greys that out in the
/// Recycle Bin properties).
pub fn read_settings(guid: &str, read: RegRead) -> Settings {
    let key = format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume\{guid}");
    let policy = |value: &str| read(Hive::Machine, POLICY_KEY, value).or_else(|| read(Hive::User, POLICY_KEY, value));
    Settings {
        nuke: read(Hive::User, &key, "NukeOnDelete"),
        max_mb: read(Hive::User, &key, "MaxCapacity"),
        policy_off: policy("NoRecycleFiles") == Some(1),
        policy_pct: policy("RecycleBinSize"),
    }
}

/// `fn(file) -> its size in bytes` (`None`: could not be read; tests inject one).
pub type SizeFn = fn(&Path) -> Option<u64>;

/// The production [`SizeFn`].
pub fn file_size(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len())
}

/// The bytes one Recycle Bin call moves: every file summed; a size that cannot
/// be read refuses (it could be anything, and the bin must not overflow).
pub fn total_size(files: &[std::path::PathBuf], size: SizeFn) -> Result<u64, String> {
    files.iter().try_fold(0u64, |sum, f| match size(f) {
        Some(n) => Ok(sum + n),
        None => Err(format!("couldn't read the size of {}", f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())),
    })
}

/// `fn(folder, bytes) -> Ok, or why these bytes cannot go to its Recycle Bin`.
pub type BinCheck = fn(&Path, u64) -> Result<(), String>;

/// The bin's maximum size in bytes (`None`: unknown).
fn max_bytes(facts: &BinFacts) -> Option<u64> {
    let s = &facts.settings;
    match (s.policy_pct, s.max_mb) {
        (Some(pct), _) => facts.total_bytes.map(|t| t / 100 * u64::from(pct.min(100))),
        (None, Some(mb)) => Some(u64::from(mb) * MIB),
        (None, None) => facts.total_bytes.map(|t| default_max_mb(t) * MIB),
    }
}

/// Room the bin must keep beyond an item: min(1 GiB, 10% of its size `max`). Windows deletes
/// an item for good when the bin cannot take it (or purges older ones), so an item must fit
/// with this spare; 10% lets a small bin (1,000 MB) still take a clip.
pub fn margin(max: u64) -> u64 {
    (max / 10).min(1 << 30)
}

/// May `bytes` from this volume go to its Recycle Bin?
pub fn decide(facts: &BinFacts, bytes: u64) -> Result<(), String> {
    if facts.subst {
        return Err("the folder is on a subst drive, which has no Recycle Bin".into());
    }
    if facts.resolved_unc {
        return Err("the folder is on a network share, which has no Recycle Bin".into());
    }
    let Some(volume) = facts.volume.as_ref().filter(|v| v.id.starts_with(r"\\?\Volume{")) else {
        return Err("the drive has no Recycle Bin (a network share or an unknown drive)".into());
    };
    if facts.drive_type != DRIVE_FIXED {
        return Err(format!("{} has no Recycle Bin (a removable or network drive)", volume.label));
    }
    if facts.settings.policy_off {
        return Err("the Recycle Bin is turned off by a policy (files are deleted right away)".into());
    }
    if facts.settings.nuke == Some(1) {
        return Err(format!("the Recycle Bin is turned off for {} (files are deleted right away)", volume.label));
    }
    let Some(max) = max_bytes(facts) else {
        return Err(format!("the Recycle Bin size for {} is unknown", volume.label));
    };
    if bytes > max {
        return Err(format!("the clip is bigger than the Recycle Bin on {} ({} MB)", volume.label, max / MIB));
    }
    let Some(used) = facts.bin_used else {
        return Err(format!("couldn't read how full the Recycle Bin on {} is", volume.label));
    };
    let room = max.saturating_sub(used);
    let spare = margin(max);
    if bytes.saturating_add(spare) > room {
        let (mb, need) = (bytes.div_ceil(MIB), bytes.saturating_add(spare).div_ceil(MIB));
        return Err(format!(
            "not enough room left in the Recycle Bin on {}: {mb} MB needs {need} MB free ({} MB spare, so Windows never deletes anything for good), and it has {} MB free of {} MB; empty the Recycle Bin or make it bigger",
            volume.label,
            spare / MIB,
            room / MIB,
            max / MIB
        ));
    }
    Ok(())
}

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn GetDriveTypeW(root: *const u16) -> u32;
    fn QueryDosDeviceW(device: *const u16, target: *mut u16, max: u32) -> u32;
}

#[repr(C)]
struct QueryRbInfo {
    size_of: u32,
    bytes: i64,
    items: i64,
}

#[link(name = "shell32", kind = "raw-dylib")]
unsafe extern "system" {
    fn SHQueryRecycleBinW(root: *const u16, info: *mut QueryRbInfo) -> i32;
}

/// Bytes the Recycle Bin of the volume at `root` (`D:\\`) holds now (read only).
pub fn bin_used(root: &str) -> Option<u64> {
    let path = wide(root);
    let mut info = QueryRbInfo { size_of: std::mem::size_of::<QueryRbInfo>() as u32, bytes: 0, items: 0 };
    // SAFETY: a NUL-terminated root path; `info` is sized as cbSize says.
    let code = unsafe { SHQueryRecycleBinW(path.as_ptr(), &mut info) };
    (code == 0 && info.bytes >= 0).then_some(info.bytes as u64)
}

/// `fn(drive letter) -> its DOS device target` (`QueryDosDeviceW`; tests inject one).
pub type DosQuery = fn(char) -> Option<String>;

/// The first target of `X:` (`\Device\HarddiskVolume3`, or `\??\D:\folder` for a subst).
pub fn dos_target(letter: char) -> Option<String> {
    let name = wide(&format!("{letter}:"));
    let mut out = [0u16; 1024];
    // SAFETY: a NUL-terminated device name; `out` is writable for the length given.
    let n = unsafe { QueryDosDeviceW(name.as_ptr(), out.as_mut_ptr(), out.len() as u32) };
    let end = out[..n as usize].iter().position(|c| *c == 0).unwrap_or(n as usize);
    (n > 0).then(|| String::from_utf16_lossy(&out[..end]))
}

/// The drive letter of `dir` as written (before anything is resolved).
fn letter_of(dir: &Path) -> Option<char> {
    let abs = std::path::absolute(dir).ok()?;
    match abs.components().next()? {
        std::path::Component::Prefix(p) => match p.kind() {
            std::path::Prefix::Disk(d) | std::path::Prefix::VerbatimDisk(d) => Some((d as char).to_ascii_uppercase()),
            _ => None,
        },
        _ => None,
    }
}

/// Is `dir`'s drive letter a subst (checked before resolving)?
pub fn is_subst(dir: &Path, dos: DosQuery) -> bool {
    letter_of(dir).and_then(dos).is_some_and(|t| t.starts_with(r"\??\"))
}

#[link(name = "advapi32", kind = "raw-dylib")]
unsafe extern "system" {
    fn RegGetValueW(key: isize, sub_key: *const u16, value: *const u16, flags: u32, kind: *mut u32, data: *mut u32, size: *mut u32) -> i32;
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// One DWORD from the registry (read only): the production [`RegRead`].
pub fn reg_dword(hive: Hive, sub_key: &str, value: &str) -> Option<u32> {
    const HKEY_CURRENT_USER: isize = 0x8000_0001_u32 as i32 as isize;
    const HKEY_LOCAL_MACHINE: isize = 0x8000_0002_u32 as i32 as isize;
    const RRF_RT_REG_DWORD: u32 = 0x10;
    let root = match hive {
        Hive::User => HKEY_CURRENT_USER,
        Hive::Machine => HKEY_LOCAL_MACHINE,
    };
    let (key, name) = (wide(sub_key), wide(value));
    let (mut data, mut size) = (0u32, 4u32);
    // SAFETY: NUL-terminated strings; `data` holds the 4 bytes `size` says.
    let code = unsafe { RegGetValueW(root, key.as_ptr(), name.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), &mut data, &mut size) };
    (code == 0).then_some(data)
}

/// The facts for the volume `dir` really lives on (read only).
pub fn facts(dir: &Path) -> BinFacts {
    facts_with(dir, dos_target)
}

/// [`facts`] with an injected `QueryDosDeviceW`.
pub fn facts_with(dir: &Path, dos: DosQuery) -> BinFacts {
    let none = |subst, resolved_unc| BinFacts { subst, resolved_unc, volume: None, drive_type: 0, total_bytes: None, bin_used: None, settings: Settings::default() };
    if is_subst(dir, dos) {
        return none(true, false);
    }
    if crate::win_volume::resolve(dir).is_some_and(|r| r.to_string_lossy().starts_with(r"\\")) {
        return none(false, true);
    }
    let volume = crate::win_volume::volume_of(dir);
    let Some(v) = volume.clone() else { return BinFacts { volume, ..none(false, false) } };
    let root = wide(&format!("{}\\", v.label));
    // SAFETY: a NUL-terminated root path.
    let drive_type = unsafe { GetDriveTypeW(root.as_ptr()) };
    let guid = v.id.trim_start_matches(r"\\?\Volume").trim_end_matches('\\');
    let total_bytes = crate::win_volume::total_bytes(dir);
    let bin_used = bin_used(&format!("{}\\", v.label));
    BinFacts { subst: false, resolved_unc: false, volume, drive_type, total_bytes, bin_used, settings: read_settings(guid, reg_dword) }
}

/// [`decide`] for the real volume of `dir` (production [`BinCheck`]).
pub fn can_recycle(dir: &Path, bytes: u64) -> Result<(), String> {
    decide(&facts(dir), bytes)
}

#[cfg(test)]
#[path = "win_recycle_tests.rs"]
mod tests;
