//! Can a file really go to the Recycle Bin? Recall never deletes a user clip
//! for good, so before a clip is sent to the Recycle Bin (the storage limit in
//! `recall-hdr`, the library's delete in `recall-native`, which both include
//! this file by path) its volume must have a Recycle Bin that will take it:
//!
//! - a local fixed volume with a volume GUID (a `\\nas` share, a mapped
//!   network drive or a removable drive has no Recycle Bin: Windows would
//!   delete for good, or ask whether to);
//! - the bin not turned off: neither the `NoRecycleFiles` policy (HKLM, else
//!   HKCU `Software\Microsoft\Windows\CurrentVersion\Policies\Explorer`) nor
//!   the volume's own `NukeOnDelete` (HKCU `...\Explorer\BitBucket\Volume\
//!   {GUID}`) is 1. All read only, never written;
//! - the clip and its companions no bigger than the bin's maximum size:
//!   the `RecycleBinSize` policy (percent of the volume; HKLM, else HKCU)
//!   when set, else the volume's `MaxCapacity` (MB), else Windows' default
//!   for a volume that has no `BitBucket\Volume` key yet ([`default_max_mb`]).
//!   A bigger item is the one Windows offers to delete permanently.
//!
//! A refusal stops the caller, which tells the user, and nothing is deleted.
//! This is the second guard: the call itself (`win_fileop.rs`) goes through
//! `IFileOperation` with a progress sink that refuses any delete Windows
//! would not recycle (a `subst` drive passes this precheck but not the sink,
//! measured 2026-11-07).

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
    pub volume: Option<Volume>,
    /// `GetDriveTypeW` of the volume's root.
    pub drive_type: u32,
    /// Volume size (`None`: unknown).
    pub total_bytes: Option<u64>,
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

/// May `bytes` from this volume go to its Recycle Bin?
pub fn decide(facts: &BinFacts, bytes: u64) -> Result<(), String> {
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
    Ok(())
}

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn GetDriveTypeW(root: *const u16) -> u32;
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
    let volume = crate::win_volume::volume_of(dir);
    let Some(v) = volume.clone() else { return BinFacts { volume, drive_type: 0, total_bytes: None, settings: Settings::default() } };
    let root = wide(&format!("{}\\", v.label));
    // SAFETY: a NUL-terminated root path.
    let drive_type = unsafe { GetDriveTypeW(root.as_ptr()) };
    let guid = v.id.trim_start_matches(r"\\?\Volume").trim_end_matches('\\');
    let total_bytes = crate::win_volume::total_bytes(dir);
    BinFacts { volume, drive_type, total_bytes, settings: read_settings(guid, reg_dword) }
}

/// [`decide`] for the real volume of `dir` (production [`BinCheck`]).
pub fn can_recycle(dir: &Path, bytes: u64) -> Result<(), String> {
    decide(&facts(dir), bytes)
}

#[cfg(test)]
#[path = "win_recycle_tests.rs"]
mod tests;
