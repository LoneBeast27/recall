//! The control pipe's server must run as this account (deep-startup-p3 a). The pipe name is
//! per session and the controller's DACL admits only its own user, but a name can be taken
//! first: another account's process that creates `recall-hdr-control-<session>` before our
//! recorder would get this app's commands and could feed it a fake status. After each connect
//! the client asks Windows for the pipe's server process (GetNamedPipeServerProcessId) and
//! compares that process's token user SID with its own; another (or unreadable) account is
//! refused and logged; the island stays disconnected (no notices from a stranger).
//!
//! The same file is `recall-native/src/services/daemon/pipe_owner.rs` (the app): keep the two
//! in step.

use interprocess::local_socket::traits::StreamCommon as _;
use interprocess::local_socket::Stream;
use windows::core::PWSTR;
use windows::Win32::Security::PSID;

/// The refusal, as logged and shown.
pub const FOREIGN: &str = "another user owns the Recall pipe";

/// `WTS_ANY_SESSION`: every session's processes.
const WTS_ANY_SESSION: u32 = 0xFFFF_FFFE;

/// Ok when the pipe's server (`server`: its pid) runs as `own`. `sid_of` reads a pid's user SID
/// (tests inject it).
pub fn verdict(server: Result<u32, String>, own: Result<String, String>, sid_of: impl FnOnce(u32) -> Result<String, String>) -> Result<(), String> {
    let pid = server.map_err(|e| format!("the Recall pipe's server can't be identified ({e})"))?;
    let own = own.map_err(|e| format!("this account's SID can't be read ({e})"))?;
    match sid_of(pid) {
        Ok(sid) if sid.eq_ignore_ascii_case(&own) => Ok(()),
        Ok(sid) => Err(format!("{FOREIGN} (server pid {pid} runs as {sid})")),
        // No query rights on it: most likely another account's process.
        Err(e) => Err(format!("{FOREIGN} (server pid {pid}, its account can't be read: {e})")),
    }
}

/// The server process of a connected client stream (GetNamedPipeServerProcessId).
pub fn server_pid(stream: &Stream) -> Result<u32, String> {
    let creds = stream.peer_creds().map_err(|e| e.to_string())?;
    creds.pid().ok_or_else(|| "no server pid".to_string())
}

/// The live check for a fresh connection.
pub fn check(stream: &Stream) -> Result<(), String> {
    verdict(server_pid(stream), process_user_sid(std::process::id()), process_user_sid)
}

/// A process's user SID as `S-1-5-...`: from its token, else from the Terminal Services
/// process list (which also names this user's elevated processes, whose token a
/// non-elevated client may not open).
pub fn process_user_sid(pid: u32) -> Result<String, String> {
    token_sid(pid).or_else(|first| wts_sid(pid).map_err(|second| format!("{first}; {second}")))
}

fn token_sid(pid: u32) -> Result<String, String> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows::Win32::System::Threading::{OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};

    // SAFETY: plain call; the handle is closed below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.map_err(|e| format!("open process: {e}"))?;
    let mut token = HANDLE::default();
    // SAFETY: `process` is open; the token handle is closed below.
    let opened = unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) };
    // SAFETY: opened above.
    let _ = unsafe { CloseHandle(process) };
    opened.map_err(|e| format!("open token: {e}"))?;
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
    // SAFETY: GetTokenInformation(TokenUser) filled `buf` with a TOKEN_USER; its SID points into `buf`.
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    sid_text(user.User.Sid)
}

