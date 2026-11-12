//! The action trace: one JSON line per action, `{"t":<ms since start>,"e":"<event>","f":{...}}`.
//! A session line opens each run with the program, its version and the UTC date (no time of
//! day), so lines cannot be matched to the user's day. The last lines are also kept in memory
//! for a crash record.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::file::RollingFile;
use crate::value::{safe_word, V};

/// The live trace never grows past this; the previous file (`.1`) holds at most as much.
pub const TRACE_CAP: u64 = 1 << 20;
/// Lines kept in memory for a crash record.
pub const RECENT_LINES: usize = 200;

pub struct Trace {
    program: &'static str,
    start: Instant,
    file: Option<RollingFile>,
    recent: VecDeque<String>,
}

impl Trace {
    /// A trace for `program` writing to `dir\<program>.trace` (no dir: memory only).
    pub fn open(program: &'static str, version: &'static str, dir: Option<PathBuf>, cap: u64) -> Self {
        let file = dir.map(|d| RollingFile::open(d.join(format!("{}.trace", safe_word(program))), cap));
        let mut trace = Trace { program, start: Instant::now(), file, recent: VecDeque::new() };
        trace.event("session.start", &[("program", V::W(program)), ("version", V::W(version_word(version)))]);
        if let Some(day) = utc_day() {
            trace.event("session.day", &[("y", V::U(day.0)), ("m", V::U(day.1)), ("d", V::U(day.2))]);
        }
        trace
    }

    pub fn program(&self) -> &'static str {
        self.program
    }

    /// Write one event. Names and keys outside the safe alphabet are written as `?`.
    pub fn event(&mut self, name: &str, fields: &[(&str, V)]) {
        let line = line(self.start.elapsed().as_millis() as u64, name, fields);
        if let Some(f) = self.file.as_mut() {
            f.write_line(&line);
        }
        if self.recent.len() == RECENT_LINES {
            self.recent.pop_front();
        }
        self.recent.push_back(line);
    }

    /// The last lines written, oldest first.
    pub fn recent(&self) -> Vec<String> {
        self.recent.iter().cloned().collect()
    }
}

/// The JSON line for one event.
pub fn line(t_ms: u64, name: &str, fields: &[(&str, V)]) -> String {
    let mut out = format!("{{\"t\":{t_ms},\"e\":\"{}\"", safe_word(name));
    if !fields.is_empty() {
        out.push_str(",\"f\":{");
        for (i, (key, value)) in fields.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!("\"{}\":{}", safe_word(key), value.json()));
        }
        out.push('}');
    }
    out.push('}');
    out
}

/// A version such as `0.1.2` (or `v0.1.2`) as a trace word (`v0.1.2`); anything else is `?`.
fn version_word(version: &'static str) -> &'static str {
    let version = version.trim_start_matches('v');
    if !version.is_empty() && version.len() <= 20 && version.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        // Leaked once per process start: a handful of bytes.
        Box::leak(format!("v{version}").into_boxed_str())
    } else {
        "?"
    }
}

/// Today's UTC date as (year, month, day).
fn utc_day() -> Option<(u64, u64, u64)> {
    let days = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() / 86_400;
    Some(civil_from_days(days as i64))
}

/// Days since 1970-01-01 to a (year, month, day) date (Howard Hinnant's algorithm).
pub(crate) fn civil_from_days(z: i64) -> (u64, u64, u64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y as u64, m as u64, d as u64)
}

static GLOBAL: OnceLock<Mutex<Trace>> = OnceLock::new();

/// `%LOCALAPPDATA%\Recall\trace` (none without LOCALAPPDATA).
pub fn default_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Recall").join("trace"))
}

/// Start the process trace. Later calls are no-ops. `dir` none = memory only (tests,
/// the trace setting off).
pub fn init(program: &'static str, version: &'static str, dir: Option<PathBuf>) {
    GLOBAL.get_or_init(|| Mutex::new(Trace::open(program, version, dir, TRACE_CAP)));
}

/// Record one action in the process trace (a no-op before [`init`]).
pub fn act(name: &str, fields: &[(&str, V)]) {
    if let Some(lock) = GLOBAL.get() {
        if let Ok(mut trace) = lock.lock() {
            trace.event(name, fields);
        }
    }
}

/// Record `name` only when its fields differ from the last call with the same name: for state
/// that is polled many times a second (the island checks every frame). The first call writes.
pub fn act_on_change(name: &'static str, fields: &[(&str, V)]) {
    static LAST: Mutex<Vec<(&'static str, String)>> = Mutex::new(Vec::new());
    let now = fields.iter().map(|(k, v)| format!("{}={}", safe_word(k), v.json())).collect::<Vec<_>>().join(",");
    let Ok(mut last) = LAST.lock() else { return };
    match last.iter_mut().find(|(n, _)| *n == name) {
        Some((_, old)) if *old == now => return,
        Some((_, old)) => *old = now,
        None => last.push((name, now)),
    }
    drop(last);
    act(name, fields);
}

/// The process trace's program name and recent lines, without blocking (crash paths).
pub(crate) fn snapshot() -> Option<(&'static str, Vec<String>)> {
    let trace = GLOBAL.get()?.try_lock().ok()?;
    Some((trace.program(), trace.recent()))
}
