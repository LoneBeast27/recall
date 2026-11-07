use super::*;
use crate::record_config::apply_patch;
use serde_json::json;

fn store(tag: &str) -> ConfigStore {
    let dir = std::env::temp_dir().join(format!(
        "recall-hdr-watch-{tag}-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    ConfigStore::at(dir.join("config.toml"))
}

/// A moment after the file's last write that is past [`SETTLE`].
fn settled(store: &ConfigStore) -> SystemTime {
    std::fs::metadata(store.path()).unwrap().modified().unwrap() + SETTLE + Duration::from_secs(1)
}

fn edited(base: Base) -> Option<Config> {
    match base {
        Base::Edited(cfg) => Some(cfg),
        _ => None,
    }
}

fn bad_copies(store: &ConfigStore) -> Vec<String> {
    let dir = store.path().parent().unwrap();
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("config.toml.bad"))
        .collect();
    names.sort();
    names
}

#[test]
fn a_hand_edit_made_while_running_survives_the_next_patch() {
    let store = store("edit");
    let running = Config::default();
    store.save(&running).unwrap();
    let mut seen = FileStamp::of(store.path());
    let first = seen;
    let text = std::fs::read_to_string(store.path()).unwrap();
    assert!(text.contains("cpu_mask = \"auto\""));
    std::fs::write(store.path(), text.replace("cpu_mask = \"auto\"", "cpu_mask = \"ffff0000\"")).unwrap();
    let base = edited(base_for_patch(&store, &mut seen, settled(&store))).unwrap_or_else(|| running.clone());
    assert_ne!(seen, first, "the edit's stamp is remembered");
    let out = apply_patch(&base, &json!({"overlay": {"enabled": true}})).unwrap();
    store.save(&out.config).unwrap();
    let back = store.load();
    assert_eq!(back.recording.cpu_mask, "ffff0000", "the hand edit is kept");
    assert!(back.overlay.enabled, "the patch applied on top");
}

#[test]
fn an_unchanged_file_is_not_read_again() {
    let store = store("same");
    let running = Config::default();
    store.save(&running).unwrap();
    let mut seen = FileStamp::of(store.path());
    assert!(matches!(base_for_patch(&store, &mut seen, settled(&store)), Base::Memory));
}

#[test]
fn a_hand_edit_with_one_wrongly_typed_value_keeps_the_rest_of_the_edit() {
    let store = store("typed");
    store.save(&Config::default()).unwrap();
    let mut seen = FileStamp::of(store.path());
    let text = std::fs::read_to_string(store.path()).unwrap();
    let text = text.replace("cpu_mask = \"auto\"", "cpu_mask = \"ffff0000\"");
    let fps = text.lines().find(|l| l.starts_with("fps = ")).unwrap().to_string();
    std::fs::write(store.path(), text.replace(&fps, "fps = \"fast\"")).unwrap();
    let base = edited(base_for_patch(&store, &mut seen, settled(&store)));
    let base = base.expect("a wrong-typed value costs only its key, not the whole edit");
    assert_eq!(base.recording.cpu_mask, "ffff0000", "the rest of the edit is the base");
    assert_eq!(base.recording.fps, Config::default().recording.fps, "the bad key gets its default");
}

#[test]
fn a_broken_hand_edit_is_kept_as_bad_before_the_patch_saves_over_it() {
    let store = store("broken");
    let mut running = Config::default();
    running.recording.fps = 120;
    store.save(&running).unwrap();
    let mut seen = FileStamp::of(store.path());
    std::fs::write(store.path(), "[recording]\nfps = = 1\n").unwrap();
    let base = base_for_patch(&store, &mut seen, settled(&store));
    let Base::Broken(line) = base else { panic!("a broken edit is reported, got {base:?}") };
    let bad = bad_copies(&store);
    assert_eq!(bad.len(), 1, "one dated backup: {bad:?}");
    assert!(line.contains(&bad[0]), "the app is told the backup's name: {line}");
    let kept = std::fs::read_to_string(store.path().with_file_name(&bad[0])).unwrap();
    assert_eq!(kept, "[recording]\nfps = = 1\n", "the backup holds the edit");
    assert!(store.path().exists(), "the file itself stays until the save replaces it");
    assert!(matches!(base_for_patch(&store, &mut seen, settled(&store)), Base::Memory), "reported once");
}

#[test]
fn an_empty_file_is_never_the_base() {
    let store = store("empty");
    let mut running = Config::default();
    running.recording.fps = 120;
    store.save(&running).unwrap();
    let mut seen = FileStamp::of(store.path());
    std::fs::write(store.path(), "  \n").unwrap();
    let base = base_for_patch(&store, &mut seen, settled(&store));
    assert!(matches!(base, Base::Memory), "the settings in memory stay the base, got {base:?}");
}

#[test]
fn a_file_written_moments_ago_is_not_merged_or_saved_over() {
    let store = store("busy");
    store.save(&Config::default()).unwrap();
    let mut seen = FileStamp::of(store.path());
    let first = seen;
    std::fs::write(store.path(), "[recording]\n").unwrap();
    let written = std::fs::metadata(store.path()).unwrap().modified().unwrap();
    let base = base_for_patch(&store, &mut seen, written + Duration::from_millis(500));
    assert!(matches!(base, Base::Busy), "a half-written file is not the base, got {base:?}");
    assert_eq!(seen, first, "and it is looked at again on the next patch");
    let early = base_for_patch(&store, &mut seen, written - Duration::from_secs(60));
    assert!(!matches!(early, Base::Busy), "a write time ahead of the clock never blocks for good");
}
