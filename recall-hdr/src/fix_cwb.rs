//! The Minecraft borderless fix (mod Cubes Without Borders, id "cwb"): its default borderless
//! type on Windows sizes the window to exactly the screen, which NVIDIA's OpenGL driver then
//! takes exclusively, hiding every window above the game. The mod's own "windows:windowed"
//! type keeps Windows' compositor in charge. Recall sets only that documented config key,
//! in that one Minecraft instance, and only while the game is closed; it never touches the mod.

use std::path::{Path, PathBuf};

use recall_fixes::plan::{minecraft_borderless_cwb, JsonKey, Plan};
use recall_fixes::Undo;
use serde_json::{Map, Value};

pub const KEY: &str = "borderlessFullscreenType";
pub const VALUE: &str = "windows:windowed";

/// What the read-only check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    Offer(Plan),
    AlreadyFixed,
    /// The instance does not use the mod, or its config is unreadable: no offer.
    NotApplicable(String),
}

/// The game folder from a Java command line (`--gameDir <path>`, quoted or not).
pub fn instance_of(command_line: &str) -> Option<PathBuf> {
    let at = command_line.find("--gameDir")? + "--gameDir".len();
    let rest = command_line[at..].trim_start();
    let path = match rest.strip_prefix('"') {
        Some(quoted) => &quoted[..quoted.find('"')?],
        None => rest.split_whitespace().next()?,
    };
    (!path.is_empty()).then(|| PathBuf::from(path))
}

pub fn config_path(instance: &Path) -> PathBuf {
    instance.join("config").join("cwb.json")
}

/// `<instance>\config\cwb.json` back to `<instance>`; anything else is refused.
fn instance_of_config(target: &str) -> Option<PathBuf> {
    let p = Path::new(target);
    let ok = p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("cwb.json")) && p.parent()?.file_name().is_some_and(|n| n.eq_ignore_ascii_case("config"));
    ok.then(|| p.parent().and_then(Path::parent).map(Path::to_path_buf)).flatten()
}

/// The instance loads the mod: `mods\cwb-*.jar` (a `.disabled` copy does not count).
pub fn has_mod(instance: &Path) -> bool {
    std::fs::read_dir(instance.join("mods")).is_ok_and(|d| {
        d.flatten().any(|e| {
            let n = e.file_name().to_string_lossy().to_lowercase();
            n.starts_with("cwb-") && n.ends_with(".jar")
        })
    })
}

fn read(path: &Path) -> Result<Option<Map<String, Value>>, String> {
    match std::fs::read(path) {
        Ok(b) => match serde_json::from_slice::<Value>(&b) {
            Ok(Value::Object(m)) => Ok(Some(m)),
            Ok(_) => Err("the mod's config is not a JSON object".into()),
            Err(e) => Err(format!("the mod's config could not be read: {e}")),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("the mod's config could not be read: {e}")),
    }
}

fn write(path: &Path, map: &Map<String, Value>) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.recall-tmp");
    let body = serde_json::to_vec_pretty(&Value::Object(map.clone())).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Read-only: the plan for the instance whose config is `target`.
pub fn plan(target: &str) -> Result<Check, String> {
    let Some(instance) = instance_of_config(target) else { return Err("not a Cubes Without Borders config path".into()) };
    if !has_mod(&instance) {
        return Ok(Check::NotApplicable("this Minecraft instance does not use Cubes Without Borders".into()));
    }
    let current = match read(Path::new(target)) {
        Ok(map) => map.and_then(|m| m.get(KEY).cloned()),
        Err(e) => return Ok(Check::NotApplicable(e)),
    };
    if current == Some(Value::String(VALUE.into())) {
        return Ok(Check::AlreadyFixed);
    }
    Ok(Check::Offer(minecraft_borderless_cwb(JsonKey { file: target.to_owned(), key: KEY.into(), from: current, to: Value::String(VALUE.into()) })))
}

fn refuse_while_running(instance: &Path) -> Result<(), String> {
    if crate::fix_procs::java_running_in(instance) {
        return Err("Minecraft is running from this folder. Close it and try again; the mod rewrites its config when the game exits.".into());
    }
    Ok(())
}

/// Helper: set the key. The game must be closed.
pub fn apply(target: &str) -> Result<Undo, String> {
    let instance = instance_of_config(target).ok_or("not a Cubes Without Borders config path")?;
    refuse_while_running(&instance)?;
    let path = Path::new(target);
    let existing = read(path)?;
    let created_file = existing.is_none();
    let mut map = existing.unwrap_or_default();
    let prior = map.insert(KEY.into(), Value::String(VALUE.into()));
    write(path, &map)?;
    Ok(Undo::JsonKey { file: target.to_owned(), key: KEY.into(), value: Value::String(VALUE.into()), prior, created_file })
}

