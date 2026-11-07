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
pub fn read_commands<R: BufRead>(reader: R, quit: &AtomicBool, apply: impl FnMut(Command)) {
    read_commands_logged(reader, quit, apply, |line| eprintln!("{line}"));
}

/// [`read_commands`] with the "stdin closed" note going to `log`. `quit` is
/// stored first: with the controller dead its stderr pipe is closed too, and a
/// log line that fails must not stop the quit. A line that is not UTF-8 is
/// skipped like any malformed line (it is not the end of stdin).
pub fn read_commands_logged<R: BufRead>(mut reader: R, quit: &AtomicBool, mut apply: impl FnMut(Command), log: impl FnOnce(&str)) {
    let mut raw = Vec::new();
    loop {
        raw.clear();
        match reader.read_until(b'\n', &mut raw) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Ok(0) | Err(_) => break, // EOF, or the pipe broke
            Ok(_) => {}
        }
        let Ok(line) = std::str::from_utf8(&raw) else { continue };
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
    quit.store(true, Relaxed);
    log("[recall] stdin closed (the controller is gone): quitting");
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

    #[test]
    fn quit_is_set_even_when_the_log_line_cannot_be_written() {
        // The controller is gone: its stderr pipe is closed too, and a log write may fail.
        let quit = AtomicBool::new(false);
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            read_commands_logged(std::io::Cursor::new(Vec::new()), &quit, |_| {}, |_| panic!("stderr is a broken pipe"));
        }));
        assert!(run.is_err(), "the failing log is reached");
        assert!(quit.load(Relaxed), "quit comes first, so the engine still quits");
    }

    #[test]
    fn a_line_that_is_not_utf8_is_skipped_not_taken_as_eof() {
        let quit = AtomicBool::new(false);
        let mut applied = 0;
        let input = b"\xff\xfe not text\n{\"cmd\":\"bookmark\"}\n".to_vec();
        read_commands_logged(std::io::Cursor::new(input), &quit, |_| applied += 1, |_| {});
        assert_eq!(applied, 1, "the command after the bad line still runs");
        assert!(quit.load(Relaxed), "then EOF quits");
    }
}
