//! When the G4 notice ("Not recording <exe>: add it as a game in Settings")
//! may name a fullscreen window
//! that DWM composes. Pure logic; `record_render` asks it on every
//! foreground observation and `render_verdict::ComposedNotice` keeps the
//! 20 s clock.
//!
//! A composed verdict alone is not enough (round 2, R2-2): meeting apps,
//! office slideshows, Electron apps in F11 and Store media apps are composed
//! too. So the notice also needs:
//! - a stem that is not on [`NOTICE_DENY_STEMS`] (meeting, office, chat,
//!   music and note apps: fullscreen there is a call or a presentation), and
//! - a steady game-like rate in some 1.5 s watch of the 20 s streak: at
//!   least [`STEADY_PRESENTS`] kernel presents (24 fps) and no video swap
//!   chain. A slower recheck does not reset the streak while the verdict
//!   stays composed (R3-detect-1: a game capped at 30 fps reads ~43-46),
//! - and the engine not capturing the desktop (`record_desktop_when_idle`):
//!   then the window is on the clip after all (R2-7).

use crate::render_verdict::RenderStats;

/// The notice's detail line; title + ": " + detail is the SPEC text
/// "Not recording <exe>: add it as a game in Settings" (merge gate ruling).
pub(crate) const NOTICE_DETAIL: &str = "add it as a game in Settings";

/// The notice for `exe`: (title, detail).
pub(crate) fn notice_text(exe: &str) -> (String, &'static str) {
    (format!("Not recording {exe}"), NOTICE_DETAIL)
}

/// Kernel presents a composed window needs in a 1.5 s watch for the notice
/// (24 fps). A slideshow, a chat window or an idle call presents less.
pub(crate) const STEADY_PRESENTS: u32 = 36;

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
#[cfg(test)]
pub(crate) fn notice_wanted(stem: &str, stats: Option<RenderStats>, desktop: bool) -> bool {
    composed_front(stem, stats, desktop) == Some(true)
}

/// `Some(steady)` while the notice's 20 s clock may run for this window (a
/// composed verdict, not a denied stem, no desktop capture); `steady` = the
/// last watch saw a game-like rate (`ComposedNotice::observe_steady`).
pub(crate) fn composed_front(stem: &str, stats: Option<RenderStats>, desktop: bool) -> Option<bool> {
    let s = stats.filter(|_| !desktop)?;
    let denied = NOTICE_DENY_STEMS.iter().any(|d| d.eq_ignore_ascii_case(stem.trim()));
    (!denied).then_some(s.kernel_presents >= STEADY_PRESENTS && s.video_presents == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(kernel: u32) -> Option<RenderStats> {
        Some(RenderStats { runtime_presents: kernel, kernel_presents: kernel, composed_frames: kernel, ..RenderStats::default() })
    }

    #[test]
    fn composed_notice_text_is_the_spec_text() {
        let (title, detail) = notice_text("Game.exe");
        assert_eq!(format!("{title}: {detail}"), "Not recording Game.exe: add it as a game in Settings");
    }

    #[test]
    fn a_30_fps_capped_composed_game_is_steady() {
        // R3-detect-1: 30 fps over a 1.5 s watch reads ~43-46 with the flush lag.
        assert!(notice_wanted("game", rate(44), false));
        assert!(notice_wanted("game", rate(36), false), "24 fps");
        assert!(!notice_wanted("game", rate(35), false));
    }

    #[test]
    fn a_slow_recheck_keeps_the_composed_streak() {
        use crate::render_verdict::ComposedNotice;
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let mut n = ComposedNotice::default();
        let mut named = None;
        // Rechecks at 0, 2, 6, 14, 22 s alternate a steady and a slow watch.
        for (secs, kernel) in [(0, 50), (2, 20), (6, 50), (14, 20), (22, 20)] {
            let front = composed_front("game", rate(kernel), false).map(|steady| ((7, 70, "Game.exe"), steady));
            named = named.or(n.observe_steady(front, t0 + Duration::from_secs(secs)));
        }
        assert_eq!(named.as_deref(), Some("Game.exe"));
        // Never steady in the streak: never named.
        let mut quiet = ComposedNotice::default();
        for secs in [0, 10, 21, 30] {
            let front = composed_front("slides", rate(10), false).map(|steady| ((8, 80, "Slides.exe"), steady));
            assert_eq!(quiet.observe_steady(front, t0 + Duration::from_secs(secs)), None);
        }
    }

    #[test]
    fn composed_notice_needs_steady_presents() {
        assert!(!notice_wanted("game", rate(5), false), "a slideshow / idle window");
        assert!(!notice_wanted("game", rate(STEADY_PRESENTS - 1), false));
        assert!(notice_wanted("game", rate(STEADY_PRESENTS), false), "24 fps");
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
