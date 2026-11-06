//! Free-space threshold, once-per-crossing warnings and drive labels (injected free space).

use super::*;
use serde_json::Value;

const MIB: u64 = 1 << 20;

fn check(threshold: u64) -> FreeCheck {
    FreeCheck { drives: vec![("E:".into(), PathBuf::from(r"E:\Clips")), ("F:".into(), PathBuf::from(r"F:\Recall\cache\staging"))], threshold }
}

#[test]
fn threshold_is_twice_the_buffer_but_never_under_5_gib() {
    // 120 s at 25 Mbit/s = 375 MB: the floor wins.
    assert_eq!(buffer_bytes(120, 25), 375_000_000);
    assert_eq!(threshold(buffer_bytes(120, 25)), 5 * GIB);
    // 600 s at 80 Mbit/s = 6 GB: twice that wins.
    assert_eq!(threshold(buffer_bytes(600, 80)), 12_000_000_000);
    assert_eq!(threshold(0), FREE_FLOOR);
}

#[test]
fn warns_once_per_crossing_and_rearms_when_space_recovers() {
    let mut w = FreeWarner::default();
    let t = 5 * GIB;
    let low_e: FreeFn = |d| Some(if d.starts_with("E:") { 4 * GIB } else { 100 * GIB });
    assert_eq!(w.update(&check(t), low_e), vec![("E:".to_string(), 4 * GIB)]);
    assert!(w.update(&check(t), low_e).is_empty(), "no repeat every tick");
    // Back just over the threshold: still inside the re-arm margin, no new warning later.
    let just_over: FreeFn = |_| Some(5 * GIB + 10 * MIB);
    assert!(w.update(&check(t), just_over).is_empty());
    assert!(w.update(&check(t), low_e).is_empty(), "not re-armed yet");
    // Recovered past the margin: re-armed, the next drop warns again.
    let recovered: FreeFn = |_| Some(7 * GIB);
    assert!(w.update(&check(t), recovered).is_empty());
    assert_eq!(w.update(&check(t), low_e), vec![("E:".to_string(), 4 * GIB)]);
    // Both short: each drive once; unknown answers never warn.
    let mut w = FreeWarner::default();
    assert_eq!(w.update(&check(t), |_| Some(MIB)).len(), 2);
    assert!(FreeWarner::default().update(&check(t), |_| None).is_empty());
}

#[test]
fn the_check_uses_the_max_rule_and_reads_each_volume_once() {
    let c = FreeCheck::new(Path::new(r"E:\Clips"), Some(Path::new(r"F:\Recall\cache\staging")), buffer_bytes(600, 80));
    assert_eq!(c.threshold, 12_000_000_000);
    assert_eq!(c.drives.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(), vec!["E:", "F:"]);
    let same = FreeCheck::new(Path::new(r"e:\Clips"), Some(Path::new(r"E:\Recall\cache\staging")), 0);
    assert_eq!(same.drives.len(), 1, "staging on the clip drive is one volume");
    assert_eq!(same.threshold, 5 * GIB);
    assert_eq!(FreeCheck::new(Path::new(r"E:\Clips"), None, 0).drives.len(), 1);
    // 6 GiB free on a 12 GB threshold warns even though it is over the 5 GiB floor.
    assert_eq!(FreeWarner::default().update(&c, |_| Some(6 * GIB)).len(), 2);
}

#[test]
fn drive_labels_and_the_engine_staging_folder() {
    assert_eq!(drive_label(Path::new(r"v:\Clips")), "V:");
    assert_eq!(drive_label(Path::new(r"\\NAS\Clips\x")), r"\\nas\clips");
    let engine = Path::new(r"F:\Programs\Recall\recall.exe");
    assert_eq!(staging_dir(None, Some(engine)), Some(PathBuf::from(r"F:\Programs\Recall\cache\staging")));
    assert_eq!(staging_dir(Some(r"D:\stage"), Some(engine)), Some(PathBuf::from(r"D:\stage")));
    assert_eq!(staging_dir(Some("OFF"), Some(engine)), None);
    assert_eq!(staging_dir(None, None), None);
}

#[test]
fn free_event_shape() {
    let v: Value = serde_json::from_str(&free_event("C:", 3 * GIB, 5 * GIB)).unwrap();
    assert_eq!(v, serde_json::json!({"type":"storage_warning","level":"free","drive":"C:","free_bytes":3 * GIB,"need_bytes":5 * GIB}));
}

#[test]
fn real_free_space_answers_for_a_folder_not_made_yet() {
    let missing = std::env::temp_dir().join("recall-free-space-not-made").join("deeper");
    assert!(free_bytes(&missing).is_some_and(|b| b > 0), "asks the nearest existing folder");
}
