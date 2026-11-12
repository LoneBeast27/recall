//! Win32 calls for `fps_setup`: the account's SID, its Performance Log Users membership, and the
//! elevated start of the installed Setup.
use std::path::Path;

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSidToSidW};
use windows::Win32::Security::PSID;
use windows::core::{HSTRING, PCWSTR, PWSTR};

/// Performance Log Users, a well-known local group (its name is translated on non-English Windows).
const GROUP_SID: &str = "S-1-5-32-559";

/// `runas` through the shell (Windows' own prompt, owned by `owner`), then wait for the elevated
/// Setup's exit code. Err is the Win32 error of the start (1223: declined).
pub(super) fn run_elevated(setup: &Path, params: &str, owner: isize) -> Result<u32, i32> {
    use windows::Win32::Foundation::{CloseHandle, GetLastError, HWND};
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::w;
    let file = HSTRING::from(setup.as_os_str());
    let params = HSTRING::from(params);
    // SAFETY: the strings outlive the call; the process handle is closed below.
    unsafe {
        // ShellExecuteEx wants COM on the calling thread (this thread is the click's own).
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS;
        info.hwnd = HWND(owner as *mut _);
        info.lpVerb = w!("runas");
        info.lpFile = PCWSTR(file.as_ptr());
        info.lpParameters = PCWSTR(params.as_ptr());
        info.nShow = SW_SHOWNORMAL.0;
        if ShellExecuteExW(&mut info).is_err() {
            return Err(GetLastError().0 as i32);
        }
        if info.hProcess.is_invalid() {
            return Err(-1);
        }
        // Setup's elevated mode only calls NetLocalGroupAddMembers; 5 minutes is a hang.
        WaitForSingleObject(info.hProcess, 5 * 60 * 1000);
        let mut code = 0u32;
        let got = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        got.map(|_| code).map_err(|_| -1)
    }
}

/// A SID as `S-1-...`.
fn sid_text(sid: PSID) -> Result<String, String> {
    let mut text = PWSTR::null();
    // SAFETY: `sid` is valid for the call; `text` is freed with LocalFree.
    unsafe { ConvertSidToStringSidW(sid, &mut text) }.map_err(|e| format!("format SID: {e}"))?;
    // SAFETY: a NUL-terminated string from ConvertSidToStringSidW.
    let out = unsafe { text.to_string() }.map_err(|e| format!("SID text: {e}"));
    // SAFETY: allocated by ConvertSidToStringSidW.
    let _ = unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
    out
}

/// The user SID of this process's token, as `S-1-5-21-...` or `S-1-12-1-...`.
pub(super) fn current_user_sid() -> Result<String, String> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut token = HANDLE::default();
    // SAFETY: our own process handle; the token handle is closed below.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.map_err(|e| format!("open token: {e}"))?;
    let mut needed = 0u32;
    // SAFETY: size query (fails with ERROR_INSUFFICIENT_BUFFER and sets `needed`).
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut needed) };
    // u64 storage keeps the TOKEN_USER (pointer-aligned) aligned.
    let mut buf = vec![0u64; (needed as usize).div_ceil(8).max(1)];
    // SAFETY: `buf` holds `needed` bytes.
    let got = unsafe { GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), needed, &mut needed) };
    // SAFETY: opened above.
    let _ = unsafe { CloseHandle(token) };
    got.map_err(|e| format!("read token user: {e}"))?;
    // SAFETY: GetTokenInformation(TokenUser) filled `buf` with a TOKEN_USER; its SID lives in `buf`.
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    sid_text(user.User.Sid)
}

/// The group's local name, from its well-known SID, NUL-terminated.
fn group_name() -> Result<Vec<u16>, String> {
    use windows::Win32::Security::{LookupAccountSidW, SID_NAME_USE};
    let mut sid = PSID::default();
    // SAFETY: freed with LocalFree below.
    unsafe { ConvertStringSidToSidW(&HSTRING::from(GROUP_SID), &mut sid) }.map_err(|e| format!("group SID: {e}"))?;
    let (mut name, mut domain) = (vec![0u16; 256], vec![0u16; 256]);
    let (mut name_len, mut domain_len) = (name.len() as u32, domain.len() as u32);
    let mut kind = SID_NAME_USE::default();
    // SAFETY: both buffers hold the lengths given.
    let got = unsafe {
        LookupAccountSidW(PCWSTR::null(), sid, Some(PWSTR(name.as_mut_ptr())), &mut name_len, Some(PWSTR(domain.as_mut_ptr())), &mut domain_len, &mut kind)
    };
    // SAFETY: allocated by ConvertStringSidToSidW.
    let _ = unsafe { LocalFree(Some(HLOCAL(sid.0))) };
    got.map_err(|e| format!("group name: {e}"))?;
    name.truncate(name_len as usize);
    name.push(0);
    Ok(name)
}

/// `user_sid` is a direct member of Performance Log Users (what Setup's elevated copy adds),
/// whether or not this sign-in's token has it yet.
pub(super) fn is_member(user_sid: &str) -> Result<bool, String> {
    use windows::Win32::NetworkManagement::NetManagement::{LOCALGROUP_MEMBERS_INFO_0, MAX_PREFERRED_LENGTH, NetApiBufferFree, NetLocalGroupGetMembers};
    let group = group_name()?;
    let mut buffer: *mut u8 = std::ptr::null_mut();
    let (mut read, mut total) = (0u32, 0u32);
    // SAFETY: the name is NUL-terminated; the buffer is freed with NetApiBufferFree.
    let status = unsafe { NetLocalGroupGetMembers(PCWSTR::null(), PCWSTR(group.as_ptr()), 0, &mut buffer, MAX_PREFERRED_LENGTH, &mut read, &mut total, None) };
    if status != 0 {
        return Err(format!("group members: error {status}"));
    }
    if buffer.is_null() {
        return Ok(false);
    }
    // SAFETY: NetLocalGroupGetMembers (level 0) returned `read` LOCALGROUP_MEMBERS_INFO_0 entries.
    let members = unsafe { std::slice::from_raw_parts(buffer as *const LOCALGROUP_MEMBERS_INFO_0, read as usize) };
    let found = members.iter().any(|m| !m.lgrmi0_sid.0.is_null() && sid_text(m.lgrmi0_sid).is_ok_and(|s| s == user_sid));
    // SAFETY: allocated by NetLocalGroupGetMembers.
    let _ = unsafe { NetApiBufferFree(Some(buffer as *const _)) };
    Ok(found)
}
