//! The control pipe's name and access (audit G3). The single-instance mutexes are per
//! session (`Local\`), so with two people signed in (fast user switching) each runs a
//! controller; a machine-wide pipe name let only the first create it, and its default
//! security gave read access to every local account. The name now carries the Windows
//! session id, and the pipe allows only this user and SYSTEM. The same user's elevated
//! and non-elevated processes share the user SID, so both reach it.
//!
//! Clients build the same name: recall-native (`services/daemon/session.rs`), the island
//! (`recall-overlay/src/pipe_name.rs`), `scripts/install.ps1` and `Start-Recall.ps1`.

use interprocess::os::windows::security_descriptor::SecurityDescriptor;

/// The name's fixed part. A session whose id can't be read uses it bare (every client
/// falls back the same way).
pub const PIPE_BASE: &str = "recall-hdr-control";

/// The pipe name for one Windows session: `recall-hdr-control-<session>`.
pub fn pipe_name(session: Option<u32>) -> String {
    match session {
        Some(id) => format!("{PIPE_BASE}-{id}"),
        None => PIPE_BASE.to_string(),
    }
}

/// This process's Windows session id.
pub fn current_session() -> Option<u32> {
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    let mut session = 0u32;
    // SAFETY: plain out-parameter call on our own pid.
    unsafe { ProcessIdToSessionId(std::process::id(), &mut session) }.ok()?;
    Some(session)
}

/// The pipe name for this session.
pub fn current_pipe_name() -> String {
    pipe_name(current_session())
}

/// A protected DACL that grants full access to SYSTEM and to one user SID, nobody else
/// (no Everyone read, no other account, no inheritance from the default).
pub fn owner_only_sddl(user_sid: &str) -> String {
    format!("D:P(A;;GA;;;SY)(A;;GA;;;{user_sid})")
}

/// The user SID of this process's token, as `S-1-5-21-...`.
pub fn current_user_sid() -> Result<String, String> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL};
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
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
    // SAFETY: GetTokenInformation(TokenUser) filled `buf` with a TOKEN_USER.
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    let mut text = PWSTR::null();
    // SAFETY: the SID points into `buf`, alive for the call; `text` is freed with LocalFree.
    unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) }.map_err(|e| format!("format SID: {e}"))?;
    // SAFETY: a NUL-terminated string from ConvertSidToStringSidW.
    let sid = unsafe { text.to_string() }.map_err(|e| format!("SID text: {e}"));
    // SAFETY: allocated by ConvertSidToStringSidW.
    let _ = unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
    sid
}

/// The listener's security descriptor: this user and SYSTEM only.
pub fn owner_only_descriptor() -> Result<SecurityDescriptor, String> {
    let sddl = owner_only_sddl(&current_user_sid()?);
    let wide = widestring::U16CString::from_str(&sddl).map_err(|e| format!("SDDL text: {e}"))?;
    SecurityDescriptor::deserialize(&wide).map_err(|e| format!("SDDL {sddl}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_name_differs_per_session() {
        assert_eq!(pipe_name(Some(1)), "recall-hdr-control-1");
        assert_eq!(pipe_name(Some(2)), "recall-hdr-control-2");
        assert_ne!(pipe_name(Some(1)), pipe_name(Some(2)));
        assert_eq!(pipe_name(None), "recall-hdr-control", "no session id: every client falls back to the bare name");
        assert_eq!(current_pipe_name(), pipe_name(current_session()));
        assert!(current_session().is_some(), "ProcessIdToSessionId works on our own pid");
    }

    #[test]
    fn the_pipe_dacl_names_only_this_user_and_system() {
        let sid = current_user_sid().expect("token user SID");
        assert!(sid.starts_with("S-1-"), "{sid}");
        let sddl = owner_only_sddl(&sid);
        assert_eq!(sddl, format!("D:P(A;;GA;;;SY)(A;;GA;;;{sid})"));
        assert!(!sddl.contains(";WD)") && !sddl.contains(";AU)") && !sddl.contains(";BA)"), "no Everyone / Authenticated Users / Administrators");
        owner_only_descriptor().expect("the SDDL parses into a descriptor");
    }
}
