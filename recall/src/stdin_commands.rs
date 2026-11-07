//! The engine's stdin command loop, over any `BufRead` so it is testable.
//!
//! The controller writes one JSON command per line. A `quit` line and stdin
//! closing (EOF or a read error: the controller crashed or was killed) both set
//! the shared `quit` flag, so the event loop takes its normal quit path and a
//! long recording is finalized instead of the engine dying later on a write to
//! the dead stdout pipe.

use std::io::BufRead;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use crate::protocol::Command;

/// Hands each well-formed command to `apply` until `quit` arrives or `reader`
/// ends; either way `quit` is set on return. Blank and malformed lines are ignored.
pub fn read_commands<R: BufRead>(reader: R, quit: &AtomicBool, mut apply: impl FnMut(Command)) {
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Command>(line) {
            Ok(Command::Quit) => {
                quit.store(true, Relaxed);
                return;
            }
            Ok(cmd) => apply(cmd),
            Err(_) => {} // ignore malformed lines
        }
    }
    eprintln!("[recall] stdin closed (the controller is gone): quitting");
    quit.store(true, Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &str) -> (bool, usize) {
        let quit = AtomicBool::new(false);
        let mut applied = 0;
        read_commands(std::io::Cursor::new(input.to_string()), &quit, |_| applied += 1);
        (quit.load(Relaxed), applied)
    }

    #[test]
    fn stdin_eof_without_a_quit_line_quits() {
        let (quit, applied) = run("{\"cmd\":\"bookmark\"}\n");
        assert!(quit, "the controller is gone: take the normal quit path");
        assert_eq!(applied, 1);
    }

    #[test]
    fn an_empty_stdin_quits() {
        assert_eq!(run(""), (true, 0));
    }

    #[test]
    fn malformed_and_blank_lines_are_ignored() {
        let (quit, applied) = run("not json\n\n{\"cmd\":\"nope\"}\n{\"cmd\":\"screenshot\"}\n{\"cmd\":\"quit\"}\n{\"cmd\":\"bookmark\"}\n");
        assert!(quit);
        assert_eq!(applied, 1, "only the screenshot runs; nothing after quit");
    }
}
