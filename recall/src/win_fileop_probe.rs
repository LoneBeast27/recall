//! Live probe of `recycle_files` (ignored by default). Run it ON A HIDDEN
//! DESKTOP, so that a dialog Windows might show can never appear on screen:
//!
//!     cargo test -p recall-hdr --offline fileop_probe_hidden_desktop -- --ignored --nocapture
//!
//! That launcher creates a desktop nobody looks at, starts this test binary on
//! it with only `fileop_live_probe`, waits for it (at most 4 minutes), lists
//! every window left on that desktop, and closes it. The probe itself uses
//! sandbox files under this checkout's `.tmp` only and removes everything it
//! made (a `subst` letter, the sandbox, its own Recycle Bin entries).

use super::*;
use std::ffi::c_void;
use std::io::Write;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

const GIB: u64 = 1 << 30;

fn say(line: String) {
    println!("{line}");
    let file = std::env::var("RECALL_PROBE_OUT").ok().and_then(|out| std::fs::OpenOptions::new().create(true).append(true).open(out).ok());
    if let Some(mut f) = file {
        let _ = writeln!(f, "{line}");
    }
}

fn sandbox() -> PathBuf {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".tmp").join(format!("fileop-probe-{stamp}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).map(|d| crate::win_volume::plain(&d)).unwrap()
}

/// The `$I` / `$R` pair in this volume's `$Recycle.Bin` whose `$I` names `original`.
fn bin_entry(original: &Path) -> Option<(PathBuf, PathBuf)> {
    let root = original.components().next()?.as_os_str().to_string_lossy().into_owned();
    let bin = PathBuf::from(format!(r"{root}\$Recycle.Bin"));
    let want = original.to_string_lossy().to_lowercase();
    for sid in std::fs::read_dir(&bin).ok()?.flatten() {
        let Ok(items) = std::fs::read_dir(sid.path()) else { continue };
        for item in items.flatten() {
            let name = item.file_name().to_string_lossy().into_owned();
            let Some(suffix) = name.strip_prefix("$I") else { continue };
            let Ok(bytes) = std::fs::read(item.path()) else { continue };
            // Version 2 layout: 8 version, 8 size, 8 time, 4 length (chars), then the UTF-16 path.
            if bytes.len() < 28 {
                continue;
            }
            let path: Vec<u16> = bytes[28..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|c| *c != 0).collect();
            if String::from_utf16_lossy(&path).to_lowercase() == want {
                return Some((item.path(), sid.path().join(format!("$R{suffix}"))));
            }
        }
    }
    None
}

/// Take our own entry back out of the bin (that one pair only); true when both are gone.
fn unbin(original: &Path) -> Option<bool> {
    let (info, data) = bin_entry(original)?;
    let _ = std::fs::remove_file(&data);
    let _ = std::fs::remove_file(&info);
    Some(!data.exists() && !info.exists())
}

fn hidden(cmd: &str, args: &[&str]) -> Option<String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new(cmd).args(args).creation_flags(0x0800_0000).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn unused_letter() -> Option<char> {
    ('P'..='Z').rev().find(|l| !Path::new(&format!("{l}:\\")).exists())
}

/// The sink, not a timeout or a dialog, refused: it saw the item and no recycle bit.
fn assert_sink_refused(case: &str, report: &Report, file: &Path) {
    say(format!("PROBE {case}: {report:?}; file exists after: {}", file.exists()));
    assert!(matches!(report.outcome, Outcome::Refused(_)), "{case}: not refused: {report:?}");
    assert!(!report.flags_seen.is_empty() && report.flags_seen.iter().all(|f| f & TSF_DELETE_RECYCLE_IF_POSSIBLE == 0), "{case}: {report:?}");
    assert!(file.exists(), "{case}: the file is gone");
}

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn DeviceIoControl(device: *mut c_void, code: u32, input: *const c_void, input_len: u32, output: *mut c_void, output_len: u32, returned: *mut u32, overlapped: *mut c_void) -> i32;
    fn GetCompressedFileSizeW(name: *const u16, high: *mut u32) -> u32;
}

