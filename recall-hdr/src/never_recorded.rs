//! What is never recorded, whatever the config says (R2-detect-r10-4: one list). Shared by
//! recall-hdr (`record_target_rules`: never a target; `record_game_ops`: a "Record" entry is
//! refused, Browse included) and recall-native (Settings -> Games -> "Add app…" does not offer
//! them), through `#[path]`. Plain std only: it compiles in both crates.

/// Recall's own processes.
pub const OWN_STEMS: &[&str] = &["recall", "recall-hdr", "recall-native", "recall-hub", "recall-overlay"];

/// Windows itself: the compositor, the shell and its surfaces, Settings, the lock and sign-in
/// screens and system processes (Rohan 2026-11-07: a user's own entry beats every other list,
/// not these). Screensavers (`.scr`) are refused by image name (`classify_event`).
pub const SYSTEM_STEMS: &[&str] = &[
    // DWM owns the "Not Responding" ghost of a hung game (third audit U3; the watcher
    // maps a ghost to its hung window first, see `ghost_window`).
    "dwm",
    "explorer",
    "shellhost",
    "shellexperiencehost",
    "startmenuexperiencehost",
    "searchhost",
    "searchapp",
    "textinputhost",
    "applicationframehost",
    "systemsettings",
    "lockapp",
    "logonui",
    "winlogon",
    "csrss",
    "sihost",
    "svchost",
    "conhost",
    "ctfmon",
    "fontdrvhost",
    "taskhostw",
    "runtimebroker",
    "smartscreen",
    "consent",
];

/// `exe` (`Game.exe`, `game` or a full path, any case) is Recall or a part of Windows, or a
/// screensaver (`*.scr`, gate p5-3); an empty name too.
pub fn never_recordable(exe: &str) -> bool {
    let name = exe.trim().rsplit(['\\', '/']).next().unwrap_or("").trim();
    let ext = |e: &str| name.len().checked_sub(e.len()).filter(|&i| name.is_char_boundary(i) && name[i..].eq_ignore_ascii_case(e));
    if ext(".scr").is_some() {
        return true;
    }
    let stem = ext(".exe").map_or(name, |i| &name[..i]);
    stem.is_empty() || OWN_STEMS.iter().chain(SYSTEM_STEMS).any(|d| d.eq_ignore_ascii_case(stem))
}
