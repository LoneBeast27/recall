//! Crash records: a panic hook and a native crash handler that each write one small JSON file,
//! `crash-<program>-<n>.json`, next to the trace, keeping the newest [`KEEP`]. A record holds
//! the program, the source location of a panic (crate-relative), the exception code and the
//! offset inside the program for a native crash, the thread name and the last trace lines.
//! It never holds the panic message: that is free text and can carry a path or a name.
//! A panic the program recovers from (`catch_unwind`) is recorded too, marked `panic`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::trace::{civil_from_days, snapshot};
use crate::value::safe_word;

/// Crash records kept in the trace folder (all programs together).
pub const KEEP: usize = 10;

struct Target {
    program: &'static str,
    version: &'static str,
    dir: PathBuf,
}

static TARGET: OnceLock<Target> = OnceLock::new();

/// Install the panic hook (chained before the existing one) and the native crash handler.
/// `dir` none = install nothing (tests, the trace setting off). Later calls are no-ops.
pub fn install(program: &'static str, version: &'static str, dir: Option<PathBuf>) {
    let Some(dir) = dir else { return };
    if TARGET.set(Target { program, version, dir }).is_err() {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let at = info.location().map(|l| format!("{}:{}", source_path(l.file()), l.line()));
        write_record("panic", at.as_deref(), None);
        previous(info);
    }));
    native::install();
}

/// A source file as a crate-relative path: `C:\..\registry\src\..\gpui-0.2.2\src\app.rs`
/// becomes `gpui-0.2.2/src/app.rs`; `recall-hdr/src/main.rs` stays. Anything outside the
/// safe alphabet becomes `?`.
pub fn source_path(file: &str) -> String {
    let file = file.replace('\\', "/");
    let parts: Vec<&str> = file.split('/').filter(|p| !p.is_empty()).collect();
    let keep = match parts.iter().rposition(|p| *p == "src") {
        Some(i) if i > 0 => parts[i - 1..].join("/"),
        Some(_) => parts.join("/"),
        None => parts.last().map(|s| s.to_string()).unwrap_or_default(),
    };
    if safe_at(&keep) && !keep.contains(':') {
        keep
    } else {
        "?".into()
    }
}

/// A source location (`recall-hdr/src/main.rs:12`) uses only `A-Z a-z 0-9 _ . - / :`.
fn safe_at(at: &str) -> bool {
    !at.is_empty() && at.len() <= 160 && at.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b'/' | b':'))
}

/// What a native crash adds to a record: the exception code and the offset inside the
/// program's own image (none: the address was in another module).
pub type Native = (u32, Option<u64>);

/// The JSON record for one crash.
pub fn record_json(program: &str, version: &str, kind: &str, at: Option<&str>, native: Option<Native>, thread: Option<&str>, lines: &[String]) -> String {
    let mut out = format!("{{\"kind\":\"{}\",\"program\":\"{}\",\"version\":\"{}\"", safe_word(kind), safe_word(program), version_text(version));
    if let Some(days) = SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs() / 86_400) {
        let (y, m, d) = civil_from_days(days as i64);
        out.push_str(&format!(",\"day\":\"{y:04}-{m:02}-{d:02}\""));
    }
    if let Some(at) = at {
        out.push_str(&format!(",\"at\":\"{}\"", if safe_at(at) && !at.contains(":\\") && !at.contains(":/") { at } else { "?" }));
    }
    if let Some((code, offset)) = native {
        out.push_str(&format!(",\"code\":\"0x{code:08X}\""));
        match offset {
            Some(o) => out.push_str(&format!(",\"offset\":\"0x{o:X}\"")),
            None => out.push_str(",\"offset\":\"other module\""),
        }
    }
    out.push_str(&format!(",\"thread\":\"{}\"", thread.map(thread_word).unwrap_or("?")));
    out.push_str(",\"trace\":[");
    out.push_str(&lines.join(","));
    out.push_str("]}");
    out
}

