//! `recording.app_audio`: apps the person chose to record as tracks of their own
//! (Settings → Clip → Audio → App audio). The engine captures each app's process
//! tree by process loopback and names the track with `label`.
//!
//! Passed to the engine as `RECALL_AUDIO_APPS`, a JSON array of `{exe, label}`.

use serde::{Deserialize, Serialize};

/// Most app tracks a recording carries (each is one capture stream).
pub const APP_AUDIO_MAX: usize = 6;
/// Engine variable holding the list as JSON.
pub const APPS_ENV: &str = "RECALL_AUDIO_APPS";

/// One app recorded as its own track.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppAudioSource {
    /// Image file name, e.g. `player.exe` (no folders).
    pub exe: String,
    /// Track name in clips ("" = the exe name without `.exe`).
    #[serde(default)]
    pub label: String,
}

/// At most `APP_AUDIO_MAX` entries, each an exe name with no folders, no app twice
/// (case-insensitive), labels one line of at most 64 characters.
pub fn validate(apps: &[AppAudioSource]) -> Result<(), String> {
    if apps.len() > APP_AUDIO_MAX {
        return Err(format!("recording.app_audio holds at most {APP_AUDIO_MAX} apps, got {}", apps.len()));
    }
    let mut seen = std::collections::HashSet::new();
    for app in apps {
        let exe = app.exe.trim();
        if exe.is_empty() || exe.len() > 64 || exe.chars().any(|c| matches!(c, '\\' | '/' | ':') || c.is_control()) {
            return Err(format!("recording.app_audio exe '{exe}' must be an exe file name (no folders)"));
        }
        if !seen.insert(exe_key(exe)) {
            return Err(format!("recording.app_audio lists {exe} twice"));
        }
        if app.label.chars().count() > 64 || app.label.chars().any(char::is_control) {
            return Err(format!("recording.app_audio label for {exe} must be one line of at most 64 characters"));
        }
    }
    Ok(())
}

/// Case-insensitive identity of an exe, with or without `.exe`.
fn exe_key(exe: &str) -> String {
    let lower = exe.trim().to_lowercase();
    lower.strip_suffix(".exe").map_or(lower.clone(), str::to_owned)
}

/// Same list for the engine? Order matters: it is the clip's track order.
pub fn changed(old: &[AppAudioSource], new: &[AppAudioSource]) -> bool {
    let key = |apps: &[AppAudioSource]| apps.iter().map(|a| (exe_key(&a.exe), a.label.trim().to_owned())).collect::<Vec<_>>();
    key(old) != key(new)
}

/// `RECALL_AUDIO_APPS` for the engine: the list as JSON, or `None` (unset) when empty.
pub fn engine_value(apps: &[AppAudioSource]) -> Option<String> {
    let trimmed: Vec<AppAudioSource> = apps
        .iter()
        .map(|a| AppAudioSource { exe: a.exe.trim().to_owned(), label: a.label.trim().to_owned() })
        .collect();
    (!trimmed.is_empty()).then(|| serde_json::to_string(&trimmed).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(exe: &str, label: &str) -> AppAudioSource {
        AppAudioSource { exe: exe.into(), label: label.into() }
    }

    #[test]
    fn an_app_listed_twice_too_many_apps_or_a_folder_is_rejected() {
        assert!(validate(&[app("a.exe", "A"), app("b.exe", "")]).is_ok());
        assert!(validate(&[app("a.exe", "A"), app("A", "again")]).unwrap_err().contains("twice"));
        let seven: Vec<_> = (0..7).map(|n| app(&format!("app{n}.exe"), "")).collect();
        assert!(validate(&seven).unwrap_err().contains("at most"));
        assert!(validate(&[app(r"C:\apps\a.exe", "")]).is_err());
        assert!(validate(&[app("a.exe", "two\nlines")]).is_err());
    }

    #[test]
    fn the_engine_gets_json_and_nothing_when_the_list_is_empty() {
        assert_eq!(engine_value(&[]), None);
        let value = engine_value(&[app(" a.exe ", " Source A ")]).unwrap();
        assert_eq!(value, r#"[{"exe":"a.exe","label":"Source A"}]"#);
    }

    #[test]
    fn reordering_or_relabelling_is_a_change_case_is_not() {
        let list = [app("a.exe", "A"), app("b.exe", "B")];
        assert!(!changed(&list, &[app("A.EXE", "A"), app("b", "B")]));
        assert!(changed(&list, &[app("b.exe", "B"), app("a.exe", "A")]));
        assert!(changed(&list, &[app("a.exe", "A2"), app("b.exe", "B")]));
    }
}
