//! What is never recorded, whatever the config says (R2-detect-r10-4: one list). Shared by
//! recall-hdr (`record_target_rules`: never a target; `record_game_ops`: a "Record" entry is
//! refused, Browse included) and recall-native (Settings -> Games -> "Add app…" does not offer
//! them), through `#[path]`. Plain std only: it compiles in both crates.

/// Recall's own processes.
pub const OWN_STEMS: &[&str] = &["recall", "recall-hdr", "recall-native", "recall-hub", "recall-overlay"];

/// Windows itself: the compositor, the shell and its surfaces, Settings, the lock and sign-in
/// screens and system processes (the maintainer 2026-11-07: a user's own entry beats every other list,
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

/// `exe` (`Game.exe` or `game`, any case) is Recall or a part of Windows; an empty name too.
pub fn never_recordable(exe: &str) -> bool {
    let exe = exe.trim();
    let stem = match exe.len().checked_sub(4) {
        Some(i) if exe.is_char_boundary(i) && exe[i..].eq_ignore_ascii_case(".exe") => &exe[..i],
        _ => exe,
    };
    stem.is_empty() || OWN_STEMS.iter().chain(SYSTEM_STEMS).any(|d| d.eq_ignore_ascii_case(stem))
}
