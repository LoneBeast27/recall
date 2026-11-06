//! Free-space threshold, once-per-crossing warnings and drive labels (injected free space and volumes).

use super::*;
use crate::record_storage::GIB;
use crate::win_volume::Volume;
use serde_json::Value;

const MIB: u64 = 1 << 20;
const FIVE_GB: u64 = 5_000_000_000;

fn drives() -> Vec<(String, PathBuf)> {
    vec![("E:".into(), PathBuf::from(r"E:\Clips")), ("F:".into(), PathBuf::from(r"F:\Recall\cache\staging"))]
}

/// Labels from the path text (case-folded), never touching a disk.
fn by_letter(dir: &Path) -> Option<Volume> {
    let label = dir.to_string_lossy()[..2].to_ascii_uppercase();
    Some(Volume { id: label.clone(), label })
}

#[test]
fn threshold_is_twice_the_buffer_but_never_under_5_gb() {
    // 120 s at 25 Mbit/s = 375 MB: the floor wins, a decimal 5 GB (the app shows "5.00 GB").
    assert_eq!(buffer_bytes(120, 25), 375_000_000);
    assert_eq!(threshold(buffer_bytes(120, 25)), FIVE_GB);
    // 600 s at 80 Mbit/s = 6 GB: twice that wins.
    assert_eq!(threshold(buffer_bytes(600, 80)), 12_000_000_000);
    assert_eq!(threshold(0), FREE_FLOOR);
}

#[test]
fn warns_once_per_crossing_and_rearms_when_space_recovers() {
    let mut w = FreeWarner::default();
    let (d, t) = (drives(), FIVE_GB);
    let low_e: FreeFn = |d| Some(if d.starts_with("E:") { 4 * GIB } else { 100 * GIB });
    assert_eq!(w.update(&d, t, low_e), vec![("E:".to_string(), 4 * GIB)]);
    assert!(w.update(&d, t, low_e).is_empty(), "no repeat every tick");
    // Back just over the threshold: still inside the re-arm margin, no new warning later.
    let just_over: FreeFn = |_| Some(5_000_000_000 + 10 * MIB);
    assert!(w.update(&d, t, just_over).is_empty());
    assert!(w.update(&d, t, low_e).is_empty(), "not re-armed yet");
    // Recovered past the margin: re-armed, the next drop warns again.
    let recovered: FreeFn = |_| Some(7 * GIB);
    assert!(w.update(&d, t, recovered).is_empty());
    assert_eq!(w.update(&d, t, low_e), vec![("E:".to_string(), 4 * GIB)]);
    // Both short: each drive once; unknown answers never warn.
    let mut w = FreeWarner::default();
    assert_eq!(w.update(&d, t, |_| Some(MIB)).len(), 2);
    assert!(FreeWarner::default().update(&d, t, |_| None).is_empty());
}

#[test]
fn the_check_uses_the_max_rule_and_reads_each_volume_once() {
    let c = FreeCheck::new(Path::new(r"E:\Clips"), Some(Path::new(r"F:\Recall\cache\staging")), buffer_bytes(600, 80));
    assert_eq!(c.threshold, 12_000_000_000);
    let found = resolve_drives(&c.dirs, by_letter);
    assert_eq!(found.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(), vec!["E:", "F:"]);
    let same = FreeCheck::new(Path::new(r"e:\Clips"), Some(Path::new(r"E:\Recall\cache\staging")), 0);
    assert_eq!(resolve_drives(&same.dirs, by_letter).len(), 1, "staging on the clip drive is one volume");
    assert_eq!(same.threshold, FIVE_GB);
    // 6 GiB free on a 12 GB threshold warns even though it is over the 5 GB floor.
    assert_eq!(FreeWarner::default().update(&found, c.threshold, |_| Some(6 * GIB)).len(), 2);
}

#[test]
fn unresolved_folders_are_named_from_the_path_and_the_engine_staging_folder() {
    let named = resolve_drives(&[PathBuf::from(r"v:\Clips"), PathBuf::from(r"\\NAS\Clips\x")], |_| None);
    assert_eq!(named.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(), vec!["V:", r"\\nas\clips"]);
    let engine = Path::new(r"F:\Programs\Recall\recall.exe");
    assert_eq!(staging_dir(None, Some(engine)), Some(PathBuf::from(r"F:\Programs\Recall\cache\staging")));
    assert_eq!(staging_dir(Some(r"D:\stage"), Some(engine)), Some(PathBuf::from(r"D:\stage")));
    assert_eq!(staging_dir(Some("OFF"), Some(engine)), None);
    assert_eq!(staging_dir(None, None), None);
}

#[test]
fn free_event_shape() {
    let v: Value = serde_json::from_str(&free_event("C:", 3 * GIB, FIVE_GB)).unwrap();
    assert_eq!(v, serde_json::json!({"type":"storage_warning","level":"free","drive":"C:","free_bytes":3 * GIB,"need_bytes":FIVE_GB}));
}

#[test]
fn real_free_space_answers_for_a_folder_not_made_yet() {
    let missing = std::env::temp_dir().join("recall-free-space-not-made").join("deeper");
    assert!(free_bytes(&missing).is_some_and(|b| b > 0), "asks the nearest existing folder");
}

/// A folder in this checkout (L: here) reached through a junction under %TEMP% (C: here).
fn junction_pair() -> (PathBuf, PathBuf) {
    use std::os::windows::process::CommandExt;
    let id = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".tmp").join(format!("hdr-junction-target-{id}"));
    std::fs::create_dir_all(&target).unwrap();
    let link = std::env::temp_dir().join(format!("recall-hdr-junction-{id}"));
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(&target)
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "mklink /J failed");
    (link, std::fs::canonicalize(&target).unwrap())
}

#[test]
fn junctioned_clip_and_staging_folders_are_checked_on_their_real_volume() {
    let (link, target) = junction_pair();
    // The staging folder through the junction is the clip folder's volume: read once, named by where it really is.
    let found = resolve_drives(&[target.clone(), link.clone()], crate::win_volume::volume_of);
    let real = crate::win_volume::volume_of(&target).map(|v| v.label);
    let _ = std::fs::remove_dir(&link); // the junction only
    let _ = std::fs::remove_dir_all(&target);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(Some(found[0].0.clone()), real);
}
