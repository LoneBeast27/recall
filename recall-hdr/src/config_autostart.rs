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
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, REG_SZ,
    };

    let sub_w = wide_nul(RUN_SUBKEY);
    let name_w = wide_nul(RUN_VALUE_NAME);
    // REG_SZ payload is the UTF-16 string *including* its terminating NUL, as
    // raw bytes (len = u16 count * 2).
    let data_w = wide_nul(data);
    let data_bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(data_w.as_ptr() as *const u8, data_w.len() * 2)
    };

    unsafe {
        let mut key = HKEY::default();
        let st = RegCreateKeyW(HKEY_CURRENT_USER, PCWSTR(sub_w.as_ptr()), &mut key);
        if st.is_err() {
            return Err(format!("RegCreateKeyW(Run) failed: {st:?}"));
        }

        let set = RegSetValueExW(
            key,
            PCWSTR(name_w.as_ptr()),
            Some(0),
            REG_SZ,
            Some(data_bytes),
        );
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
