//! Why the recorder is not connected (audit R2-deep-startup-3). A recorder that runs in this
//! session but never takes the pipe (its pipe failed, or it runs as another account, e.g.
//! elevated with an administrator's credentials) used to show "Recorder off · Start", and Start
//! did nothing because a recorder already runs. The app now says so instead.
use std::time::Duration;

/// How long a running recorder may stay unconnected before it counts as unreachable: a
/// recorder that was just started needs a moment to open its pipe.
pub const UNREACHABLE_AFTER: Duration = Duration::from_secs(5);

/// The disconnected recorder's state, for the status pill and the Start buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecorderReach {
    /// No recorder runs in this session: Start starts one.
    #[default]
    Off,
    /// One runs and was seen only briefly (it is starting): wait.
    Starting,
    /// One runs but has not answered for [`UNREACHABLE_AFTER`]: Start can't help.
    Unreachable,
}

/// `running`: a recorder process runs in this session. `disconnected_for`: how long the app
/// has been without a connection while one runs.
pub fn reach(running: bool, disconnected_for: Duration) -> RecorderReach {
    if !running {
        RecorderReach::Off
    } else if disconnected_for < UNREACHABLE_AFTER {
        RecorderReach::Starting
    } else {
        RecorderReach::Unreachable
    }
}

/// [`reach`], except that a connect refused by the owner check (`pipe_owner`: the pipe's
/// server runs as another account) is unreachable at once: that recorder is not in this
/// session's process list, and Start can't help.
pub fn reach_or_refused(refused: bool, running: bool, disconnected_for: Duration) -> RecorderReach {
    if refused {
        RecorderReach::Unreachable
    } else {
        reach(running, disconnected_for)
    }
}

/// The unreachable reason for the toast and the Settings banner: the refusal when the owner
/// check refused the pipe, else `fallback` (a recorder that runs but never answers).
pub fn unreachable_detail(refused: Option<&str>, fallback: &str) -> String {
    match refused {
        Some(reason) => format!(
            "{reason}. Recall sends it nothing: a recorder under another account must not get this account's commands. \
             Quit it from that account (its tray icon), then open Recall again."
        ),
        None => fallback.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_pipe_is_unreachable_with_its_reason() {
        let refused = "another user owns the Recall pipe (server pid 9 runs as S-1-5-21-2)";
        assert_eq!(reach_or_refused(true, false, Duration::ZERO), RecorderReach::Unreachable, "no recorder of ours runs");
        assert_eq!(reach_or_refused(false, false, Duration::ZERO), RecorderReach::Off);
        assert!(unreachable_detail(Some(refused), "fallback").starts_with(refused));
        assert_eq!(unreachable_detail(None, "fallback"), "fallback");
    }

    #[test]
    fn the_refusal_text_has_no_run_of_spaces() {
        // R4-startup-3: a line break inside the literal put 14 spaces into the toast.
        let text = unreachable_detail(Some("x"), "f");
        assert!(!text.contains("  "), "{text:?}");
        assert!(text.contains("commands. Quit it from that account"), "{text:?}");
    }

    #[test]
    fn a_recorder_that_runs_but_never_answers_is_unreachable() {
        assert_eq!(reach(false, Duration::from_secs(60)), RecorderReach::Off);
        assert_eq!(reach(true, Duration::from_secs(1)), RecorderReach::Starting);
        assert_eq!(reach(true, Duration::from_secs(6)), RecorderReach::Unreachable);
    }
}
