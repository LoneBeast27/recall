//! Autostart mirror: the `HKCU\...\Run` value that relaunches recall-hdr at
//! login (written by [`super::Config::save`] and the record mode's settings
//! path).

/// Registry subkey holding per-user run-at-login entries.
pub(super) const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// The value name we own under [`RUN_SUBKEY`].
pub(super) const RUN_VALUE_NAME: &str = "recall-hdr";

/// Run-key argument that relaunches the unified record mode at login.
pub const RECORD_RUN_ARG: &str = "--record";
/// Run-key argument that relaunches the legacy HDR-only tray at login.
pub const LEGACY_RUN_ARG: &str = "--legacy-hdr";

/// The exact `REG_SZ` data written under the Run key: the quoted exe path plus
/// the mode argument, e.g. `"C:\Recall\recall-hdr.exe" --record`. Pure so the
/// string is unit-tested without touching the registry.
pub fn run_value(exe: &str, arg: &str) -> String {
    if arg.is_empty() {
        format!("\"{exe}\"")
    } else {
        format!("\"{exe}\" {arg}")
    }
}

/// Mirror an autostart flag into `HKCU\...\Run`.
///
/// Idempotent:
/// - `enabled == true`  -> set `recall-hdr` = [`run_value`]`(current_exe, arg)`.
/// - `enabled == false` -> delete the value if present (absent is success).
///
/// Uses the `windows-rs` `Win32::System::Registry` surface
/// (`RegCreateKeyW` / `RegSetValueExW` / `RegDeleteValueW`). Errors are
/// returned as strings so the caller can log-and-continue.
pub fn sync_autostart(enabled: bool, arg: &str) -> Result<(), String> {
    if enabled {
        let exe = std::env::current_exe()
            .map_err(|e| format!("current_exe: {e}"))?
            .to_string_lossy()
            .into_owned();
        set_run_value(&run_value(&exe, arg))
    } else {
        delete_run_value()
    }
}

/// Encode a Rust string as a NUL-terminated UTF-16 buffer for the W registry API.
pub(super) fn wide_nul(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Create-or-open the Run key for write and set our value to `data` (REG_SZ).
///
/// Uses `RegCreateKeyW` (create-or-open, default security, all-access) rather
/// than `RegCreateKeyExW`: the Ex variant takes a `SECURITY_ATTRIBUTES` pointer
/// and is gated behind the `Win32_Security` cargo feature, which this crate's
/// pinned feature set does not enable. The `...\Run` key exists on every Windows
/// install, so this is effectively an open-for-write.
fn set_run_value(data: &str) -> Result<(), String> {
    // REG_SZ payload is the UTF-16 string *including* its terminating NUL, as
    // raw bytes (len = u16 count * 2).
    let data_w = wide_nul(data);
    let data_bytes: Vec<u8> = data_w.iter().flat_map(|u| u.to_le_bytes()).collect();
    set_value(RUN_SUBKEY, windows::Win32::System::Registry::REG_SZ, &data_bytes)
}

/// Create-or-open `subkey` (HKCU) for write and set our value to `bytes` of type `kind`.
fn set_value(subkey: &str, kind: windows::Win32::System::Registry::REG_VALUE_TYPE, bytes: &[u8]) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{RegCloseKey, RegCreateKeyW, RegSetValueExW, HKEY, HKEY_CURRENT_USER};

    let sub_w = wide_nul(subkey);
    let name_w = wide_nul(RUN_VALUE_NAME);
    unsafe {
        let mut key = HKEY::default();
        let st = RegCreateKeyW(HKEY_CURRENT_USER, PCWSTR(sub_w.as_ptr()), &mut key);
        if st.is_err() {
            return Err(format!("RegCreateKeyW({subkey}) failed: {st:?}"));
        }
        let set = RegSetValueExW(key, PCWSTR(name_w.as_ptr()), Some(0), kind, Some(bytes));
        let _ = RegCloseKey(key);
        if set.is_err() {
            return Err(format!("RegSetValueExW({RUN_VALUE_NAME}) failed: {set:?}"));
        }
    }
    Ok(())
}

