//! What the storage cap keeps besides favourites and edits (played, open), re-read before
//! every call, and the cross-process Recycle Bin lock around the check and the call.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::Mutex;

const NOW: u64 = 1_000_000;
const A: &str = "G__2026-01-01__00-00-00.mp4";
const B: &str = "G__2026-01-01__00-01-00.mp4";
const C: &str = "G__2026-01-01__00-02-00.mp4";

fn clip(name: &str, modified: u64) -> ClipFile {
    ClipFile { name: name.into(), bytes: GIB, modified }
}

fn three() -> Listing {
    let clips = vec![clip(A, 100), clip(B, 200), clip(C, 300), clip("G__2026-01-02__00-00-00.mp4", 400)];
    let names = clips.iter().map(|c| c.name.clone()).collect();
    Listing { clips, names, thumbs: Vec::new() }
}

fn temp_dir(tag: &str) -> PathBuf {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("recall-keep-{tag}-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_clip_played_in_the_last_ten_minutes_is_not_recycled() {
    let library = format!(r#"{{"{A}":{{"played":{}}},"{B}":{{"played":{}}}}}"#, NOW - 60, NOW - 601);
    let keep = keep::played_recently(library.as_bytes(), NOW);
    let planned = plan(&three(), &keep, GIB, NOW);
    assert!(!planned.remove.contains(&A.to_string()), "played a minute ago: kept: {planned:?}");
    assert!(planned.remove.contains(&B.to_string()), "played eleven minutes ago: may go: {planned:?}");
}

#[test]
fn a_clip_open_in_the_app_is_kept_and_an_unreadable_open_list_stops_the_run() {
    let dir = temp_dir("open");
    let library = dir.join("library.json");
    std::fs::write(&library, b"{}").unwrap();
    assert!(keep::recent_and_open(&library, NOW).unwrap().is_empty(), "no open-clips.json: nothing open");
    let open = keep::open_clips_path(&library);
    std::fs::write(&open, format!(r#"{{"paths":["E:\\Clips\\{C}"]}}"#)).unwrap();
    let kept = keep::recent_and_open(&library, NOW).unwrap();
    assert!(kept.contains(&C.to_lowercase()), "{kept:?}");
    let planned = plan(&three(), &kept, GIB, NOW);
    assert!(!planned.remove.contains(&C.to_string()), "{planned:?}");
    std::fs::write(&open, b"\0\0\0").unwrap();
    assert!(keep::recent_and_open(&library, NOW).is_err(), "a zero-filled list is unknown, not empty");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn favourites_keyed_by_full_path_still_protect_the_clip() {
    let favs = favourites(format!(r#"{{"E:\\Clips\\{A}":{{"favourite":true}}}}"#).as_bytes()).unwrap();
    assert!(favs.contains(&A.to_lowercase()), "{favs:?}");
}

fn plan_three(_: &Path, _: u32) -> Result<(Plan, Listing), String> {
    let listing = three();
    Ok((Plan { remove: vec![A.into(), B.into(), C.into()], freed: 3 * GIB, total: 600 * GIB, through: Some(300), next: None }, listing))
}

fn any_bin(_: &Path, _: u64) -> Result<(), String> {
    Ok(())
}

fn no_files(_: &Path) -> Option<u64> {
    Some(0)
}

static STAR_CALLS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static STARRED: AtomicBool = AtomicBool::new(false);

/// The user stars B right after A went.
fn recycle_then_star(files: &[PathBuf]) -> Result<(), RecycleError> {
    STAR_CALLS.lock().unwrap().push(files[0].file_name().unwrap().to_string_lossy().into_owned());
    STARRED.store(true, SeqCst);
    Ok(())
}

fn keeps_b_once_starred() -> Result<HashSet<String>, String> {
    Ok(if STARRED.load(SeqCst) { [B.to_lowercase()].into_iter().collect() } else { HashSet::new() })
}

#[test]
fn a_clip_starred_during_the_run_is_not_recycled() {
    let out = run_once(Path::new(r"E:\Clips"), 500, plan_three, (&|| true, keeps_b_once_starred), (any_bin, no_files), recycle_then_star).unwrap();
    assert_eq!(STAR_CALLS.lock().unwrap().clone(), vec![A.to_string(), C.to_string()], "B was starred before its call");
    assert_eq!(out.removed, 2);
}

static UNKNOWN_CALLS: AtomicUsize = AtomicUsize::new(0);

fn recycle_counted(_: &[PathBuf]) -> Result<(), RecycleError> {
    UNKNOWN_CALLS.fetch_add(1, SeqCst);
    Ok(())
}

#[test]
fn keeps_that_cannot_be_read_stop_the_run() {
    let unknown: KeepFn = || Err("library.json: unreadable".into());
    let out = run_once(Path::new(r"E:\Clips"), 500, plan_three, (&|| true, unknown), (any_bin, no_files), recycle_counted).unwrap();
    assert_eq!((UNKNOWN_CALLS.load(SeqCst), out.removed), (0, 0), "nothing moved while favourites are unknown");
}

static LOCK_SEEN: Mutex<Vec<bool>> = Mutex::new(Vec::new());

/// Can another thread (standing in for the app's Delete) take the Recycle Bin lock now?
fn lock_free_elsewhere() -> bool {
    std::thread::spawn(|| crate::win_fileop::bin_lock::acquire(crate::win_fileop::bin_lock::NAME, Duration::ZERO).is_ok()).join().unwrap()
}

fn bin_records_lock(_: &Path, _: u64) -> Result<(), String> {
    LOCK_SEEN.lock().unwrap().push(lock_free_elsewhere());
    Ok(())
}

fn recycle_records_lock(_: &[PathBuf]) -> Result<(), RecycleError> {
    LOCK_SEEN.lock().unwrap().push(lock_free_elsewhere());
    Ok(())
}

#[test]
fn the_bin_check_and_the_move_run_under_the_shared_recycle_lock() {
    let none: KeepFn = || Ok(HashSet::new());
    run_once(Path::new(r"E:\Clips"), 500, plan_three, (&|| true, none), (bin_records_lock, no_files), recycle_records_lock).unwrap();
    assert_eq!(LOCK_SEEN.lock().unwrap().clone(), vec![false; 6], "held across every check and every call");
}
