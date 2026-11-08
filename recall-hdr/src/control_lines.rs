//! Reading a control-pipe client's lines with a length cap (audit G14). `BufRead::lines`
//! collects a line until `\n` with no limit, so a client that writes without newlines
//! made the controller allocate until it ran out of memory (and recording ended).

use std::io::{BufRead, BufReader, Read};

/// The longest line a client may send. Real commands are a few hundred bytes; a
/// `set_config` patch stays well under this.
pub const MAX_LINE: usize = 1 << 20;

/// How a client's input ended.
#[derive(Debug, PartialEq, Eq)]
pub enum LinesEnd {
    /// End of the stream, a read error, invalid UTF-8, or `on_line` asked to stop.
    Closed,
    /// A line ran past `max` bytes with no newline: the client is dropped.
    TooLong,
}

/// Calls `on_line` with each line (newline and surrounding whitespace removed, blank lines
/// skipped) until the stream ends, `on_line` returns false, or a line exceeds `max` bytes.
/// At most `max + 1` bytes are buffered at any time.
pub fn read_lines_capped(reader: impl Read, max: usize, mut on_line: impl FnMut(&str) -> bool) -> LinesEnd {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let limit = (max as u64).saturating_add(1);
        match (&mut reader).take(limit).read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return LinesEnd::Closed,
            Ok(_) => {}
        }
        if buf.last() != Some(&b'\n') && buf.len() > max {
            return LinesEnd::TooLong;
        }
        let Ok(text) = std::str::from_utf8(&buf) else { return LinesEnd::Closed };
        let line = text.trim();
        if !line.is_empty() && !on_line(line) {
            return LinesEnd::Closed;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(input: &[u8], max: usize) -> (Vec<String>, LinesEnd) {
        let mut lines = Vec::new();
        let end = read_lines_capped(input, max, |l| {
            lines.push(l.to_string());
            true
        });
        (lines, end)
    }

    #[test]
    fn lines_are_trimmed_blanks_skipped_and_a_last_line_without_newline_counts() {
        let (lines, end) = collect(b"  {\"cmd\":\"quit\"}  \n\n\r\nb", 64);
        assert_eq!(lines, vec![r#"{"cmd":"quit"}"#, "b"]);
        assert_eq!(end, LinesEnd::Closed);
    }

    #[test]
    fn a_line_at_the_cap_passes_and_one_past_it_drops_the_client() {
        let mut exact = vec![b'x'; 16];
        exact.push(b'\n');
        exact.extend_from_slice(b"next\n");
        assert_eq!(collect(&exact, 16), (vec!["x".repeat(16), "next".into()], LinesEnd::Closed));
        let mut long = vec![b'y'; 17];
        long.extend_from_slice(b"\nnever\n");
        assert_eq!(collect(&long, 16), (vec![], LinesEnd::TooLong));
    }

    #[test]
    fn invalid_utf8_or_a_false_return_stops_reading() {
        assert_eq!(collect(b"ok\n\xC3\x28\nlater\n", 64), (vec!["ok".into()], LinesEnd::Closed));
        let mut seen = 0;
        assert_eq!(read_lines_capped(&b"a\nb\nc\n"[..], 64, |_| { seen += 1; seen < 2 }), LinesEnd::Closed);
        assert_eq!(seen, 2);
    }
}
