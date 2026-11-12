//! Crash records: a panic hook and a native crash handler, each leaving one small JSON file
//! `crash-<program>-<n>.json` next to the trace (`n` counts up; the newest [`KEEP`] stay).
//!
//! A panic record holds the program, the crate-relative source location, the thread name's
//! first word and the last trace lines. It never holds the panic message: that is free text
//! and can carry a path or a name. A panic the program recovers from (`catch_unwind`) is
//! recorded too.
//!
//! The native handler runs inside a crashed process, where the heap or the loader lock may be
//! held, so it allocates nothing and touches no `std::fs`: everything it needs (the file path,
//! the record's first bytes, the program's image range) is prepared by [`install`], and the
//! filter writes the exception code and the image offset into a stack buffer with one
//! `CreateFileW`/`WriteFile` to `crash-<program>-native.pending`. It writes once per process
//! (a second crash only chains on), skips stack overflows (no stack left to run on), and then
//! hands over to the previous filter, so Windows Error Reporting runs as before. The next
//! start turns the pending file into a numbered record.

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

/// Install the panic hook (chained before the existing one) and the native crash handler,
/// and file a native crash left by the previous run. `dir` none = install nothing (tests, the
/// trace setting off). Later calls are no-ops.
pub fn install(program: &'static str, version: &'static str, dir: Option<PathBuf>) {
    let Some(dir) = dir else { return };
    if TARGET.set(Target { program, version, dir: dir.clone() }).is_err() {
        return;
    }
    file_pending(&dir, program);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let at = info.location().map(|l| format!("{}:{}", source_path(l.file()), l.line()));
        write_record("panic", at.as_deref(), None);
        previous(info);
    }));
    native::install(&pending_path(&dir, program), &native_prefix(program, version));
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

/// A thread name's first word (`vu-<device>` becomes `vu`): Recall names some threads after
/// what they serve, and that part can be a user's word. `?` outside the safe alphabet.
pub fn thread_word(name: &str) -> &str {
    safe_word(name.split(['-', ' ', ':']).next().unwrap_or(""))
}

/// Write a record for the installed program (no-op before [`install`]).
pub(crate) fn write_record(kind: &str, at: Option<&str>, native: Option<Native>) {
    let Some(target) = TARGET.get() else { return };
    let lines = snapshot().map(|(_, lines)| lines).unwrap_or_default();
    let thread = std::thread::current();
    let json = record_json(target.program, target.version, kind, at, native, thread.name(), &lines);
    write_to(&target.dir, target.program, &json);
}

/// Write `json` as the newest crash record in `dir` (the next number after the highest one
/// there) and drop the oldest past [`KEEP`].
pub fn write_to(dir: &Path, program: &str, json: &str) -> Option<PathBuf> {
    let _ = fs::create_dir_all(dir);
    let next = records_numbered(dir).last().map_or(1, |(n, _)| n + 1);
    let path = dir.join(format!("crash-{}-{next}.json", safe_word(program)));
    fs::write(&path, json).ok()?;
    prune(dir);
    Some(path)
}

/// The crash records in `dir`, oldest first.
pub fn records(dir: &Path) -> Vec<PathBuf> {
    records_numbered(dir).into_iter().map(|(_, p)| p).collect()
}

fn records_numbered(dir: &Path) -> Vec<(u64, PathBuf)> {
    let mut found: Vec<(u64, PathBuf)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let stem = name.strip_prefix("crash-")?.strip_suffix(".json")?;
            let n = stem.rsplit('-').next()?.parse().ok()?;
            Some((n, e.path()))
        })
        .collect();
    found.sort();
    found
}

fn prune(dir: &Path) {
    let all = records(dir);
    for old in all.iter().take(all.len().saturating_sub(KEEP)) {
        let _ = fs::remove_file(old);
    }
}

/// Where the native handler of `program` leaves its record.
pub fn pending_path(dir: &Path, program: &str) -> PathBuf {
    dir.join(format!("crash-{}-native.pending", safe_word(program)))
}

/// The record's fixed first bytes, made before any crash: `{"kind":"native",...,"version":"…"`.
pub fn native_prefix(program: &str, version: &str) -> Vec<u8> {
    format!("{{\"kind\":\"native\",\"program\":\"{}\",\"version\":\"{}\"", safe_word(program), version_text(version)).into_bytes()
}

/// The longest prefix the handler's buffer takes; [`native_bytes`] needs 64 bytes more.
pub const PREFIX_MAX: usize = 128;
/// The handler's stack buffer.
pub const NATIVE_BUF: usize = PREFIX_MAX + 64;

