//! Can a file really go to the Recycle Bin? Recall never deletes a user clip
//! for good, so before a clip is sent to the Recycle Bin (the storage limit in
//! `recall-hdr`, the library's delete in `recall-native`, which both include
//! this file by path) its volume must have a Recycle Bin that will take it:
//!
//! - a local fixed volume with a volume GUID (a `\\nas` share, a mapped
//!   network drive or a removable drive has no Recycle Bin: Windows would
//!   delete for good, or ask whether to);
//! - its Recycle Bin settings present and on: `HKCU\Software\Microsoft\
//!   Windows\CurrentVersion\Explorer\BitBucket\Volume\{GUID}` with
//!   `NukeOnDelete` not 1 (read only, never written). A volume without that
//!   key counts as unknown, so nothing moves;
//! - the clip and its companions no bigger than that volume's `MaxCapacity`
//!   (MB). A bigger item is the one Windows offers to delete permanently.
//!
//! A refusal stops the caller, which tells the user, and nothing is deleted.
//! The shell call itself uses [`RECYCLE_FLAGS`]: `FOF_ALLOWUNDO` always, and
//! `FOF_NOCONFIRMATION` (no "move to the Recycle Bin?" box) only together
//! with `FOF_WANTNUKEWARNING`. If this check were ever wrong, Windows would
//! then ask before a permanent delete instead of doing one silently.

use std::path::Path;

use crate::win_volume::Volume;

/// `FOF_SILENT`: no progress window.
pub const FOF_SILENT: u16 = 0x0004;
/// Auto-answers "yes" to confirmations. Safe only with `FOF_WANTNUKEWARNING`.
pub const FOF_NOCONFIRMATION: u16 = 0x0010;
/// Recycle Bin instead of delete.
pub const FOF_ALLOWUNDO: u16 = 0x0040;
pub const FOF_NOERRORUI: u16 = 0x0400;
/// Ask before a delete that cannot go to the Recycle Bin, even with `FOF_NOCONFIRMATION`.
pub const FOF_WANTNUKEWARNING: u16 = 0x4000;
/// The one flag set Recall passes to `SHFileOperationW(FO_DELETE)`.
pub const RECYCLE_FLAGS: u16 = FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI | FOF_WANTNUKEWARNING;

/// A flag set that can never delete for good without asking: it recycles,
/// and it does not auto-confirm a permanent delete.
pub fn recycles_only(flags: u16) -> bool {
    flags & FOF_ALLOWUNDO != 0 && (flags & FOF_NOCONFIRMATION == 0 || flags & FOF_WANTNUKEWARNING != 0)
}

const DRIVE_FIXED: u32 = 3;

/// What Windows says about one volume's Recycle Bin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinFacts {
    pub volume: Option<Volume>,
    /// `GetDriveTypeW` of the volume's root.
    pub drive_type: u32,
    /// `NukeOnDelete` (`None`: the value is missing).
    pub nuke: Option<u32>,
    /// `MaxCapacity` in MB (`None`: missing).
    pub max_mb: Option<u32>,
}

/// `fn(folder, bytes) -> Ok, or why these bytes cannot go to its Recycle Bin`.
pub type BinCheck = fn(&Path, u64) -> Result<(), String>;

/// May `bytes` from this volume go to its Recycle Bin?
pub fn decide(facts: &BinFacts, bytes: u64) -> Result<(), String> {
    let Some(volume) = facts.volume.as_ref().filter(|v| v.id.starts_with(r"\\?\Volume{")) else {
        return Err("the drive has no Recycle Bin (a network share or an unknown drive)".into());
    };
    if facts.drive_type != DRIVE_FIXED {
        return Err(format!("{} has no Recycle Bin (a removable or network drive)", volume.label));
    }
    let (Some(nuke), Some(max_mb)) = (facts.nuke, facts.max_mb) else {
        return Err(format!("the Recycle Bin settings for {} are unknown", volume.label));
    };
    if nuke == 1 {
        return Err(format!("the Recycle Bin is turned off for {} (files are deleted right away)", volume.label));
    }
    let max = u64::from(max_mb) << 20;
    if bytes > max {
        return Err(format!("the clip is bigger than the Recycle Bin on {} ({} MB)", volume.label, max_mb));
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

/// One DWORD under `HKCU\<sub_key>` (read only).
fn reg_dword(sub_key: &str, value: &str) -> Option<u32> {
    const HKEY_CURRENT_USER: isize = 0x8000_0001_u32 as i32 as isize;
    const RRF_RT_REG_DWORD: u32 = 0x10;
    let (key, name) = (wide(sub_key), wide(value));
    let (mut data, mut size) = (0u32, 4u32);
    // SAFETY: NUL-terminated strings; `data` holds the 4 bytes `size` says.
    let code = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), &mut data, &mut size)
    };
    (code == 0).then_some(data)
}

