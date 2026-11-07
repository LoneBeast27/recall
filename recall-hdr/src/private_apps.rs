//! Call, chat, meeting, mail, office, note, media-player and music apps: a
//! fullscreen window there is a call, a conversation, a document or a film,
//! not a game (R4-detect-1, R5-detect-1, privacy). Such a window is never a
//! recording target and never gets the HDR rules' fullscreen default, with or
//! without trace rights and whether or not the render check could run.
//!
//! Opt-ins that beat the list:
//! - recording: a game override (`[[overrides]]`), a `detect.fullscreen_games`
//!   entry, or positive game evidence (the exe lives in a Steam library, or
//!   `known_games` lists it) -- an indie Steam game can ship a stock
//!   `electron.exe` or be called `Element.exe` (R5-detect-2);
//! - HDR rules: a game override, or a `detect.fullscreen_games` entry for a
//!   fullscreen window (`rules::classify`, R5-detect-3).
//!
//! The G4 composed notice ("add it as a game") never names these either
//! (`record_composed`), from this same list.
//!
//! Matching is by exact exe stem, case-insensitive, `.exe` optional, so a
//! longer name that only starts the same (`SignalRgb` vs `signal`) is not
//! caught. Discord is not here: `detect.fullscreen_games` lists it by default
//! (Rohan, 2026-11-05) and `rules::DENY_STEMS` keeps it out of HDR switching.
//! The stems live in `private_app_stems`.

pub(crate) use crate::private_app_stems::PRIVATE_APP_STEMS;

/// `stem` (`.exe` optional) is on [`PRIVATE_APP_STEMS`].
pub(crate) fn is_private(stem: &str) -> bool {
    let s = stem.trim();
    let s = match s.len().checked_sub(4) {
        Some(i) if s.is_char_boundary(i) && s[i..].eq_ignore_ascii_case(".exe") => &s[..i],
        _ => s,
    };
    PRIVATE_APP_STEMS.iter().any(|d| d.eq_ignore_ascii_case(s))
}

/// `stem` is a private app and nothing says it is a game: `game_evidence` =
/// the exe lives in a Steam library or `known_games` lists it (R5-detect-2).
pub(crate) fn blocks_recording(stem: &str, game_evidence: bool) -> bool {
    !game_evidence && is_private(stem)
}

#[cfg(test)]
#[path = "private_apps_tests.rs"]
mod tests;
