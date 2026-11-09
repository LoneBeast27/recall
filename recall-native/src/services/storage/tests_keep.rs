//! storage.json keeps the keys a newer build wrote.
#[test]
fn storage_json_keeps_a_newer_builds_keys_and_drops_a_cleared_first_run() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("storage.json");
    std::fs::write(&file, r#"{"cache_budget_bytes":5,"first_run":true,"future_tier":"fast"}"#).unwrap();
    let settings = super::evict::Settings { cache_budget_bytes: Some(9), first_run: None };
    super::evict::write_settings(root.path(), &settings).unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(saved, serde_json::json!({"cache_budget_bytes": 9, "future_tier": "fast"}));
    assert_eq!(super::evict::read_settings(root.path()), settings);
}