/// The facts for the volume `dir` really lives on (read only).
pub fn facts(dir: &Path) -> BinFacts {
    let volume = crate::win_volume::volume_of(dir);
    let Some(v) = volume.clone() else { return BinFacts { volume, drive_type: 0, nuke: None, max_mb: None } };
    let root = wide(&format!("{}\\", v.label));
    // SAFETY: a NUL-terminated root path.
    let drive_type = unsafe { GetDriveTypeW(root.as_ptr()) };
    let guid = v.id.trim_start_matches(r"\\?\Volume").trim_end_matches('\\');
    let key = format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume\{guid}");
    BinFacts { volume, drive_type, nuke: reg_dword(&key, "NukeOnDelete"), max_mb: reg_dword(&key, "MaxCapacity") }
}

/// [`decide`] for the real volume of `dir` (production [`BinCheck`]).
pub fn can_recycle(dir: &Path, bytes: u64) -> Result<(), String> {
    decide(&facts(dir), bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(id: &str, label: &str) -> Option<Volume> {
        Some(Volume { id: id.into(), label: label.into() })
    }

    fn local() -> BinFacts {
        BinFacts { volume: volume(r"\\?\Volume{6b9a6f8e-84ea-42bb-8615-8aea7bdcfa86}\", "C:"), drive_type: DRIVE_FIXED, nuke: Some(0), max_mb: Some(1000) }
    }

    #[test]
    fn only_a_local_enabled_bin_big_enough_takes_a_clip() {
        assert_eq!(decide(&local(), 1000 << 20), Ok(()));
        assert!(decide(&local(), (1000 << 20) + 1).unwrap_err().contains("bigger than the Recycle Bin"));
        let share = BinFacts { volume: volume(r"\\nas\clips\", r"\\nas\clips"), ..local() };
        assert!(decide(&share, 1).unwrap_err().contains("no Recycle Bin"), "a share is never recycled into");
        assert!(decide(&BinFacts { volume: None, ..local() }, 1).is_err(), "unknown drive");
        assert!(decide(&BinFacts { drive_type: 2, ..local() }, 1).unwrap_err().contains("removable"));
        assert!(decide(&BinFacts { nuke: Some(1), ..local() }, 1).unwrap_err().contains("turned off"));
        assert!(decide(&BinFacts { max_mb: None, ..local() }, 1).unwrap_err().contains("unknown"));
        assert!(decide(&BinFacts { nuke: None, ..local() }, 1).is_err());
    }

    #[test]
    fn the_recycle_flags_never_delete_for_good_unasked() {
        assert!(recycles_only(RECYCLE_FLAGS));
        assert!(!recycles_only(FOF_NOCONFIRMATION | FOF_SILENT), "no FOF_ALLOWUNDO: a plain delete");
        assert!(!recycles_only(FOF_ALLOWUNDO | FOF_NOCONFIRMATION), "auto-yes to a permanent delete");
        assert!(recycles_only(FOF_ALLOWUNDO | FOF_SILENT));
    }

    #[test]
    fn this_machine_reports_its_temp_volume_bin() {
        // Read only: the temp folder's volume is local and fixed with a bin; the answer is printed for the record.
        let f = facts(&std::env::temp_dir());
        println!("temp volume bin facts: {f:?}");
        assert_eq!(f.drive_type, DRIVE_FIXED, "{f:?}");
        assert!(f.volume.is_some_and(|v| v.id.starts_with(r"\\?\Volume{")));
    }
}
