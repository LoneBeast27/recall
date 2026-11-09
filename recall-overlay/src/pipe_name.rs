//! The controller's control pipe for this Windows session (audit G3): the controller serves
//! `recall-hdr-control-<session>` (`recall-hdr/src/control_pipe.rs`), so a second signed-in
//! user's island reaches their own recorder, never the other one.

/// The name's fixed part; a session id that can't be read uses it bare (as every side does).
const PIPE_BASE: &str = "recall-hdr-control";

pub fn pipe_name(session: Option<u32>) -> String {
    match session {
        Some(id) => format!("{PIPE_BASE}-{id}"),
        None => PIPE_BASE.to_string(),
    }
}

/// The pipe for the session this island runs in.
pub fn current_pipe_name() -> String {
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    let mut session = 0u32;
    // SAFETY: plain out-parameter call on our own pid.
    let known = unsafe { ProcessIdToSessionId(std::process::id(), &mut session) }.is_ok();
    pipe_name(known.then_some(session))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_name_differs_per_session() {
        assert_eq!(pipe_name(Some(3)), "recall-hdr-control-3");
        assert_ne!(pipe_name(Some(1)), pipe_name(Some(2)));
        assert_eq!(pipe_name(None), "recall-hdr-control");
        assert!(current_pipe_name().starts_with("recall-hdr-control-"), "{}", current_pipe_name());
    }
}