/// Delete our Run value. A missing value (ERROR_FILE_NOT_FOUND) is success.
fn delete_run_value() -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_WRITE,
    };

    let sub_w = wide_nul(RUN_SUBKEY);
    let name_w = wide_nul(RUN_VALUE_NAME);

    unsafe {
        let mut key = HKEY::default();
        let open = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(sub_w.as_ptr()),
            Some(0),
            KEY_WRITE,
            &mut key,
        );
        if open.is_err() {
            // No Run key at all -> nothing to delete -> success.
            if open == ERROR_FILE_NOT_FOUND {
                return Ok(());
            }
            return Err(format!("RegOpenKeyExW(Run) failed: {open:?}"));
        }

        let del = RegDeleteValueW(key, PCWSTR(name_w.as_ptr()));
        let _ = RegCloseKey(key);

        if del.is_err() && del != ERROR_FILE_NOT_FOUND {
            return Err(format!("RegDeleteValueW({RUN_VALUE_NAME}) failed: {del:?}"));
        }
    }
    Ok(())
}

/// Windows' own on/off switch for every Run entry (Task Manager > Startup apps, Settings >
/// Apps > Startup). It sits beside the Run key, so a Run value can be present while Windows
/// keeps it from starting.
pub(super) const APPROVED_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

/// The 12-byte record Task Manager writes when it turns an entry on: flag 2, no time.
const APPROVED_ON: [u8; 12] = [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// A StartupApproved record says "off" when its first byte is odd (Windows writes 3 to turn
/// an entry off, 2 or 6 to turn it on). No record means on.
pub fn approved_says_off(record: Option<&[u8]>) -> bool {
    record.and_then(|r| r.first()).is_some_and(|b| b & 1 == 1)
}

/// The Run value must be (re)written: the setting is on and the value is missing or points
/// somewhere else (an old install folder, a hand edit). Paths compare without case, like Windows.
pub fn heal_needed(enabled: bool, current: Option<&str>, want: &str) -> bool {
    enabled && current.is_none_or(|c| !c.trim().eq_ignore_ascii_case(want))
}

/// Only an installed copy may point the Run key at itself: Setup leaves
/// `install-manifest.json` beside the exe, a dev or test build has none. A test instance
/// (`RECALL_INSTANCE_TAG`) never touches it either.
pub fn may_heal_from(exe_dir: &std::path::Path, instance_tag: Option<&str>) -> bool {
    instance_tag.is_none_or(str::is_empty) && exe_dir.join("install-manifest.json").is_file()
}

/// What [`heal_autostart`] did at start.
#[derive(Debug, PartialEq, Eq)]
pub enum Heal {
    /// The setting is off, or the Run value already matches.
    Nothing,
    /// Not an installed copy: left alone.
    NotInstalled,
    /// The Run value was missing (`None`) or stale (`Some(old)`), and is now right.
    Restored(Option<String>),
}

/// At start: with the setting on, put back a missing or stale Run value (the setting is the
/// truth). Never turns autostart on, never removes anything, never touches Windows' own
/// on/off switch (that stays the user's; [`windows_turned_off`] reports it).
pub fn heal_autostart(enabled: bool, arg: &str) -> Result<Heal, String> {
    if !enabled {
        return Ok(Heal::Nothing);
    }
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let tag = std::env::var("RECALL_INSTANCE_TAG").ok();
    if !exe.parent().is_some_and(|d| may_heal_from(d, tag.as_deref())) {
        return Ok(Heal::NotInstalled);
    }
    let want = run_value(&exe.to_string_lossy(), arg);
    let current = read_run_value();
    if !heal_needed(enabled, current.as_deref(), &want) {
        return Ok(Heal::Nothing);
    }
    set_run_value(&want)?;
    Ok(Heal::Restored(current))
}

/// Windows has our Run entry turned off in Startup apps.
pub fn windows_turned_off() -> bool {
    approved_says_off(read_value(APPROVED_SUBKEY, windows::Win32::System::Registry::RRF_RT_REG_BINARY).as_deref())
}

/// The user turned Start with Windows on in Recall: clear Windows' "off" record too, as
/// Task Manager's Enable does, or the switch reads on while nothing starts. An absent or
/// "on" record is left as it is.
pub fn clear_windows_off() -> Result<bool, String> {
    if !windows_turned_off() {
        return Ok(false);
    }
    set_value(APPROVED_SUBKEY, windows::Win32::System::Registry::REG_BINARY, &APPROVED_ON)?;
    Ok(true)
}

/// Our Run value's text, if any.
fn read_run_value() -> Option<String> {
    let bytes = read_value(RUN_SUBKEY, windows::Win32::System::Registry::RRF_RT_REG_SZ)?;
    let wide: Vec<u16> = bytes.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect();
    let text = String::from_utf16_lossy(&wide);
    Some(text.trim_end_matches('\0').to_owned())
}

/// Read our value under `subkey` (HKCU) with the given type filter; `None` when absent.
fn read_value(subkey: &str, flags: windows::Win32::System::Registry::REG_ROUTINE_FLAGS) -> Option<Vec<u8>> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER};

    let sub_w = wide_nul(subkey);
    let name_w = wide_nul(RUN_VALUE_NAME);
    unsafe {
        let mut size: u32 = 0;
        let st = RegGetValueW(HKEY_CURRENT_USER, PCWSTR(sub_w.as_ptr()), PCWSTR(name_w.as_ptr()), flags, None, None, Some(&mut size));
        if st.is_err() || size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let st = RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(sub_w.as_ptr()),
            PCWSTR(name_w.as_ptr()),
            flags,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        );
        if st.is_err() {
            return None;
        }
        buf.truncate(size as usize);
        Some(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_off_record_is_odd_first_byte() {
        assert!(approved_says_off(Some(&[3, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8])));
        assert!(!approved_says_off(Some(&APPROVED_ON)));
        assert!(!approved_says_off(Some(&[6, 0, 0, 0])));
        assert!(!approved_says_off(None), "no record = Windows never turned it off");
        assert!(!approved_says_off(Some(&[])));
    }

    #[test]
    fn heal_only_when_on_and_missing_or_different() {
        let want = run_value(r"C:\Apps\Recall\recall-hdr.exe", RECORD_RUN_ARG);
        assert!(heal_needed(true, None, &want), "missing value is put back");
        assert!(heal_needed(true, Some(r#""D:\Old\recall-hdr.exe" --record"#), &want), "stale folder is fixed");
        assert!(heal_needed(true, Some(r#""C:\Apps\Recall\recall-hdr.exe" --legacy-hdr"#), &want), "old mode arg is fixed");
        assert!(!heal_needed(true, Some(&want), &want));
        assert!(!heal_needed(true, Some(&want.to_uppercase()), &want), "case alone is not stale");
        assert!(!heal_needed(false, None, &want), "setting off: never written (default stays off)");
        assert!(!heal_needed(false, Some(r#""D:\Old\recall-hdr.exe""#), &want), "setting off: never touched");
    }

    #[test]
    fn only_an_installed_non_test_copy_heals() {
        let dir = std::env::temp_dir().join(format!("recall-autostart-heal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!may_heal_from(&dir, None), "a dev build has no install manifest");
        std::fs::write(dir.join("install-manifest.json"), "{}").unwrap();
        assert!(may_heal_from(&dir, None));
        assert!(may_heal_from(&dir, Some("")));
        assert!(!may_heal_from(&dir, Some("gate-1")), "a test instance never touches the Run key");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn heal_with_the_setting_off_does_nothing() {
        assert_eq!(heal_autostart(false, RECORD_RUN_ARG), Ok(Heal::Nothing));
    }

    #[test]
    fn heal_from_a_test_build_leaves_the_registry_alone() {
        // The test exe sits in target\, with no install manifest beside it.
        assert_eq!(heal_autostart(true, RECORD_RUN_ARG), Ok(Heal::NotInstalled));
    }
}
