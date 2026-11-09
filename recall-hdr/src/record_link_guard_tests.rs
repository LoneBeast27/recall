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
    let text = r#"{"recordings":"V:\\Recall","imported":["D:\\Moments","V:\\Old"],"former_recordings":["V:\\Old"],"extra":1}"#;
    std::fs::write(&path, text).unwrap();
    let read = read_links_from(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(read, links(&[r"D:\Moments"], &[r"V:\Recall", r"V:\Old"]));
    assert_eq!(read_links_from(&dir.join("missing.json")), Links::default());
}
