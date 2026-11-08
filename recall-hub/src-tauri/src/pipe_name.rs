//! The controller's control pipe for this Windows session (deep-startup-p3 b): the controller
//! serves `recall-hdr-control-<session>` (`recall-hdr/src/control_pipe.rs`), so the hub reaches
//! this user's recorder, never another signed-in account's. No dependencies on purpose: the
//! recall-hdr tests compile this file to check it (`recall-hdr/src/hub_pipe_tests.rs`), since
//! the hub (Tauri) can't be built without its frontend.

/// The name's fixed part; a session id that can't be read uses it bare (as every side does).
const PIPE_BASE: &str = "recall-hdr-control";

/// `recall-hdr-control-<session>`.
pub fn pipe_name(session: Option<u32>) -> String {
    match session {
        Some(id) => format!("{PIPE_BASE}-{id}"),
        None => PIPE_BASE.to_string(),
    }
}

#[link(name = "kernel32")]
extern "system" {
    fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> i32;
}

/// The pipe for the session this hub runs in.
pub fn current_pipe_name() -> String {
    let mut session = 0u32;
    // SAFETY: plain out-parameter call on our own pid.
    let known = unsafe { ProcessIdToSessionId(std::process::id(), &mut session) } != 0;
    pipe_name(known.then_some(session))
}
