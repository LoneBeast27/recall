//! Call, chat, meeting, mail, office and note apps: a fullscreen window there
//! is a call, a conversation or a document, not a game (R4-detect-1, privacy).
//! Such a window is never a recording target and never gets the HDR rules'
//! fullscreen default, with or without trace rights and whether or not the
//! render check could run. An explicit opt-in still beats the list: a game
//! override (`[[overrides]]`) or a `detect.fullscreen_games` entry.
//!
//! The G4 composed notice ("add it as a game") never names these either
//! (`record_composed`), from this same list.
//!
//! Matching is by exact exe stem, case-insensitive, `.exe` optional, so a
//! longer name that only starts the same (`SignalRgb` vs `signal`) is not
//! caught. Discord is not here: `detect.fullscreen_games` lists it by default
//! (Rohan, 2026-11-05) and `rules::DENY_STEMS` keeps it out of HDR switching.

/// Stems checked by `record_target::is_target` and `rules::classify`.
/// "(seen)" = the exe name was read on this PC (install folder or the
/// package's AppxManifest, 2026-11-07); the others are the vendors' known
/// install names, not verified here.
pub(crate) const PRIVATE_APP_STEMS: &[&str] = &[
    // Meetings and calls.
    "zoom",     // (seen) C:\Program Files\Zoom\bin\Zoom.exe
    "cpthost",  // (seen) Zoom's screen-share host, same folder
    "ms-teams", // (seen) new Teams, MSTeams package
    "msteams",  // new Teams' earlier name
    "teams",    // classic Teams
    "webex",
    "ciscocollabhost", // Webex app
    "atmgr",           // Webex Meetings (classic)
    "skype",
    "lync", // Skype for Business
    // Chat.
    "slack",
    "whatsapp",
    "whatsapp.root", // (seen) WhatsApp Desktop package, WhatsApp.Root.exe
    "telegram",
    "signal",
    "viber",
    "phoneexperiencehost", // (seen) Phone Link, Microsoft.YourPhone package
    // Mail.
    "outlook",
    "olk", // (seen) new Outlook, Microsoft.OutlookForWindows package
    "thunderbird",
    // Office, slideshows, documents, notes.
    "powerpnt",
    "winword",
    "excel",
    "onenote",
    "acrobat",
    "acrord32",
    "notion",
    "obsidian", // (seen) C:\Program Files\Obsidian\Obsidian.exe
    "figma",
    // Music and other Electron-type apps.
    "spotify",
    "electron",
];

/// `stem` (`.exe` optional) is on [`PRIVATE_APP_STEMS`].
pub(crate) fn is_private(stem: &str) -> bool {
    let s = stem.trim();
    let s = match s.len().checked_sub(4) {
        Some(i) if s.is_char_boundary(i) && s[i..].eq_ignore_ascii_case(".exe") => &s[..i],
        _ => s,
    };
    PRIVATE_APP_STEMS.iter().any(|d| d.eq_ignore_ascii_case(s))
}

#[cfg(test)]
#[path = "private_apps_tests.rs"]
mod tests;
