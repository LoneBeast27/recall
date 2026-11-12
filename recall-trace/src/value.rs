//! The only values an event field may hold. There is no string variant: a word must be a
//! `&'static str` from Recall's own code, and it is checked against a short safe alphabet
//! when written, so a path, a title, a clip name or a device name cannot reach the trace.

/// One field value of a trace event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum V {
    /// A count, a size or an index.
    U(u64),
    /// A signed number (an offset, a delta).
    I(i64),
    /// A yes/no state.
    B(bool),
    /// A duration in milliseconds.
    Ms(u64),
    /// A Windows HRESULT, a Win32 error or an FFmpeg error code, printed as hex.
    Code(u32),
    /// A word from a fixed list in Recall's code (a state or a reason). Never a game or
    /// program name: [`crate::find_content`] flags those.
    W(&'static str),
}

impl From<u64> for V {
    fn from(v: u64) -> Self {
        V::U(v)
    }
}
impl From<u32> for V {
    fn from(v: u32) -> Self {
        V::U(v.into())
    }
}
impl From<usize> for V {
    fn from(v: usize) -> Self {
        V::U(v as u64)
    }
}
impl From<i64> for V {
    fn from(v: i64) -> Self {
        V::I(v)
    }
}
impl From<i32> for V {
    fn from(v: i32) -> Self {
        V::I(v.into())
    }
}
impl From<bool> for V {
    fn from(v: bool) -> Self {
        V::B(v)
    }
}
impl From<std::time::Duration> for V {
    fn from(v: std::time::Duration) -> Self {
        V::Ms(v.as_millis().min(u64::MAX as u128) as u64)
    }
}
impl From<&'static str> for V {
    fn from(v: &'static str) -> Self {
        V::W(v)
    }
}

/// A word (event name, field key or `V::W`) may only use `a-z 0-9 _ . -`, 1 to 40
/// characters, and must start with a letter. Anything else is written as `?`.
pub fn safe_word(word: &str) -> &str {
    let ok = !word.is_empty()
        && word.len() <= 40
        && word.as_bytes()[0].is_ascii_lowercase()
        && word.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-'));
    if ok {
        word
    } else {
        "?"
    }
}

impl V {
    /// The JSON text of this value.
    pub fn json(&self) -> String {
        match *self {
            V::U(v) | V::Ms(v) => v.to_string(),
            V::I(v) => v.to_string(),
            V::B(v) => v.to_string(),
            V::Code(v) => format!("\"0x{v:08X}\""),
            V::W(w) => format!("\"{}\"", safe_word(w)),
        }
    }
}
