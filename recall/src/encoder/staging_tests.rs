//! Staging folder choice, the volume check and start-up recovery (temp dirs only).

use super::*;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("recall-staging-{tag}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn staging_folder_comes_from_the_env_or_the_exe_folder() {
    let exe = Path::new(r"L:\Programs\Recall\recall.exe");
    let default = PathBuf::from(r"L:\Programs\Recall\cache\staging");
    assert_eq!(configured(None, Some(exe)), Some(default.clone()));
    assert_eq!(configured(Some("  "), Some(exe)), Some(default.clone()), "blank = default");
    assert_eq!(configured(Some(r"D:\fast\stage"), Some(exe)), Some(PathBuf::from(r"D:\fast\stage")));
    assert_eq!(configured(Some("off"), Some(exe)), None);
    assert_eq!(configured(Some(" OFF "), Some(exe)), None);
    assert_eq!(configured(None, None), None, "no exe path, no default");
    // Off still recovers what an earlier run left in the default folder.
    assert_eq!(recovery_dir(Some("off"), Some(exe)), Some(default));
    assert_eq!(recovery_dir(Some(r"D:\s"), Some(exe)), Some(PathBuf::from(r"D:\s")));
}

#[test]
fn volumes_compare_by_drive_letter_or_share() {
    let same = |a: &str, b: &str| same_volume(Path::new(a), Path::new(b));
    assert!(!same(r"L:\Programs\Recall\cache\staging", r"V:\ALL RECORDINGS"));
    assert!(same(r"l:\a", r"L:\b\c"), "drive letters are case-insensitive");
    assert!(same(r"\\?\V:\x", r"V:\ALL RECORDINGS"), "verbatim and plain forms match");
    assert!(same(r"\\nas\clips\a", r"\\NAS\Clips\b"));
    assert!(!same(r"\\nas\clips\a", r"\\nas\other\a"));
    assert!(!same(r"\\nas\clips", r"V:\clips"));
}

#[test]
fn a_save_is_staged_only_across_volumes() {
    let stage = Path::new(r"L:\Recall\cache\staging");
    let clip = Path::new(r"V:\ALL RECORDINGS\G__2026-10-23__10-00-00-000__ab.mp4");
    assert_eq!(
        staged_path(stage, clip, same_volume),
        Some(PathBuf::from(r"L:\Recall\cache\staging\G__2026-10-23__10-00-00-000__ab.mp4"))
    );
    assert_eq!(staged_path(stage, Path::new(r"L:\clips\G.mp4"), same_volume), None, "same volume: in place");
    assert_eq!(staged_path(stage, Path::new(r"V:\"), same_volume), None, "no file name");
}

#[test]
fn recovery_queues_finished_clips_and_deletes_only_stale_mp4_parts() {
    let (stage, out) = (temp_dir("recover"), PathBuf::from(r"V:\never-touched"));
    let older = stage.join("A__2026-10-22__09-00-00-000__aa.mp4");
    std::fs::write(&older, b"a").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let newer = stage.join("B__2026-10-22__09-05-00-000__bb.MP4");
    std::fs::write(&newer, b"b").unwrap();
    let part = stage.join("C__2026-10-22__09-06-00-000__cc.mp4.part");
    std::fs::write(&part, b"half").unwrap();
    std::fs::write(stage.join("notes.txt"), b"keep").unwrap();
    std::fs::write(stage.join("other.part"), b"keep").unwrap();
    std::fs::create_dir_all(stage.join("sub.mp4")).unwrap();

    let found = recover(&stage, &out);
    assert_eq!(
        found.moves,
        vec![
            (older.clone(), out.join("A__2026-10-22__09-00-00-000__aa.mp4")),
            (newer.clone(), out.join("B__2026-10-22__09-05-00-000__bb.MP4")),
        ],
        "oldest first"
    );
    assert_eq!(found.removed, vec![part.clone()]);
    assert!(found.errors.is_empty(), "{:?}", found.errors);
    assert!(!part.exists());
    for kept in ["notes.txt", "other.part", "sub.mp4"] {
        assert!(stage.join(kept).exists(), "{kept} must be left alone");
    }
    assert!(older.exists() && newer.exists(), "recovery only queues; the mover moves");
    assert!(recover(&stage.join("missing"), &out).moves.is_empty());
    let _ = std::fs::remove_dir_all(&stage);
}

#[test]
fn recovery_never_requeues_copies_kept_for_the_app() {
    let (stage, out) = (temp_dir("kept"), PathBuf::from(r"V:
ever-touched"));
    let kept_dir = stage.join(super::mover::KEPT_DIR);
    std::fs::create_dir_all(&kept_dir).unwrap();
    let kept = kept_dir.join("A__2026-10-22__09-00-00-000__aa.mp4");
    std::fs::write(&kept, b"moved already").unwrap();
    let part = kept_dir.join("B__2026-10-22__09-01-00-000__bb.mp4.part");
    std::fs::write(&part, b"not ours").unwrap();
    let found = recover(&stage, &out);
    assert!(found.moves.is_empty(), "{:?}", found.moves);
    assert!(found.removed.is_empty() && found.errors.is_empty());
    assert!(kept.exists() && part.exists(), "kept copies are the app's to adopt or delete");
    let _ = std::fs::remove_dir_all(&stage);
}

#[test]
fn an_uncreatable_staging_folder_falls_back_to_writing_in_place() {
    let dir = temp_dir("blocked");
    let file = dir.join("not-a-folder");
    std::fs::write(&file, b"x").unwrap();
    let (events, sink) = Events::capture(true);
    let cfg = mover::Config { background: false, ..mover::Config::default() };
    let (queue, control) = mover::spawn(events.clone(), cfg, Vec::new()).unwrap();
    let stage = Stage { dir: file.join("staging"), queue, same_volume: |_, _| false };
    assert_eq!(stage.target(r"V:\c\G.mp4", &events), None);
    let logs = sink.lock().unwrap().clone();
    assert!(logs.iter().any(|l| l.contains("writing the clip in place")), "{logs:?}");
    let ok = Stage { dir: dir.join("staging"), ..stage };
    assert_eq!(ok.target(r"V:\c\G.mp4", &events), Some(dir.join("staging").join("G.mp4")));
    assert!(dir.join("staging").is_dir());
    assert!(control.shutdown(std::time::Duration::from_secs(1)));
    let _ = std::fs::remove_dir_all(&dir);
}
