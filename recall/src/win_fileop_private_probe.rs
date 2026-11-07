//! Live probe of the private desktop (ignored by default). Two stages:
//!
//! 1. Safety stage, the whole probe on a hidden desktop:
//!    `cargo test -p recall-hdr --offline fileop_private_probe_hidden_desktop -- --ignored --nocapture`
//! 2. Only after stage 1 showed the dialog on the PRIVATE desktop, the same
//!    probe from the normal interactive desktop, so only the worker is moved:
//!    `cargo test -p recall-hdr --offline fileop_private_probe -- --ignored --exact --nocapture`
//!    (filter `win_fileop::tests::probe::private::fileop_private_probe`).
//!
//! Sandbox sparse files only (0 bytes on disk); everything made is removed.

use super::*;
use std::sync::atomic::AtomicBool as Flag;

const GIB: u64 = 1 << 30;
const DESKTOP_READOBJECTS: u32 = 0x0001;
const BM_CLICK: u32 = 0x00F5;

#[link(name = "user32", kind = "raw-dylib")]
unsafe extern "system" {
    // Same signature as record_screen.rs (recall-hdr) declares.
    fn OpenInputDesktop(flags: u32, inherit: i32, access: u32) -> isize;
    fn OpenDesktopW(name: *const u16, flags: u32, inherit: i32, access: u32) -> isize;
    fn EnumChildWindows(parent: *mut c_void, callback: unsafe extern "system" fn(*mut c_void, isize) -> i32, param: isize) -> i32;
    fn PostMessageW(window: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> i32;
}

/// One window: (handle, class, title).
type Win = (isize, String, String);

unsafe extern "system" fn collect(window: *mut c_void, param: isize) -> i32 {
    let list = unsafe { &mut *(param as *mut Vec<Win>) };
    let (mut class, mut title) = ([0u16; 128], [0u16; 256]);
    let c = unsafe { GetClassNameW(window, class.as_mut_ptr(), 128) }.max(0) as usize;
    let t = unsafe { GetWindowTextW(window, title.as_mut_ptr(), 256) }.max(0) as usize;
    list.push((window as isize, String::from_utf16_lossy(&class[..c]), String::from_utf16_lossy(&title[..t])));
    1
}

fn windows_of(desk: isize) -> Vec<Win> {
    let mut list: Vec<Win> = Vec::new();
    if desk != 0 {
        // SAFETY: the callback only reads window names into `list`.
        unsafe { EnumDesktopWindows(desk, collect, &mut list as *mut Vec<Win> as isize) };
    }
    list
}

/// Top-level dialogs (`#32770`) on the input desktop (`None`: it cannot be opened, e.g. locked).
fn input_dialogs() -> Option<Vec<Win>> {
    // SAFETY: read-only access; closed below.
    let desk = unsafe { OpenInputDesktop(0, 0, DESKTOP_READOBJECTS) };
    if desk == 0 {
        return None;
    }
    let found = windows_of(desk).into_iter().filter(|w| w.1 == "#32770").collect();
    // SAFETY: opened above.
    unsafe { CloseDesktop(desk) };
    Some(found)
}

/// Every window on the named desktop.
fn named_windows(name: &str) -> Vec<Win> {
    let wide = wide_str(name);
    // SAFETY: a NUL-terminated name; read-only access; closed below.
    let desk = unsafe { OpenDesktopW(wide.as_ptr(), 0, 0, DESKTOP_READOBJECTS) };
    let found = windows_of(desk);
    if desk != 0 {
        // SAFETY: opened above.
        unsafe { CloseDesktop(desk) };
    }
    found
}

/// Watch the private desktop of the next call for its "Delete File" dialog
/// for up to 15 s; with `answer_yes`, click its Yes button once. Returns what was seen.
fn watch(answer_yes: bool, stop: Arc<Flag>) -> std::thread::JoinHandle<(Vec<Win>, Vec<String>, bool)> {
    std::thread::spawn(move || {
        let (mut seen, mut buttons, mut clicked) = (Vec::new(), Vec::new(), false);
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while std::time::Instant::now() < deadline && !stop.load(Ordering::SeqCst) {
            let name = LAST_DESKTOP.lock().unwrap().clone();
            for w in named_windows(&name).into_iter().filter(|w| w.1 == "#32770") {
                if !seen.contains(&w) {
                    seen.push(w.clone());
                }
                if answer_yes && !clicked {
                    let mut kids: Vec<Win> = Vec::new();
                    // SAFETY: the callback only reads window names into `kids`.
                    unsafe { EnumChildWindows(w.0 as *mut c_void, collect, &mut kids as *mut Vec<Win> as isize) };
                    buttons = kids.iter().filter(|k| k.1 == "Button").map(|k| k.2.clone()).collect();
                    if let Some(yes) = kids.iter().find(|k| k.1 == "Button" && k.2.replace('&', "").eq_ignore_ascii_case("yes")) {
                        // SAFETY: a button handle enumerated just now.
                        clicked = unsafe { PostMessageW(yes.0 as *mut c_void, BM_CLICK, 0, 0) } != 0;
                    }
                }
            }
            if !seen.is_empty() && (!answer_yes || clicked) {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        (seen, buttons, clicked)
    })
}

/// Run on purpose (see the module doc). Cases in order, each must pass first.
#[test]
#[ignore]
fn fileop_private_probe() {
    let dir = sandbox();
    let before = input_dialogs();
    say(format!("PRIVATE input-desktop dialogs before: {before:?}"));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| private_cases(&dir, &before)));
    let _ = std::fs::remove_dir_all(&dir);
    say(format!("PRIVATE cleanup: sandbox exists after: {} (a stuck worker may still hold a file until the process exits)", dir.exists()));
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

fn no_new_input_dialog(before: &Option<Vec<Win>>, case: &str) {
    let after = input_dialogs();
    say(format!("PRIVATE {case}: input-desktop dialogs now: {after:?}"));
    if let (Some(before), Some(after)) = (before, &after) {
        let new: Vec<&Win> = after.iter().filter(|w| !before.contains(w)).collect();
        assert!(new.is_empty(), "{case}: a dialog appeared on the input desktop: {new:?}");
    }
}

fn private_cases(dir: &Path, before: &Option<Vec<Win>>) {
    // 1. Recycling still works from the private desktop.
    let file = dir.join("recycle-me.txt");
    std::fs::write(&file, b"recall probe").unwrap();
    let report = recycle_files(std::slice::from_ref(&file));
    let desk = LAST_DESKTOP.lock().unwrap().clone();
    say(format!("PRIVATE 1 recycled on {desk}: {report:?}; file exists after: {}", file.exists()));
    assert_eq!(report.outcome, Outcome::Recycled);
    assert!(desk.starts_with("RecallRecycle") && !file.exists());
    assert_eq!(unbin(&file), Some(true), "own bin entry removed");

    let len = 120 * GIB; // more than L:'s 97,434 MB bin
    // 2. The nuke dialog, answered Yes within the wait: what does the sink see?
    let f1 = dir.join("too-big-yes.bin");
    assert!(sparse(&f1, len).is_some_and(|a| a < (1 << 20)), "sparse");
    let stop = Arc::new(Flag::new(false));
    let watcher = watch(true, stop.clone());
    let report = recycle_files(std::slice::from_ref(&f1));
    stop.store(true, Ordering::SeqCst);
    let (seen, buttons, clicked) = watcher.join().unwrap();
    say(format!("PRIVATE 2 Yes: dialogs on the private desktop {seen:?}; buttons {buttons:?}; clicked Yes: {clicked}"));
    say(format!("PRIVATE 2 Yes: {report:?}; file exists after: {}; bin entry: {:?}", f1.exists(), bin_entry(&f1)));
    assert!(seen.iter().any(|w| w.2 == "Delete File"), "the dialog was on the private desktop");
    no_new_input_dialog(before, "2");
    if bin_entry(&f1).is_some() {
        say(format!("PRIVATE 2 Yes: own bin entry removed: {:?}", unbin(&f1)));
    }

    // 3. The same dialog left unanswered: the wait refuses and the file stays.
    let f2 = dir.join("too-big-wait.bin");
    assert!(sparse(&f2, len).is_some_and(|a| a < (1 << 20)), "sparse");
    let stop = Arc::new(Flag::new(false));
    let watcher = watch(false, stop.clone());
    let report = recycle_files(std::slice::from_ref(&f2));
    stop.store(true, Ordering::SeqCst);
    let (seen, _, _) = watcher.join().unwrap();
    say(format!("PRIVATE 3 unanswered: dialogs on the private desktop {seen:?}; {report:?}; file exists after: {}", f2.exists()));
    assert!(matches!(&report.outcome, Outcome::Refused(why) if why.contains("did not finish")), "{report:?}");
    assert!(f2.exists(), "the too-big file stays");
    assert!(seen.iter().any(|w| w.2 == "Delete File"), "the dialog was on the private desktop");
    no_new_input_dialog(before, "3");

    // 4. While that worker sits on its dialog, the next call is refused at once and starts no thread.
    let f3 = dir.join("after-stuck.txt");
    std::fs::write(&f3, b"recall probe").unwrap();
    let threads = SPAWNED.load(Ordering::SeqCst);
    let report = recycle_files(std::slice::from_ref(&f3));
    say(format!("PRIVATE 4 while stuck: {report:?}; threads started: {}; file exists after: {}", SPAWNED.load(Ordering::SeqCst) - threads, f3.exists()));
    assert_eq!(report.outcome, Outcome::Refused(STUCK_REFUSAL.into()));
    assert!(f3.exists() && SPAWNED.load(Ordering::SeqCst) == threads);
}

#[test]
#[ignore]
fn fileop_private_probe_hidden_desktop() {
    run_on_hidden_desktop("win_fileop::tests::probe::private::fileop_private_probe");
}
