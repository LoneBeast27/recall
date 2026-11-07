//! Stdout / stderr writes that never panic. The engine's stdout and stderr are pipes to
//! the controller; when the controller dies, std's `println!` / `eprintln!` panic on the
//! next write (`ERROR_NO_DATA`). This module is declared first in `main.rs` with
//! `#[macro_use]`, so its `println!` / `eprintln!` shadow std's in every engine module:
//! a line that cannot be written is dropped, and the first failure points both streams
//! at NUL (`stdin_reader::stdout_failed`). Test builds keep std's macros, so cargo's
//! output capture still works; the writer itself is tested directly.

use std::fmt::Arguments;
use std::io::Write;

/// Write one line to `out`; false (the line dropped) when the pipe is gone.
pub fn write_line_to(out: &mut impl Write, args: Arguments<'_>) -> bool {
    writeln!(out, "{args}").and_then(|()| out.flush()).is_ok()
}

/// `println!`: one line to stdout, never a panic.
#[cfg_attr(test, allow(dead_code))]
pub fn out(args: Arguments<'_>) {
    if !write_line_to(&mut std::io::stdout().lock(), args) {
        crate::stdin_reader::stdout_failed();
    }
}

/// `eprintln!`: one line to stderr, never a panic.
#[cfg_attr(test, allow(dead_code))]
pub fn err(args: Arguments<'_>) {
    if !write_line_to(&mut std::io::stderr().lock(), args) {
        crate::stdin_reader::stdout_failed();
    }
}

#[cfg(not(test))]
macro_rules! println {
    () => { $crate::quiet_io::out(format_args!("")) };
    ($($arg:tt)*) => { $crate::quiet_io::out(format_args!($($arg)*)) };
}

#[cfg(not(test))]
macro_rules! eprintln {
    () => { $crate::quiet_io::err(format_args!("")) };
    ($($arg:tt)*) => { $crate::quiet_io::err(format_args!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::write_line_to;

    /// A pipe whose reader is gone (`ERROR_NO_DATA`).
    struct ClosedPipe;
    impl std::io::Write for ClosedPipe {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from_raw_os_error(232))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::from_raw_os_error(232))
        }
    }

    #[test]
    fn a_log_line_to_a_closed_pipe_is_dropped_without_a_panic() {
        let wrote = std::panic::catch_unwind(|| write_line_to(&mut ClosedPipe, format_args!("[recall] x {}", 1)));
        assert_eq!(wrote.ok(), Some(false), "dropped, no panic");
        let mut ok = Vec::new();
        assert!(write_line_to(&mut ok, format_args!("[recall] {}", "y")));
        assert_eq!(ok, b"[recall] y\n");
    }
}
