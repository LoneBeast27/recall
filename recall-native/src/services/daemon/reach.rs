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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorder_that_runs_but_never_answers_is_unreachable() {
        assert_eq!(reach(false, Duration::from_secs(60)), RecorderReach::Off);
        assert_eq!(reach(true, Duration::from_secs(1)), RecorderReach::Starting);
        assert_eq!(reach(true, Duration::from_secs(6)), RecorderReach::Unreachable);
    }
}
