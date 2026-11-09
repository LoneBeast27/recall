//! Engine log lines on stderr that never panic.
//!
//! The controller pipes the engine's stderr (`supervisor.rs`). When the
//! controller dies that pipe closes, and std's `eprintln!` panics on the failed
//! write (Windows std only excuses an invalid handle). A panic on the main
//! thread unwinds past the long recording's shutdown and leaves a `.part`; on
//! the stdin thread it stops the quit. So the engine's `eprintln!` is this
//! crate's macro (defined at the top of `main.rs`, before every module): the
//! line is written if it can be, and dropped if not.

use std::fmt;
use std::io::Write;

/// Writes one line to `out`; a failed write (a closed pipe) is ignored.
pub fn write_line(out: &mut dyn Write, args: fmt::Arguments) {
    let _ = out.write_fmt(args).and_then(|_| out.write_all(b"\n"));
}

/// One line on this process's stderr, never a panic.
pub fn line(args: fmt::Arguments) {
    write_line(&mut std::io::stderr().lock(), args);
}

#[cfg(test)]
mod tests {
    use super::*;

    struct BrokenPipe;

    impl Write for BrokenPipe {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn a_closed_pipe_does_not_panic() {
        write_line(&mut BrokenPipe, format_args!("[recall] {} left", 3));
    }

    #[test]
    fn a_line_is_written_with_its_newline() {
        let mut out = Vec::new();
        write_line(&mut out, format_args!("[recall] {}", "ok"));
        assert_eq!(out, b"[recall] ok\n");
    }
}
