//! Recall's anonymous action trace (the bug logger) and crash records.
//!
//! The trace records what Recall did, in what order, and what failed: action names, states,
//! counts, durations and error codes. It never records content: no paths, window titles,
//! clip names, device names or program names. The API enforces this: a field holds a [`V`],
//! which has no string variant, and every word is checked against a short safe alphabet.
//! Everything stays in `%LOCALAPPDATA%\Recall\trace`; nothing is sent anywhere.

mod crash;
mod file;
mod scrub;
mod trace;
mod value;

pub use crash::{
    file_pending, install as install_crash_records, native_bytes, native_prefix, pending_path, record_json, records as crash_records, source_path, thread_word,
    write_to as write_crash_record, Native, KEEP, NATIVE_BUF,
};
pub use scrub::find_content;
pub use trace::{act, act_on_change, default_dir, init, line, Trace, RECENT_LINES, TRACE_CAP};
pub use value::{safe_word, V};

/// The release version (`RECALL_VERSION` at build time, as release builds set it), or `0`
/// for a development build.
pub const BUILD_VERSION: &str = match option_env!("RECALL_VERSION") {
    Some(v) => v,
    None => "0",
};

/// Variables only Recall's tests and check scripts set. With any of them present a run
/// writes no trace or crash record, so a check never touches the user's trace folder.
pub const TEST_MARKERS: &[&str] = &[
    "RECALL_INSTANCE_TAG",
    "RECALL_NATIVE_OFFSCREEN",
    "RECALL_NATIVE_TRACE",
    "RECALL_NATIVE_LAYOUT",
    "RECALL_NATIVE_CLIP",
    "RECALL_NO_CONTROLLER",
    "RECALL_CONTROL_PIPE",
    "RECALL_CPU_MASK",
    "RECALL_FAKE_RECORDING",
    "RECALL_FAKE_NO_RECORDER",
    "RECALL_FAKE_FREE_BYTES",
    "RECALL_SMOKE_CLIPS",
    "RECALL_SLOW_DRIVE",
    "RECALL_HW_TESTS",
    "RECALL_COMPAT_WRITE_DIR",
];

/// Where this run keeps its trace: `RECALL_TRACE_DIR` when set (`off` = memory only); none
/// when any [`TEST_MARKERS`] variable is set; else `%LOCALAPPDATA%\Recall\trace`.
pub fn run_dir() -> Option<std::path::PathBuf> {
    dir_from(|k| std::env::var(k).ok())
}

pub(crate) fn dir_from(var: impl Fn(&str) -> Option<String>) -> Option<std::path::PathBuf> {
    match var("RECALL_TRACE_DIR").filter(|v| !v.is_empty()) {
        Some(v) if v.eq_ignore_ascii_case("off") => None,
        Some(v) => Some(v.into()),
        None if TEST_MARKERS.iter().any(|m| var(m).is_some_and(|t| !t.is_empty())) => None,
        None => var("LOCALAPPDATA").map(|d| std::path::PathBuf::from(d).join("Recall").join("trace")),
    }
}

/// Start the trace and the crash records for one program (`recorder`, `island`, `library`),
/// both under [`run_dir`] (none: memory only, no crash records).
pub fn start(program: &'static str) {
    let dir = run_dir();
    init(program, BUILD_VERSION, dir.clone());
    install_crash_records(program, BUILD_VERSION, dir);
}

#[cfg(test)]
mod tests;
