use super::*;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("recall-overlay-trace-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir.join("recall-overlay.log")
}

fn lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path).unwrap_or_default().lines().map(|l| l.split_once("] ").map_or(l, |(_, rest)| rest).to_string()).collect()
}

#[test]
fn writes_a_line_only_when_the_value_changes() {
    let path = temp("change");
    let mut trace = Trace::open(Some(path.clone()), LIMIT);
    assert!(trace.note("raise", "clear"));
    assert!(!trace.note("raise", "clear"));
    assert!(!trace.note("raise", "clear"));
    assert!(trace.note("raise", "above: RocketLeague.exe"));
    assert!(trace.note("island", "show stats"));
    assert!(!trace.note("island", "show stats"));
    assert_eq!(
        lines(&path),
        ["raise: clear", "raise: above: RocketLeague.exe (previous held 2 more calls)", "island: show stats"]
    );
}

#[test]
fn keys_are_independent() {
    let path = temp("keys");
    let mut trace = Trace::open(Some(path.clone()), LIMIT);
    assert!(trace.note("a", "1"));
    assert!(trace.note("b", "1"));
    assert!(!trace.note("a", "1"));
    assert_eq!(lines(&path).len(), 2);
}

#[test]
fn rotates_to_old_past_the_limit() {
    let path = temp("rotate");
    let mut trace = Trace::open(Some(path.clone()), 120);
    for i in 0..6 {
        assert!(trace.note("n", format!("value {i} padded to make the line long")));
    }
    let old = old_path(&path);
    assert!(old.exists(), "the full log moved to .old");
    assert!(fs::metadata(&path).unwrap().len() <= 120);
    assert!(fs::metadata(&old).unwrap().len() <= 120);
    let newest = lines(&path);
    assert!(newest.last().unwrap().starts_with("n: value 5"));
}

#[test]
fn rotation_counts_an_existing_file() {
    let path = temp("existing");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "x".repeat(100)).unwrap();
    let mut trace = Trace::open(Some(path.clone()), 120);
    assert!(trace.event("a line that does not fit after the existing 100 bytes"));
    assert_eq!(fs::read_to_string(old_path(&path)).unwrap().len(), 100);
    assert_eq!(lines(&path).len(), 1);
}

#[test]
fn no_path_writes_nothing() {
    let mut trace = Trace::open(None, LIMIT);
    assert!(!trace.note("raise", "clear"));
    assert!(!trace.event("start"));
}
