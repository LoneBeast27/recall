//! The private desktop's access list. The desktop is created with an explicit
//! DACL holding exactly one ACE: this process token's USER SID (`TokenUser`)
//! with [`super::DESKTOP_ACCESS`] and nothing else, protected from
//! inheritance (`D:P`).
//!
//! Why the user SID and not the logon SID: `TokenUser` is in every token this
//! app can run with, while a logon SID can be missing (service and S4U
//! tokens). What the DACL does and does not stop: other users, services and
//! other sessions' accounts cannot open the desktop by name. Another process
//! of the SAME user carries the same SID (and, as the desktop's owner, its
//! implicit READ_CONTROL / WRITE_DAC rights), so no SID-based DACL can keep it
//! out; that is a Windows limit, stated here rather than claimed away.

use std::ffi::c_void;

#[link(name = "advapi32", kind = "raw-dylib")]
unsafe extern "system" {
    fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
    fn GetTokenInformation(token: *mut c_void, class: u32, info: *mut c_void, len: u32, needed: *mut u32) -> i32;
    fn ConvertSidToStringSidW(sid: *mut c_void, text: *mut *mut u16) -> i32;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(text: *const u16, revision: u32, sd: *mut *mut c_void, size: *mut u32) -> i32;
    fn ConvertSecurityDescriptorToStringSecurityDescriptorW(sd: *mut c_void, revision: u32, info: u32, text: *mut *mut u16, len: *mut u32) -> i32;
    fn GetSecurityInfo(handle: isize, kind: u32, info: u32, owner: *mut *mut c_void, group: *mut *mut c_void, dacl: *mut *mut c_void, sacl: *mut *mut c_void, sd: *mut *mut c_void) -> u32;
}

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn GetCurrentProcess() -> *mut c_void;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn LocalFree(mem: *mut c_void) -> *mut c_void;
}

const TOKEN_QUERY: u32 = 0x0008;
const TOKEN_USER: u32 = 1;
const SDDL_REVISION_1: u32 = 1;
const DACL_SECURITY_INFORMATION: u32 = 0x4;
const SE_WINDOW_OBJECT: u32 = 7;

fn from_local(text: *mut u16) -> String {
    let mut n = 0;
    // SAFETY: a NUL-terminated string the system allocated; read up to its NUL.
    unsafe {
        while *text.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(text, n))
    }
}

/// `S-1-5-21-…` of this process's token user (`None`: it cannot be read).
pub fn token_user_sid() -> Option<String> {
    let mut token: *mut c_void = std::ptr::null_mut();
    // SAFETY: the current-process pseudo handle; `token` is closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let mut buf = vec![0u64; 64]; // 512 bytes, 8-aligned: TOKEN_USER plus its SID
    let mut needed = 0u32;
    // SAFETY: `buf` is writable for the length given.
    let ok = unsafe { GetTokenInformation(token, TOKEN_USER, buf.as_mut_ptr().cast(), 512, &mut needed) };
    // SAFETY: opened above.
    unsafe { CloseHandle(token) };
    if ok == 0 {
        return None;
    }
    // TOKEN_USER { SID_AND_ATTRIBUTES { PSID Sid; DWORD Attributes } }: the first field is the SID pointer.
    let sid = buf[0] as usize as *mut c_void;
    let mut text: *mut u16 = std::ptr::null_mut();
    // SAFETY: a SID inside `buf`, alive for the call; the string is freed below.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return None;
    }
    let out = from_local(text);
    // SAFETY: allocated by ConvertSidToStringSidW.
    unsafe { LocalFree(text.cast()) };
    Some(out)
}

/// The SDDL for the private desktop: one ACE, this user, the desktop rights only.
pub fn desktop_sddl(sid: &str, rights: u32) -> String {
    format!("D:P(A;;{rights:#x};;;{sid})")
}

/// [`desktop_sddl`] for this process (`None`: the token user cannot be read).
pub fn own_sddl() -> Option<String> {
    token_user_sid().map(|sid| desktop_sddl(&sid, super::DESKTOP_ACCESS))
}

/// A self-relative security descriptor built from `sddl` (free with [`free_sd`]).
pub fn sd_from(sddl: &str) -> Option<*mut c_void> {
    let text: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
    let mut sd: *mut c_void = std::ptr::null_mut();
    // SAFETY: a NUL-terminated SDDL string; the descriptor is LocalFree'd by the caller.
    let ok = unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(text.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) };
    (ok != 0 && !sd.is_null()).then_some(sd)
}

pub fn free_sd(sd: *mut c_void) {
    // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
    unsafe { LocalFree(sd) };
}

/// The DACL of an open desktop handle as SDDL (read only; the handle needs READ_CONTROL).
pub fn desktop_dacl(desk: isize) -> Option<String> {
    let mut sd: *mut c_void = std::ptr::null_mut();
    let mut dacl: *mut c_void = std::ptr::null_mut();
    // SAFETY: out pointers are valid; the descriptor is freed below.
    let code = unsafe { GetSecurityInfo(desk, SE_WINDOW_OBJECT, DACL_SECURITY_INFORMATION, std::ptr::null_mut(), std::ptr::null_mut(), &mut dacl, std::ptr::null_mut(), &mut sd) };
    if code != 0 || sd.is_null() {
        return None;
    }
    let mut text: *mut u16 = std::ptr::null_mut();
    // SAFETY: a descriptor from GetSecurityInfo; the string is freed below.
    let ok = unsafe { ConvertSecurityDescriptorToStringSecurityDescriptorW(sd, SDDL_REVISION_1, DACL_SECURITY_INFORMATION, &mut text, std::ptr::null_mut()) };
    let out = (ok != 0).then(|| from_local(text));
    // SAFETY: both were allocated by the calls above.
    unsafe {
        if ok != 0 {
            LocalFree(text.cast());
        }
        LocalFree(sd);
    }
    out
}