fn wts_sid(pid: u32) -> Result<String, String> {
    use windows::Win32::System::RemoteDesktop::{WTSEnumerateProcessesExW, WTSFreeMemoryExW, WTSTypeProcessInfoLevel1, WTS_PROCESS_INFO_EXW};

    let mut level: u32;
    let mut list = PWSTR::null();
    let mut count = 0u32;
    // The list can change size while Windows builds it (ERROR_BAD_LENGTH, 0x80070018, seen under
    // heavy process churn): ask again a few times before giving up.
    let mut tries = 0;
    loop {
        level = 1;
        // SAFETY: out-parameters; the list is freed with WTSFreeMemoryExW below.
        match unsafe { WTSEnumerateProcessesExW(None, &mut level, WTS_ANY_SESSION, &mut list, &mut count) } {
            Ok(()) => break,
            Err(e) if e.code() == windows::Win32::Foundation::ERROR_BAD_LENGTH.to_hresult() && tries < 4 => tries += 1,
            Err(e) => return Err(format!("process list: {e}")),
        }
    }
    // SAFETY: level 1 returns `count` WTS_PROCESS_INFO_EXW rows at `list`.
    let rows = unsafe { std::slice::from_raw_parts(list.0 as *const WTS_PROCESS_INFO_EXW, count as usize) };
    let sid = match rows.iter().find(|row| row.ProcessId == pid) {
        Some(row) if !row.pUserSid.0.is_null() => sid_text(row.pUserSid),
        Some(_) => Err(format!("pid {pid} has no user SID")),
        None => Err(format!("pid {pid} not listed")),
    };
    // SAFETY: allocated by WTSEnumerateProcessesExW at this level and count.
    let _ = unsafe { WTSFreeMemoryExW(WTSTypeProcessInfoLevel1, list.0 as *const _, count) };
    sid
}

fn sid_text(sid: PSID) -> Result<String, String> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;

    let mut text = PWSTR::null();
    // SAFETY: `sid` is valid for the call; `text` is freed with LocalFree.
    unsafe { ConvertSidToStringSidW(sid, &mut text) }.map_err(|e| format!("format SID: {e}"))?;
    // SAFETY: a NUL-terminated string from ConvertSidToStringSidW.
    let out = unsafe { text.to_string() }.map_err(|e| format!("SID text: {e}"));
    // SAFETY: allocated by ConvertSidToStringSidW.
    let _ = unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pipe_served_by_another_account_is_refused() {
        let me = || Ok("S-1-5-21-1-2-3-1001".to_string());
        assert_eq!(verdict(Ok(40), me(), |_| Ok("s-1-5-21-1-2-3-1001".into())), Ok(()), "same SID (case aside)");
        let other = verdict(Ok(40), me(), |pid| {
            assert_eq!(pid, 40, "the server's pid is looked up");
            Ok("S-1-5-21-9-9-9-1002".into())
        });
        assert!(other.as_ref().is_err_and(|e| e.starts_with(FOREIGN) && e.contains("pid 40")), "{other:?}");
        let unreadable = verdict(Ok(40), me(), |_| Err("access denied".into()));
        assert!(unreadable.as_ref().is_err_and(|e| e.starts_with(FOREIGN)), "{unreadable:?}");
        assert!(verdict(Err("gone".into()), me(), |_| unreachable!()).is_err(), "no server pid: refused");
        assert!(verdict(Ok(40), Err("no token".into()), |_| unreachable!()).is_err(), "own SID unknown: refused");
    }

    #[test]
    fn a_real_pipe_of_this_account_passes_and_a_faked_other_account_does_not() {
        use interprocess::local_socket::traits::{Listener as _, Stream as _};
        use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName};

        let pipe = format!("recall-overlay-owner-test-{}", std::process::id());
        let name = || pipe.as_str().to_ns_name::<GenericNamespaced>().unwrap();
        let listener = ListenerOptions::new().name(name()).create_sync().unwrap();
        // The first instance exists once the listener does: connect, then accept it here.
        let stream = Stream::connect(name()).expect("connect");
        let _served = listener.accept().expect("accept");
        assert_eq!(server_pid(&stream), Ok(std::process::id()), "this process serves it");
        assert_eq!(check(&stream), Ok(()), "same account");
        let faked = verdict(server_pid(&stream), Ok("S-1-5-21-1-1-1-1001".into()), |_| Ok("S-1-5-21-2-2-2-1002".into()));
        assert!(faked.is_err_and(|e| e.starts_with(FOREIGN)));
    }

    #[test]
    fn this_process_reads_its_own_user_sid() {
        let own = process_user_sid(std::process::id()).expect("own SID");
        assert!(own.starts_with("S-1-5-"), "{own}");
        assert_eq!(wts_sid(std::process::id()).as_deref(), Ok(own.as_str()), "the fallback agrees");
    }
}
