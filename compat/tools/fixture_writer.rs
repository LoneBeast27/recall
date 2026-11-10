//! Mounted into a release's `recall-hdr/src/config.rs` by `scripts/dev/compat-fixture.ps1`
//! (never committed there). In `COMPAT_DIR`: writes that release's defaults (`default.toml`),
//! then loads `input.toml` with that release's loader and saves it as `config.toml`, noting in
//! `report.txt` whether the release read every value as written (no repairs).
use super::{load_bytes, Config};

#[test]
fn compat_fixture_writer() {
    let Some(dir) = std::env::var_os("COMPAT_DIR").map(std::path::PathBuf::from) else { return };
    std::fs::create_dir_all(&dir).unwrap();
    Config::default().save_to(&dir.join("default.toml")).unwrap();
    let input = dir.join("input.toml");
    let bytes = std::fs::read(&input).expect("input.toml");
    let strict: Result<Config, _> = toml::from_str(std::str::from_utf8(&bytes).unwrap());
    let cfg = load_bytes(&input, &bytes, std::time::SystemTime::now());
    let repairs = crate::config_repairs::take(&input);
    let same = strict.as_ref().is_ok_and(|s| s == &cfg);
    let mut report = format!("strict_parse_ok={}\nstrict_equals_load={same}\nrepairs={repairs:?}\n", strict.is_ok());
    if let Err(e) = &strict {
        report.push_str(&format!("strict_error={e}\n"));
    }
    std::fs::write(dir.join("report.txt"), report).unwrap();
    cfg.save_to(&dir.join("config.toml")).unwrap();
}
