//! The controller's `mod:"session"` status fields the island needs:
//! `{"type":"status","state":S,"target":{"name":S,"exe":S|null,"pid":N|null,"hwnd":N?}|null,
//!   "overlay":{"enabled":B,"stats_style":"minimal",...},"overlay_stats":B,
//!   "fps_source":"etw|no_permission|off","save_seconds":N,...}`.
//! `overlay.enabled` gates every notice, `overlay_stats` the stats island;
//! before the first status both are off.

use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Session {
    /// `waiting|starting|recording|paused|error`; "" before the first status.
    pub state: String,
    /// The target's display name ("VALORANT", "Desktop"); "" without a target.
    pub target_name: String,
    pub target_pid: Option<u32>,
    /// The game window, when the controller includes it (`target.hwnd`).
    pub target_hwnd: Option<isize>,
    /// The target's executable ("VALORANT-Win64-Shipping.exe"); "" for the desktop.
    pub target_exe: String,
    /// The target is the desktop (it has no executable).
    pub desktop: bool,
    /// Notices are on (`overlay.enabled`; a status without it keeps the
    /// pre-island meaning: the controller only ran the overlay when enabled).
    pub notices: bool,
    /// The stats island toggle (`[overlay] stats`, top-level `overlay_stats`).
    pub overlay_stats: bool,
    /// `overlay.stats_style`; only "minimal" exists, anything else draws minimal.
    pub stats_style: String,
    /// The controller's FPS source (`etw` | `no_permission` | `off`); "" = not reported.
    pub fps_source: String,
    /// The configured replay length (seconds).
    pub save_seconds: u64,
}

impl Session {
    /// Applies a session `status` event; true when anything the island uses changed.
    pub fn apply_status(&mut self, event: &Value) -> bool {
        let target = event.get("target").filter(|t| t.is_object());
        let field = |name: &str| target.and_then(|t| t.get(name));
        let overlay = |name: &str| event.get("overlay").and_then(|o| o.get(name));
        let next = Session {
            notices: overlay("enabled").and_then(Value::as_bool).unwrap_or(true),
            stats_style: text(overlay("stats_style")),
            state: text(event.get("state")),
            target_name: text(field("name")),
            target_pid: field("pid")
                .and_then(Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok())
                .filter(|&pid| pid != 0),
            target_hwnd: field("hwnd")
                .and_then(Value::as_i64)
                .filter(|&hwnd| hwnd != 0)
                .map(|hwnd| hwnd as isize),
            target_exe: text(field("exe")),
            desktop: target.is_some() && field("exe").is_none_or(Value::is_null),
            overlay_stats: event.get("overlay_stats").and_then(Value::as_bool).unwrap_or(false),
            fps_source: text(event.get("fps_source")),
            save_seconds: event.get("save_seconds").and_then(Value::as_u64).unwrap_or(0),
        };
        if *self == next {
            return false;
        }
        *self = next;
        true
    }

    pub fn recording(&self) -> bool {
        self.state == "recording"
    }

    pub fn paused(&self) -> bool {
        self.state == "paused"
    }

    /// The HUD-scan key of the current target ("" for the desktop / no target).
    pub fn game_key(&self) -> String {
        if self.desktop {
            return String::new();
        }
        game_key(&self.target_name)
    }

    /// The Dynamic corner's per-game key: the exe stem only, lowercased
    /// ("rocketleague"). Never the window title: this key reaches disk
    /// (`island-hud.json`) and the trace. "" for the desktop / no exe.
    pub fn hud_key(&self) -> String {
        if self.desktop {
            return String::new();
        }
        let stem = self.target_exe.rsplit(['\\', '/']).next().unwrap_or("");
        let lower = stem.to_ascii_lowercase();
        game_key(lower.strip_suffix(".exe").unwrap_or(&lower))
    }
}

fn text(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or("").to_string()
}

/// Lowercase ASCII letters and digits of a target name:
/// "VALORANT" → "valorant", "Rocket League" → "rocketleague".
pub fn game_key(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hud_keys_follow_the_exe_and_never_the_desktop() {
        let mut session = Session::default();
        session.apply_status(&json!({"type":"status","state":"recording","target":{"name":"Rocket League (64-bit, DX11, Cooked)","exe":"RocketLeague.exe","pid":4}}));
        assert_eq!(session.hud_key(), "rocketleague");
        session.apply_status(&json!({"type":"status","state":"recording","target":{"name":"VALORANT","exe":"C:\\Riot\\VALORANT-Win64-Shipping.exe","pid":5}}));
        assert_eq!(session.hud_key(), "valorantwin64shipping");
        session.apply_status(&json!({"type":"status","state":"recording","target":{"name":"Desktop","exe":null,"pid":null}}));
        assert_eq!(session.hud_key(), "");
    }

    #[test]
    fn hud_key_never_uses_the_window_title() {
        let mut session = Session::default();
        // No exe: the key is empty, never the title.
        session.apply_status(&json!({"type":"status","state":"recording","target":{"name":"My Secret Chat - Discord","exe":"","pid":4}}));
        assert_eq!(session.hud_key(), "");
        // The exe wins over any title, and the title leaves no trace in the key.
        session.apply_status(&json!({"type":"status","state":"recording","target":{"name":"My Secret Chat - Discord","exe":"C:\\Apps\\Discord.EXE","pid":4}}));
        assert_eq!(session.hud_key(), "discord");
        assert!(!session.hud_key().contains("secret"));
    }

    #[test]
    fn game_keys_match_the_hud_scan_table() {
        assert_eq!(game_key("VALORANT"), "valorant");
        assert_eq!(game_key("Rocket League"), "rocketleague");
        assert_eq!(game_key("Counter-Strike 2"), "counterstrike2");
        assert_eq!(game_key("Tom Clancy's Rainbow Six® Siege"), "tomclancysrainbowsixsiege");
        assert_eq!(game_key(""), "");
    }

    #[test]
    fn status_reads_target_toggle_and_fps_source() {
        let mut session = Session::default();
        assert!(!session.notices && !session.overlay_stats, "nothing shows before the first status");
        let status = json!({"type":"status","state":"recording","overlay_stats":true,"fps_source":"etw","save_seconds":120,
            "overlay":{"enabled":false,"corner":"top-right","running":true,"stats_style":"minimal"},
            "target":{"name":"VALORANT","exe":"VALORANT-Win64-Shipping.exe","pid":42,"hwnd":1234}});
        assert!(session.apply_status(&status));
        assert!(!session.apply_status(&status), "unchanged status is not a change");
        assert!(session.recording() && session.overlay_stats && !session.desktop);
        assert!(!session.notices, "overlay.enabled = false gates the notices");
        assert_eq!(session.stats_style, "minimal");
        assert_eq!((session.target_pid, session.target_hwnd), (Some(42), Some(1234)));
        assert_eq!((session.game_key().as_str(), session.fps_source.as_str()), ("valorant", "etw"));
    }

    #[test]
    fn desktop_and_missing_fields_default_safely() {
        let mut session = Session::default();
        session.apply_status(&json!({"type":"status","state":"recording","target":{"name":"Desktop","exe":null,"pid":null}}));
        assert!(session.desktop && !session.overlay_stats);
        assert!(session.notices, "a status without `overlay` keeps notices on (pre-island controller)");
        assert_eq!((session.target_pid, session.game_key().as_str(), session.fps_source.as_str()), (None, "", ""));
        session.apply_status(&json!({"type":"status","state":"waiting","target":null}));
        assert!(!session.desktop && session.target_name.is_empty());
    }
}
