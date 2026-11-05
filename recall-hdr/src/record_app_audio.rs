//! `recording.app_audio`: the older list of apps recorded as tracks of their own. Read only
//! to build `recording.audio_sources` (`record_audio_sources::from_legacy`); the engine no
//! longer receives it (`RECALL_AUDIO_APPS` is removed from its environment).

use serde::{Deserialize, Serialize};

/// Most app tracks a recording carries (each is one capture stream).
pub const APP_AUDIO_MAX: usize = 6;
/// Engine variable that held the list (now always removed).
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
}
