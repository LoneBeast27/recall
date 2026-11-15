//! Keeps other programs of the same user from injecting code into the library app the usual
//! way, so screen sharing uses Windows' own window capture.
//!
//! Discord shares an app window with a hook DLL it injects into the app (when it can) and only
//! falls back to Windows Graphics Capture when it cannot. In Recall the hook binds to the video
//! player's Direct3D 9 device (Media Foundation's EVR, in the child video window) instead of the
//! window, because the UI itself is a DirectComposition layer with no window surface of its own,
//! so the share showed nothing. Windows Graphics Capture records the whole window, UI and video.
//!
//! The process object's access list lets other processes of this user query, wait on and end
//! the app (Task Manager, Setup's update wait, the single-instance check), but not open it to
//! write its memory, start threads in it or copy its handles, which remote-thread injection
//! needs. This is not a security boundary: an elevated process with SeDebugPrivilege ignores the
//! list, and injection through SetWindowsHookEx or AppInit DLLs does not open the process at all.
//! SYSTEM keeps full access. `RECALL_ALLOW_INJECTION=1` turns it off.
use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    SE_KERNEL_OBJECT, SetSecurityInfo,
};
use windows::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetTokenInformation, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER, TokenUser, ACL,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{PCWSTR, PWSTR};

/// What another process of this user may still do: terminate (0x1), query information
/// (0x400), query limited information (0x1000), read the access list (0x20000), wait (0x100000).
pub const USER_RIGHTS: u32 = 0x0012_1401;
/// What every other account may do: query limited information and wait (the audio service's
/// per-app capture, Task Manager across sessions).
pub const OTHER_RIGHTS: u32 = 0x0010_1000;

pub fn enabled() -> bool {
    std::env::var_os("RECALL_ALLOW_INJECTION").is_none()
}

pub enum Outcome {
    Applied,
    /// `RECALL_ALLOW_INJECTION` is set.
    Off,
    /// The app runs on without the list.
    Failed(String),
}

/// Start-up entry: applies the list unless turned off.
pub fn start() -> Outcome {
    if !enabled() {
        return Outcome::Off;
    }
    match apply() {
        Ok(()) => Outcome::Applied,
        Err(error) => Outcome::Failed(error),
    }
}

/// The access list as SDDL: protected (no inherited entries), the user, SYSTEM, everyone.
pub fn sddl(user_sid: &str) -> String {
    format!("D:P(A;;0x{USER_RIGHTS:x};;;{user_sid})(A;;GA;;;SY)(A;;0x{OTHER_RIGHTS:x};;;WD)")
}

/// Applies the access list to this process. Errors are returned for the player log; the app
/// runs either way.
pub fn apply() -> Result<(), String> {
    let sid = user_sid()?;
    let text: Vec<u16> = sddl(&sid).encode_utf16().chain(Some(0)).collect();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(text.as_ptr()), SDDL_REVISION_1, &mut descriptor, None) }
        .map_err(|e| format!("parse access list: {e}"))?;
    let result = (|| {
        let (mut present, mut defaulted) = (false.into(), false.into());
        let mut dacl: *mut ACL = std::ptr::null_mut();
        unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted) }
            .map_err(|e| format!("read access list: {e}"))?;
        let info = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;
        let status = unsafe { SetSecurityInfo(GetCurrentProcess(), SE_KERNEL_OBJECT, info, None, None, Some(dacl), None) };
        if status.is_err() { Err(format!("set process access list: {}", status.0)) } else { Ok(()) }
    })();
    unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
    result
}

/// This process's user, as an SID string (S-1-5-21-…).
fn user_sid() -> Result<String, String> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.map_err(|e| format!("open token: {e}"))?;
    let result = (|| {
        let mut size = 0u32;
        let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut size) };
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        unsafe { GetTokenInformation(token, TokenUser, Some(buffer.as_mut_ptr().cast()), size, &mut size) }
            .map_err(|e| format!("read token user: {e}"))?;
        let user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };
        let mut text = PWSTR::null();
        unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) }.map_err(|e| format!("user sid: {e}"))?;
        let sid = unsafe { text.to_string() }.map_err(|e| format!("user sid text: {e}"));
        unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
        sid
    })();
    let _ = unsafe { CloseHandle(token) };
    result
}

#[cfg(test)]
#[path = "process_guard_tests.rs"]
mod tests;
