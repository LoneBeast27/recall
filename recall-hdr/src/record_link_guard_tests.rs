use super::*;

fn links(linked: &[&str], owned: &[&str]) -> Links {
    Links { linked: linked.iter().map(|s| s.to_string()).collect(), owned: owned.iter().map(|s| s.to_string()).collect() }
}

fn cfg_at(dir: &str) -> Config {
    let mut cfg = Config::default();
    cfg.recording.output_dir = dir.into();
    cfg
}

#[test]
fn an_output_dir_on_a_linked_folder_falls_back_to_the_last_good_one() {
    // (A) output_dir hand-set to the linked Moments folder: the old folder is kept.
    let l = links(&[r"D:\SteelSeries\Moments"], &[r"D:\Old"]);
    let mut cfg = cfg_at(r"d:\steelseries\moments\");
    let notice = guard(&mut cfg, Some(r"D:\Old"), &l).expect("refused with a notice");
    assert_eq!(cfg.recording.output_dir, r"D:\Old");
    assert!(notice.contains("Recall won't record into it"), "{notice}");
    // A folder holding a link is refused as well.
    assert!(refusal(r"D:\SteelSeries", &l).is_some());
}

#[test]
fn a_linked_parent_keeps_the_owned_recordings_folder_and_refuses_the_rest() {
    // (B) Videos linked with recordings in Videos\Recall: allowed; Videos\Moments is not.
    let l = links(&[r"C:\Users\me\Videos"], &[r"C:\Users\me\Videos\Recall"]);
    assert_eq!(refusal(r"C:\Users\me\Videos\Recall", &l), None);
    let mut cfg = cfg_at(r"C:\Users\me\Videos\Recall");
    assert_eq!(guard(&mut cfg, None, &l), None);
    assert_eq!(cfg.recording.output_dir, r"C:\Users\me\Videos\Recall");
    let mut moved = cfg_at(r"C:\Users\me\Videos\Moments");
    assert!(guard(&mut moved, Some(r"C:\Users\me\Videos\Recall"), &l).is_some());
    assert_eq!(moved.recording.output_dir, r"C:\Users\me\Videos\Recall");
    assert!(refusal(r"C:\Users\me\Videos", &l).is_some(), "the linked root itself");
}

#[test]
fn links_come_from_the_apps_folder_list() {
    let dir = std::env::temp_dir().join(format!("recall-hdr-links-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("folders.json");
    let text = r#"{"recordings":"X:\\Recall","imported":["D:\\Moments","X:\\Old"],"former_recordings":["X:\\Old"],"extra":1}"#;
    std::fs::write(&path, text).unwrap();
    let read = read_links_from(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(read, links(&[r"D:\Moments"], &[r"X:\Recall", r"X:\Old"]));
    assert_eq!(read_links_from(&dir.join("missing.json")), Links::default());
}

#[test]
fn every_fallback_refused_records_into_recalls_own_clips_folder() {
    // Recall never records into a refused (linked) folder: when the last good folder, the app's
    // recordings folder and the default are all refused, clips go to %LOCALAPPDATA%\Recall\Clips.
    let tmp = std::env::temp_dir().join(format!("recall-hdr-final-fallback-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let _root = crate::user_dirs::inject(&tmp);
    let clips = tmp.join(r"AppData\Local\Recall\Clips");
    let default = crate::record_config::default_output_dir();
    let everything = [r"D:\SteelSeries\Moments", r"D:\Old", r"D:\Owned", default.as_str()];
    let mut cfg = cfg_at(r"D:\SteelSeries\Moments");
    let notice = guard(&mut cfg, Some(r"D:\Old"), &links(&everything, &[r"D:\Owned"])).expect("a notice");
    assert_eq!(cfg.recording.output_dir, clips.to_string_lossy(), "{notice}");
    assert!(clips.is_dir(), "created when missing");
    assert_eq!(notice, format!("Recordings folder unavailable, saving to {}", clips.display()));
    // It cannot be refused: even a link over the temp root itself does not keep the refused folder.
    let mut cfg = cfg_at(r"D:\SteelSeries\Moments");
    let tmp_text = tmp.to_string_lossy().into_owned();
    let all = [r"D:\SteelSeries\Moments", r"D:\Old", r"D:\Owned", default.as_str(), tmp_text.as_str()];
    guard(&mut cfg, Some(r"D:\Old"), &links(&all, &[r"D:\Owned"]));
    assert_eq!(cfg.recording.output_dir, clips.to_string_lossy());
    let _ = std::fs::remove_dir_all(&tmp);
}

/// One of Recall's game folders (it holds the marker) linked in folders.json is not a link: the
/// recordings folder that holds it is not refused (`game_folder_rule::link_refusal`).
#[test]
fn a_linked_game_folder_is_ignored() {
    let dir = std::env::temp_dir().join(format!("recall-hdr-link-gamefolder-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let game = dir.join("Recall").join("VALORANT");
    std::fs::create_dir_all(&game).unwrap();
    crate::game_folder_rule::write_marker(&game).unwrap();
    let path = dir.join("folders.json");
    let list = serde_json::json!({"recordings": dir.join("Recall"), "imported": [game, r"D:\Moments"]});
    std::fs::write(&path, list.to_string()).unwrap();
    let read = read_links_from(&path);
    let recordings = dir.join("Recall").to_string_lossy().into_owned();
    let refused = refusal(&recordings, &read);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(read.linked, vec![r"D:\Moments".to_string()]);
    assert_eq!(refused, None, "the recordings folder holding its own game folder stays usable");
}

/// (R3) A marked folder outside the recordings folder (a backup copy, `D:\Backup\Apex Legends`)
/// is linked like any folder: only the recordings folder's own game folders are ignored.
#[test]
fn a_marked_backup_elsewhere_stays_linked() {
    let dir = std::env::temp_dir().join(format!("recall-hdr-link-backup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let game = dir.join("Recall").join("Apex Legends");
    std::fs::create_dir_all(dir.join("Recall")).unwrap();
    crate::game_folder_rule::make(&game).unwrap();
    let backup = dir.join("Backup").join("Apex Legends");
    std::fs::create_dir_all(&backup).unwrap();
    std::fs::copy(game.join(crate::game_folder_rule::MARKER), backup.join(crate::game_folder_rule::MARKER)).unwrap();
    let path = dir.join("folders.json");
    let list = serde_json::json!({"recordings": dir.join("Recall"), "imported": [game, backup]});
    std::fs::write(&path, list.to_string()).unwrap();
    let read = read_links_from(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(read.linked, vec![backup.to_string_lossy().into_owned()]);
}
