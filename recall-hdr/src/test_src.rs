//! Test helper for the source guards (`include_str!` on a controller file: no test can
//! build a `Controller`, it needs a live supervisor, hub and display).

/// The text of `fn <name>(` in `src`, up to its closing brace at 4-space indent (a method).
pub fn method_body<'a>(src: &'a str, name: &str) -> &'a str {
    let start = src.find(&format!("fn {name}(")).unwrap_or_else(|| panic!("fn {name} not found"));
    let end = src[start..].find("\n    }").map(|e| start + e).unwrap_or_else(|| panic!("fn {name} has no end"));
    &src[start..end]
}
