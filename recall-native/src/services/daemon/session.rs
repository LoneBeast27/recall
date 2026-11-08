//! Windows sessions (audit G3). The controller's pipe is per session
//! (`recall-hdr/src/control_pipe.rs`: `recall-hdr-control-<session>`), and only a recorder in
//! this session counts as "running": with two people signed in, the other person's
//! `recall-hdr.exe` is not ours to wait on.

/// The name's fixed part; a session id that can't be read uses it bare (as every side does).
const PIPE_BASE: &str = "recall-hdr-control";

/// `recall-hdr-control-<session>`.
pub(super) fn pipe_name(session: Option<u32>) -> String {
    match session {
        Some(id) => format!("{PIPE_BASE}-{id}"),
        None => PIPE_BASE.to_string(),
    }
}

/// The Windows session a process runs in; None when it can't be read (another account's
/// process, or one that has exited).
pub(super) fn session_of(pid: u32) -> Option<u32> {
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    let mut session = 0u32;
    // SAFETY: plain out-parameter call.
    unsafe { ProcessIdToSessionId(pid, &mut session) }.ok()?;
    Some(session)
}

pub(super) fn current_session() -> Option<u32> {
    session_of(std::process::id())
}

/// The controller pipe for this session.
pub(super) fn current_pipe_name() -> String {
    pipe_name(current_session())
}

/// Whether any `(exe name, pid)` row names `exe_name` (case-insensitive) in session `mine`.
/// A row whose session can't be read is not ours.
pub(super) fn running_in_session(rows: &[(String, u32)], exe_name: &str, mine: Option<u32>, session_of: impl Fn(u32) -> Option<u32>) -> bool {
    let Some(mine) = mine else {
        // Our own session unknown: fall back to any match (the old behaviour).
        return rows.iter().any(|(name, _)| name.eq_ignore_ascii_case(exe_name));
    };
    rows.iter().any(|(name, pid)| name.eq_ignore_ascii_case(exe_name) && session_of(*pid) == Some(mine))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_name_differs_per_session() {
        assert_eq!(pipe_name(Some(1)), "recall-hdr-control-1");
        assert_ne!(pipe_name(Some(1)), pipe_name(Some(2)));
        assert_eq!(pipe_name(None), "recall-hdr-control");
        assert!(current_session().is_some());
        assert_eq!(current_pipe_name(), pipe_name(current_session()));
    }

    #[test]
    fn process_running_ignores_other_sessions() {
        let rows = vec![("explorer.exe".to_string(), 10), ("recall-hdr.exe".to_string(), 20), ("RECALL-HDR.EXE".to_string(), 30)];
        // pid 20 is the other user's (session 2); pid 30 can't be read.
        let sessions = |pid: u32| match pid {
            10 | 20 => Some(2),
            _ => None,
        };
        assert!(!running_in_session(&rows, "recall-hdr.exe", Some(1), sessions), "another session's recorder is not ours");
        assert!(running_in_session(&rows, "recall-hdr.exe", Some(2), sessions));
        assert!(running_in_session(&rows, "recall-hdr.exe", None, sessions), "own session unknown: any match");
        assert!(!running_in_session(&rows, "recall.exe", Some(2), sessions));
    }
}