/// Fill `buf` with a native record: `prefix` + the code + the offset, no heap and no
/// formatting machinery (the native handler calls this). Returns the bytes used.
pub fn native_bytes(prefix: &[u8], code: u32, offset: Option<u64>, buf: &mut [u8; NATIVE_BUF]) -> usize {
    let mut at = 0;
    let mut put = |bytes: &[u8], at: &mut usize| {
        for &b in bytes {
            if *at < buf.len() {
                buf[*at] = b;
                *at += 1;
            }
        }
    };
    put(&prefix[..prefix.len().min(PREFIX_MAX)], &mut at);
    put(b",\"code\":\"0x", &mut at);
    put(&hex(u64::from(code))[8..], &mut at);
    match offset {
        Some(o) => {
            put(b"\",\"offset\":\"0x", &mut at);
            put(&hex(o), &mut at);
            put(b"\"}", &mut at);
        }
        None => put(b"\",\"offset\":\"other module\"}", &mut at),
    }
    at
}

/// `value` as 16 upper-case hex digits, on the stack (the last 8 hold a `u32`).
fn hex(value: u64) -> [u8; 16] {
    let mut out = [b'0'; 16];
    for (i, slot) in out.iter_mut().enumerate() {
        let nibble = ((value >> (4 * (15 - i))) & 0xF) as u8;
        *slot = if nibble < 10 { b'0' + nibble } else { b'A' + nibble - 10 };
    }
    out
}

/// Turn `program`'s pending native record from the previous run into a numbered record.
pub fn file_pending(dir: &Path, program: &str) {
    let pending = pending_path(dir, program);
    let Ok(bytes) = fs::read(&pending) else { return };
    let _ = fs::remove_file(&pending);
    let text: String = bytes.iter().map(|&b| b as char).collect();
    let well_formed = text.starts_with("{\"kind\":\"native\"") && text.ends_with('}') && text.bytes().all(|b| (0x20..0x7F).contains(&b));
    if well_formed {
        write_to(dir, program, &text);
    }
}

#[cfg(windows)]
mod native {
    use std::ffi::c_void;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::OnceLock;

    use super::{native_bytes, NATIVE_BUF};

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
        fn CreateFileW(name: *const u16, access: u32, share: u32, security: *const c_void, disposition: u32, flags: u32, template: *mut c_void) -> *mut c_void;
        fn WriteFile(file: *mut c_void, buf: *const u8, len: u32, written: *mut u32, overlapped: *mut c_void) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    const STACK_OVERFLOW: u32 = 0xC000_00FD;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const CREATE_ALWAYS: u32 = 2;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;

    static PREVIOUS: AtomicUsize = AtomicUsize::new(0);
    static WRITTEN: AtomicBool = AtomicBool::new(false);
    static BASE: AtomicUsize = AtomicUsize::new(0);
    static SIZE: AtomicUsize = AtomicUsize::new(0);
    /// The pending file's NUL-terminated UTF-16 path and the record's first bytes.
    static PATH: OnceLock<Vec<u16>> = OnceLock::new();
    static PREFIX: OnceLock<Vec<u8>> = OnceLock::new();

    pub fn install(path: &Path, prefix: &[u8]) {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        if PATH.set(wide).is_err() || PREFIX.set(prefix.to_vec()).is_err() {
            return;
        }
        if let Some((base, size)) = image() {
            BASE.store(base, Ordering::SeqCst);
            SIZE.store(size, Ordering::SeqCst);
        }
        let previous = unsafe { SetUnhandledExceptionFilter(Some(filter)) };
        PREVIOUS.store(previous.map_or(0, |f| f as usize), Ordering::SeqCst);
    }

    /// The program's own image as (base, size), read from its PE header (at install).
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

    /// No heap, no std::fs, no formatting: stack buffer + one CreateFileW/WriteFile.
    unsafe extern "system" fn filter(info: *const ExceptionPointers) -> i32 {
        if let Some(record) = info.as_ref().and_then(|i| i.record.as_ref()) {
            if record.code != STACK_OVERFLOW && !WRITTEN.swap(true, Ordering::SeqCst) {
                write(record.code, record.address as usize);
            }
        }
        match PREVIOUS.load(Ordering::SeqCst) {
            0 => 0, // EXCEPTION_CONTINUE_SEARCH: Windows ends the program as before.
            f => {
                let previous: Filter = std::mem::transmute(f);
                previous(info)
            }
        }
    }

    unsafe fn write(code: u32, address: usize) {
        let (Some(path), Some(prefix)) = (PATH.get(), PREFIX.get()) else { return };
        let (base, size) = (BASE.load(Ordering::SeqCst), SIZE.load(Ordering::SeqCst));
        let offset = (base != 0 && address >= base && address < base + size).then(|| (address - base) as u64);
        let mut buf = [0u8; NATIVE_BUF];
        let len = native_bytes(prefix, code, offset, &mut buf);
        let file = CreateFileW(path.as_ptr(), GENERIC_WRITE, 0, std::ptr::null(), CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, std::ptr::null_mut());
        if file.is_null() || file as isize == -1 {
            return;
        }
        let mut written = 0u32;
        WriteFile(file, buf.as_ptr(), len as u32, &mut written, std::ptr::null_mut());
        CloseHandle(file);
    }
}

#[cfg(not(windows))]
mod native {
    pub fn install(_path: &std::path::Path, _prefix: &[u8]) {}
}