/// A sparse file of `len` bytes that takes (almost) no disk space; its allocated size.
fn sparse(path: &Path, len: u64) -> Option<u64> {
    const FSCTL_SET_SPARSE: u32 = 0x0009_00C4;
    let file = std::fs::OpenOptions::new().create_new(true).write(true).open(path).ok()?;
    let mut ret = 0u32;
    // SAFETY: a valid open handle; no buffers.
    let ok = unsafe { DeviceIoControl(file.as_raw_handle(), FSCTL_SET_SPARSE, std::ptr::null(), 0, std::ptr::null_mut(), 0, &mut ret, std::ptr::null_mut()) };
    if ok == 0 || file.set_len(len).is_err() {
        return None;
    }
    drop(file);
    let name = wide(path);
    let mut high = 0u32;
    // SAFETY: a NUL-terminated path; `high` is writable.
    let low = unsafe { GetCompressedFileSizeW(name.as_ptr(), &mut high) };
    Some((u64::from(high) << 32) | u64::from(low))
}

/// The probe (run it through `fileop_probe_hidden_desktop`). Cases run in
/// order and each must pass before the next: recycled, subst, admin share,
/// then (only with `RECALL_PROBE_SPARSE=1`) a sparse file bigger than the bin.
#[test]
#[ignore]
fn fileop_live_probe() {
    let dir = sandbox();
    let mut letter_made: Option<String> = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_cases(&dir, &mut letter_made)));
    if let Some(drive) = &letter_made {
        let ok = hidden("subst", &[drive, "/d"]).is_some();
        let list = hidden("subst", &[]).unwrap_or_default();
        say(format!("PROBE cleanup: subst {drive} /d ok: {ok}; subst list now: {:?}", list.trim()));
    }
    let _ = std::fs::remove_dir_all(&dir);
    say(format!("PROBE cleanup: sandbox exists after: {}", dir.exists()));
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    assert!(!dir.exists());
}

