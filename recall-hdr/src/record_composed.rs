//! When the G4 notice ("Not recording <exe>") may name a fullscreen window
//! that DWM composes. Pure logic; `record_render` asks it on every
//! foreground observation and `render_verdict::ComposedNotice` keeps the
//! 20 s clock.
//!
//! A composed verdict alone is not enough (round 2, R2-2): meeting apps,
//! office slideshows, Electron apps in F11 and Store media apps are composed
//! too. So the notice also needs:
//! - a stem that is not on [`NOTICE_DENY_STEMS`] (meeting, office, chat,
//!   music and note apps: fullscreen there is a call or a presentation), and
//! - a steady game-like rate in the 1.5 s watch: at least
//!   [`STEADY_PRESENTS`] kernel presents (30 fps) and no video swap chain,
//! - and the engine not capturing the desktop (`record_desktop_when_idle`):
//!   then the window is on the clip after all (R2-7).

use crate::render_verdict::RenderStats;

/// The notice's detail line (R2-1: Settings has no "add as a game" control,
/// so it names none).
pub(crate) const NOTICE_DETAIL: &str = "Recall doesn't treat it as a game";

/// Kernel presents a composed window needs in the 1.5 s watch for the notice
/// (30 fps). A slideshow, a chat window or an idle call presents less.
pub(crate) const STEADY_PRESENTS: u32 = 45;

/// Stems whose fullscreen window is not a game the player expects recorded:
/// the notice never names them. Matching is case-insensitive.
const NOTICE_DENY_STEMS: &[&str] = &[
    // Meetings and calls.
    "zoom",
    "ms-teams",
    "teams",
    "msteams",
    "webex",
    "ciscocollabhost",
    "skype",
    "slack",
    "whatsapp",
    "telegram",
    "signal",
    // Office, slideshows, documents, notes.
    "powerpnt",
    "winword",
    "excel",
    "onenote",
    "outlook",
    "olk",
    "acrobat",
    "acrord32",
    "notion",
    "obsidian",
    "figma",
    // Music, video and other Electron-type apps.
    "spotify",
    "electron",
    "netflix",
    "video.ui",
    "microsoft.media.player",
    "microsoft.photos",
];

/// The notice may name this composed window: `stats` = what the probe saw
/// (`Rejected::composed`), `desktop` = the engine is capturing the desktop.
pub(crate) fn notice_wanted(stem: &str, stats: Option<RenderStats>, desktop: bool) -> bool {
    let Some(s) = stats.filter(|_| !desktop) else { return false };
    let denied = NOTICE_DENY_STEMS.iter().any(|d| d.eq_ignore_ascii_case(stem.trim()));
    !denied && s.kernel_presents >= STEADY_PRESENTS && s.video_presents == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(kernel: u32) -> Option<RenderStats> {
        Some(RenderStats { runtime_presents: kernel, kernel_presents: kernel, composed_frames: kernel, ..RenderStats::default() })
    }

    #[test]
    fn composed_notice_names_no_missing_settings_control() {
        assert!(!NOTICE_DETAIL.contains("Settings"), "{NOTICE_DETAIL}");
        assert_eq!(NOTICE_DETAIL, "Recall doesn't treat it as a game");
    }

    #[test]
    fn composed_notice_needs_steady_presents() {
        assert!(!notice_wanted("game", rate(5), false), "a slideshow / idle window");
        assert!(!notice_wanted("game", rate(STEADY_PRESENTS - 1), false));
        assert!(notice_wanted("game", rate(STEADY_PRESENTS), false), "30 fps");
        assert!(notice_wanted("game", rate(90), false));
        let video = rate(90).map(|s| RenderStats { video_presents: 3, ..s });
        assert!(!notice_wanted("game", video, false), "a video swap chain is not a game");
        assert!(!notice_wanted("game", None, false), "not composed");
    }

    #[test]
    fn composed_notice_skips_meeting_and_office_apps() {
        for stem in ["Zoom", "ms-teams", "Teams", "POWERPNT", "Spotify", "slack", "WhatsApp"] {
            assert!(!notice_wanted(stem, rate(90), false), "{stem}");
        }
    }

    #[test]
    fn no_composed_notice_while_the_desktop_is_recorded() {
        assert!(!notice_wanted("game", rate(90), true));
    }
}