fn version_text(version: &str) -> &str {
    let version = version.trim_start_matches('v');
    if !version.is_empty() && version.len() <= 20 && version.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        version
    } else {
        "?"
    }
}

/// Thread names are Recall's own (`saver`, `mover`); a name outside the safe alphabet is `?`.
fn thread_word(name: &str) -> &str {
    let ok = !name.is_empty()
        && name.len() <= 40
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b' '));
    if ok {
        name
    } else {
        "?"
    }
}

/// Write a record for the installed program (no-op before [`install`]).
pub(crate) fn write_record(kind: &str, at: Option<&str>, native: Option<Native>) {
    let Some(target) = TARGET.get() else { return };
    let lines = snapshot().map(|(_, lines)| lines).unwrap_or_default();
    let thread = std::thread::current();
    let json = record_json(target.program, target.version, kind, at, native, thread.name(), &lines);
    write_to(&target.dir, target.program, &json);
}

/// Write `json` as the newest crash record in `dir` and drop the oldest past [`KEEP`].
pub fn write_to(dir: &Path, program: &str, json: &str) -> Option<PathBuf> {
    let _ = fs::create_dir_all(dir);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let path = dir.join(format!("crash-{}-{stamp}.json", safe_word(program)));
    fs::write(&path, json).ok()?;
    prune(dir);
    Some(path)
}

/// The crash records in `dir`, oldest first.
pub fn records(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<(u128, PathBuf)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let stem = name.strip_prefix("crash-")?.strip_suffix(".json")?;
            let stamp = stem.rsplit('-').next()?.parse().ok()?;
            Some((stamp, e.path()))
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, p)| p).collect()
}

fn prune(dir: &Path) {
    let all = records(dir);
    for old in all.iter().take(all.len().saturating_sub(KEEP)) {
        let _ = fs::remove_file(old);
    }
}

#[cfg(windows)]
mod native {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[repr(C)]
    struct ExceptionRecord {
        code: u32,
        flags: u32,
        record: *mut ExceptionRecord,
        address: *mut c_void,
    }
    #[repr(C)]
    struct ExceptionPointers {
        record: *mut ExceptionRecord,
        context: *mut c_void,
    }
    type Filter = unsafe extern "system" fn(*const ExceptionPointers) -> i32;

    #[link(name = "kernel32")]
    extern "system" {
        fn SetUnhandledExceptionFilter(filter: Option<Filter>) -> Option<Filter>;
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    }

    static PREVIOUS: AtomicUsize = AtomicUsize::new(0);

    pub fn install() {
        let previous = unsafe { SetUnhandledExceptionFilter(Some(filter)) };
        PREVIOUS.store(previous.map_or(0, |f| f as usize), Ordering::SeqCst);
    }

    /// The program's own image as (base, size), read from its PE header.
    fn image() -> Option<(usize, usize)> {
        let base = unsafe { GetModuleHandleW(std::ptr::null()) } as usize;
        if base == 0 {
            return None;
        }
        unsafe {
            let pe = base + *((base + 0x3C) as *const u32) as usize;
            // Signature (4) + file header (20), then SizeOfImage at 56 in the optional header.
            let size = *((pe + 4 + 20 + 56) as *const u32) as usize;
            Some((base, size))
        }
    }

    unsafe extern "system" fn filter(info: *const ExceptionPointers) -> i32 {
        if let Some(record) = info.as_ref().and_then(|i| i.record.as_ref()) {
            let address = record.address as usize;
            let offset = image().and_then(|(base, size)| (address >= base && address < base + size).then(|| (address - base) as u64));
            super::write_record("native", None, Some((record.code, offset)));
        }
        match PREVIOUS.load(Ordering::SeqCst) {
            0 => 0, // EXCEPTION_CONTINUE_SEARCH: Windows ends the program as before.
            f => {
                let previous: Filter = std::mem::transmute(f);
                previous(info)
            }
        }
    }
}

#[cfg(not(windows))]
mod native {
    pub fn install() {}
}