/// Helper: put the key back, only while it still holds Recall's value.
pub fn undo(target: &str, undo: &Undo) -> Result<Option<String>, String> {
    let Undo::JsonKey { file, key, value, prior, created_file } = undo else { return Err("the undo receipt does not match this fix".into()) };
    if !file.eq_ignore_ascii_case(target) || key != KEY || *value != Value::String(VALUE.into()) {
        return Err("the undo receipt does not match this fix".into());
    }
    let instance = instance_of_config(target).ok_or("not a Cubes Without Borders config path")?;
    refuse_while_running(&instance)?;
    let path = Path::new(target);
    let Some(mut map) = read(path)? else { return Ok(None) };
    if map.get(KEY) != Some(value) {
        return Ok(Some("The borderless type was changed after the fix, so it was left as you set it.".into()));
    }
    match prior {
        Some(old) => {
            map.insert(KEY.into(), old.clone());
        }
        None => {
            map.remove(KEY);
        }
    }
    if *created_file && map.is_empty() {
        std::fs::remove_file(path).map_err(|e| e.to_string())?;
        return Ok(None);
    }
    write(path, &map).map(|()| None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("recall-cwb-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("mods")).unwrap();
        d
    }

    #[test]
    fn the_game_folder_comes_from_the_command_line() {
        let cmd = r#"javaw.exe -Xmx4G -cp a.jar net.fabricmc.loader.impl.launch.knot.KnotClient --username x --gameDir "C:\Users\u\AppData\Roaming\ModrinthApp\profiles\Fabulously Optimized 1.0.0" --assetsDir y"#;
        assert_eq!(instance_of(cmd).unwrap(), PathBuf::from(r"C:\Users\u\AppData\Roaming\ModrinthApp\profiles\Fabulously Optimized 1.0.0"));
        assert_eq!(instance_of("java --gameDir C:\\mc --x").unwrap(), PathBuf::from(r"C:\mc"));
        assert!(instance_of("java -jar x.jar").is_none());
    }

    #[test]
    fn only_a_cwb_config_path_is_a_target() {
        assert!(instance_of_config(r"C:\mc\config\cwb.json").is_some());
        assert!(instance_of_config(r"C:\mc\config\other.json").is_none());
        assert!(instance_of_config(r"C:\mc\cwb.json").is_none());
    }

    #[test]
    fn the_mod_must_be_loaded() {
        let d = temp("mod");
        assert!(!has_mod(&d));
        std::fs::write(d.join("mods").join("cwb-fabric-3.2.0.jar.disabled"), b"").unwrap();
        assert!(!has_mod(&d), "a disabled copy does not count");
        std::fs::write(d.join("mods").join("cwb-fabric-3.2.0.jar"), b"").unwrap();
        assert!(has_mod(&d));
        let target = config_path(&d).to_string_lossy().into_owned();
        let Check::Offer(plan) = plan(&target).unwrap() else { panic!("an offer") };
        assert!(plan.steps[0].change.exact().join("\n").contains("windows:windowed"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn apply_and_undo_keep_everything_else() {
        let d = temp("roundtrip");
        std::fs::write(d.join("mods").join("cwb-3.2.0.jar"), b"").unwrap();
        let cfg = config_path(&d);
        std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        std::fs::write(&cfg, br#"{"fullscreenMode":"BORDERLESS","borderlessFullscreenType":"minecraft:windowed"}"#).unwrap();
        let target = cfg.to_string_lossy().into_owned();
        let u = apply(&target).unwrap();
        let after: Value = serde_json::from_slice(&std::fs::read(&cfg).unwrap()).unwrap();
        assert_eq!(after[KEY], VALUE);
        assert_eq!(after["fullscreenMode"], "BORDERLESS", "other keys untouched");
        assert_eq!(plan(&target).unwrap(), Check::AlreadyFixed);
        assert_eq!(undo(&target, &u).unwrap(), None);
        let back: Value = serde_json::from_slice(&std::fs::read(&cfg).unwrap()).unwrap();
        assert_eq!(back[KEY], "minecraft:windowed", "the prior value is back");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn undo_leaves_a_later_user_change_and_removes_a_file_it_made() {
        let d = temp("cas");
        std::fs::write(d.join("mods").join("cwb-3.2.0.jar"), b"").unwrap();
        let cfg = config_path(&d);
        let target = cfg.to_string_lossy().into_owned();
        let u = apply(&target).unwrap();
        std::fs::write(&cfg, br#"{"borderlessFullscreenType":"minecraft:fullscreen"}"#).unwrap();
        assert!(undo(&target, &u).unwrap().unwrap().contains("left as you set it"));
        std::fs::write(&cfg, br#"{"borderlessFullscreenType":"windows:windowed"}"#).unwrap();
        assert_eq!(undo(&target, &u).unwrap(), None);
        assert!(!cfg.exists(), "Recall made the file and it holds nothing else");
        let _ = std::fs::remove_dir_all(&d);
    }
}