fn run_cases(dir: &Path, letter_made: &mut Option<String>) {
    // 1. Recycled: a local fixed volume.
    let file = dir.join("recycle-me.txt");
    std::fs::write(&file, b"recall probe").unwrap();
    let report = recycle_files(std::slice::from_ref(&file));
    say(format!("PROBE 1 recycled: {report:?}; file exists after: {}", file.exists()));
    assert_eq!(report.outcome, Outcome::Recycled);
    assert!(report.flags_seen.iter().all(|f| f & TSF_DELETE_RECYCLE_IF_POSSIBLE != 0) && report.items == (1, 1));
    assert!(!file.exists());
    let unbinned = unbin(&file);
    say(format!("PROBE 1 own bin entry removed: {unbinned:?}"));
    assert_eq!(unbinned, Some(true));

    // 2. Would-nuke: a subst drive onto the sandbox.
    let letter = unused_letter().expect("a free drive letter");
    let drive = format!("{letter}:");
    hidden("subst", &[&drive, &dir.to_string_lossy()]).expect("subst");
    *letter_made = Some(drive.clone());
    let file = PathBuf::from(format!(r"{drive}\nuke-me.txt"));
    std::fs::write(&file, b"recall probe").unwrap();
    let pre = crate::win_recycle::can_recycle(Path::new(&format!(r"{drive}\")), 12);
    say(format!("PROBE 2 subst precheck: {pre:?}"));
    assert!(pre.is_err_and(|e| e.contains("subst")), "the precheck refuses a subst drive");
    assert_sink_refused("2 subst", &recycle_files(std::slice::from_ref(&file)), &file);

    // 3. Would-nuke: the admin share of this volume, if reachable unelevated.
    let text = dir.to_string_lossy().into_owned();
    let share = PathBuf::from(format!(r"\\localhost\{}${}", &text[..1], &text[2..]));
    if share.is_dir() {
        let file = share.join("share-me.txt");
        std::fs::write(&file, b"recall probe").unwrap();
        let pre = crate::win_recycle::can_recycle(&share, 12);
        say(format!("PROBE 3 share precheck: {pre:?}"));
        assert!(pre.is_err(), "the precheck refuses a share");
        assert_sink_refused("3 share", &recycle_files(std::slice::from_ref(&file)), &file);
    } else {
        say(format!("PROBE 3 share: {} not reachable unelevated; skipped", share.display()));
    }

    // 4. Too big for the bin: a sparse file (tiny on disk), only when asked.
    if std::env::var("RECALL_PROBE_SPARSE").as_deref() != Ok("1") {
        say("PROBE 4 sparse: not asked (RECALL_PROBE_SPARSE unset); skipped".into());
        return;
    }
    let facts = crate::win_recycle::facts(dir);
    let max_mb = facts.settings.max_mb.map(u64::from).or_else(|| facts.total_bytes.map(crate::win_recycle::default_max_mb)).expect("bin size");
    let len = (max_mb << 20) + GIB;
    let file = dir.join("too-big.bin");
    let allocated = sparse(&file, len);
    say(format!("PROBE 4 sparse: bin max {max_mb} MB, logical {} bytes, allocated {allocated:?} bytes", len));
    let Some(allocated) = allocated.filter(|a| *a < (1 << 20)) else {
        let _ = std::fs::remove_file(&file);
        say("PROBE 4 sparse: not sparse (allocated >= 1 MiB or failed); removed, skipped".into());
        return;
    };
    let pre = crate::win_recycle::can_recycle(dir, len);
    say(format!("PROBE 4 sparse precheck: {pre:?} (allocated {allocated})"));
    assert!(pre.is_err_and(|e| e.contains("bigger than the Recycle Bin")));
    let report = recycle_files(std::slice::from_ref(&file));
    say(format!("PROBE 4 sparse (shipped flags): {report:?}; file exists after: {}", file.exists()));
    if report.outcome == Outcome::Recycled {
        say(format!("PROBE 4 sparse: Windows recycled it; own bin entry removed: {:?}", unbin(&file)));
    }
    // Refused either by the sink or by the wait (Windows asked first; the launcher lists the dialog).
    assert!(matches!(report.outcome, Outcome::Refused(_)), "{report:?}");
    assert!(file.exists(), "the too-big file is still there");

    // 4b. The same without FOF_WANTNUKEWARNING (the earlier flag set): is the sink asked at all?
    let file_b = dir.join("too-big-b.bin");
    if sparse(&file_b, len).is_some_and(|a| a < (1 << 20)) {
        let report = recycle_with_flags(std::slice::from_ref(&file_b), WAIT, OP_FLAGS & !FOF_WANTNUKEWARNING.0);
        say(format!("PROBE 4b sparse (no nuke warning): {report:?}; file exists after: {}", file_b.exists()));
        if report.outcome == Outcome::Recycled {
            say(format!("PROBE 4b sparse: Windows recycled it; own bin entry removed: {:?}", unbin(&file_b)));
        }
    }
    let _ = (std::fs::remove_file(&file), std::fs::remove_file(&file_b));
}

// ---- The launcher: run the probe on a desktop nobody sees ----

#[repr(C)]
struct StartupInfoW {
    cb: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    dims: [u32; 8],
    show_window: u16,
    reserved2_len: u16,
    reserved2: *mut u8,
    std_in: *mut c_void,
    std_out: *mut c_void,
    std_err: *mut c_void,
}

#[repr(C)]
struct ProcessInfo {
    process: *mut c_void,
    thread: *mut c_void,
    pid: u32,
    tid: u32,
}

#[link(name = "user32", kind = "raw-dylib")]
unsafe extern "system" {
    // Desktop handles as isize, as `record_screen.rs` declares `CloseDesktop` in this crate.
    fn CreateDesktopW(name: *const u16, device: *const u16, devmode: *const c_void, flags: u32, access: u32, sa: *const c_void) -> isize;
    fn CloseDesktop(desktop: isize) -> i32;
    fn EnumDesktopWindows(desktop: isize, callback: unsafe extern "system" fn(*mut c_void, isize) -> i32, param: isize) -> i32;
    fn GetClassNameW(window: *mut c_void, name: *mut u16, max: i32) -> i32;
    fn GetWindowTextW(window: *mut c_void, text: *mut u16, max: i32) -> i32;
}

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn CreateProcessW(app: *const u16, cmd: *mut u16, pa: *const c_void, ta: *const c_void, inherit: i32, flags: u32, env: *const c_void, cwd: *const u16, si: *const StartupInfoW, pi: *mut ProcessInfo) -> i32;
    fn WaitForSingleObject(handle: *mut c_void, millis: u32) -> u32;
    fn GetExitCodeProcess(handle: *mut c_void, code: *mut u32) -> i32;
    fn TerminateProcess(handle: *mut c_void, code: u32) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

unsafe extern "system" fn note_window(window: *mut c_void, param: isize) -> i32 {
    let list = unsafe { &mut *(param as *mut Vec<String>) };
    let (mut class, mut title) = ([0u16; 128], [0u16; 256]);
    let c = unsafe { GetClassNameW(window, class.as_mut_ptr(), 128) }.max(0) as usize;
    let t = unsafe { GetWindowTextW(window, title.as_mut_ptr(), 256) }.max(0) as usize;
    list.push(format!("{} \"{}\"", String::from_utf16_lossy(&class[..c]), String::from_utf16_lossy(&title[..t])));
    1
}

fn wide_str(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[test]
#[ignore]
#[allow(unused_unsafe)]
fn fileop_probe_hidden_desktop() {
    const GENERIC_ALL: u32 = 0x1000_0000;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let name = format!("RecallProbe{}", std::process::id());
    let desk_name = wide_str(&name);
    // SAFETY: a NUL-terminated name; default device and security.
    let desk = unsafe { CreateDesktopW(desk_name.as_ptr(), std::ptr::null(), std::ptr::null(), 0, GENERIC_ALL, std::ptr::null()) };
    assert_ne!(desk, 0, "CreateDesktopW failed");
    let out = sandbox_out();
    // SAFETY (env): this launcher runs alone (`--ignored`, exact filter); the child inherits it.
    unsafe { std::env::set_var("RECALL_PROBE_OUT", &out) };
    let exe = std::env::current_exe().unwrap();
    let mut cmd = wide_str(&format!("\"{}\" win_fileop::tests::probe::fileop_live_probe --exact --ignored --nocapture --test-threads=1", exe.display()));
    let mut desktop = wide_str(&format!(r"WinSta0\{name}"));
    let si = StartupInfoW {
        cb: std::mem::size_of::<StartupInfoW>() as u32,
        reserved: std::ptr::null_mut(),
        desktop: desktop.as_mut_ptr(),
        title: std::ptr::null_mut(),
        dims: [0; 8],
        show_window: 0,
        reserved2_len: 0,
        reserved2: std::ptr::null_mut(),
        std_in: std::ptr::null_mut(),
        std_out: std::ptr::null_mut(),
        std_err: std::ptr::null_mut(),
    };
    let mut pi = ProcessInfo { process: std::ptr::null_mut(), thread: std::ptr::null_mut(), pid: 0, tid: 0 };
    // SAFETY: valid NUL-terminated strings and structs alive for the call.
    let started = unsafe { CreateProcessW(std::ptr::null(), cmd.as_mut_ptr(), std::ptr::null(), std::ptr::null(), 0, CREATE_NO_WINDOW, std::ptr::null(), std::ptr::null(), &si, &mut pi) };
    assert_ne!(started, 0, "CreateProcessW failed");
    // Wait up to 4 minutes, listing the hidden desktop's windows every second while the probe runs.
    let mut windows: Vec<String> = Vec::new();
    let mut waited = 1;
    for _ in 0..240 {
        // SAFETY: a valid process handle.
        waited = unsafe { WaitForSingleObject(pi.process, 1000) };
        let mut now: Vec<String> = Vec::new();
        // SAFETY: the callback only reads window names into `now`.
        unsafe { EnumDesktopWindows(desk, note_window, &mut now as *mut Vec<String> as isize) };
        for w in now {
            if !windows.contains(&w) {
                windows.push(w);
            }
        }
        if waited == 0 {
            break;
        }
    }
    let mut code = 0u32;
    if waited != 0 {
        // SAFETY: our own child process.
        unsafe { TerminateProcess(pi.process, 1) };
    }
    // SAFETY: valid handles from CreateProcessW / CreateDesktopW.
    unsafe {
        GetExitCodeProcess(pi.process, &mut code);
        CloseHandle(pi.thread);
        CloseHandle(pi.process);
        CloseDesktop(desk);
    }
    let lines = std::fs::read_to_string(&out).unwrap_or_default();
    let _ = std::fs::remove_file(&out);
    println!("{lines}");
    println!("PROBE launcher: child exit code {code} (timed out: {}); windows on the hidden desktop: {} {windows:?}", waited != 0, windows.len());
    assert_eq!(waited, 0, "the probe did not finish in 4 minutes");
    assert_eq!(code, 0, "the probe failed (see the PROBE lines)");
}

fn sandbox_out() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".tmp");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("fileop-probe-{}.out", std::process::id()))
}
