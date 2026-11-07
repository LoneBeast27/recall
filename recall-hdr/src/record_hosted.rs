//! An identity Recall resolved itself (`WatchEvent::hosted`: a UWP frame
//! named by its hosted CoreWindow's process, G6) never gets the "fullscreen =
//! game" fallback. Before G6 every UWP frame was `ApplicationFrameHost`, which
//! is denied; resolving it must not turn every fullscreen Store app (Phone
//! Link, WhatsApp, Clipchamp) into a recorded game when the render check is
//! off (no trace rights) or could not start.
//!
//! So a hosted identity is a target only on positive evidence: a known game,
//! an override or `detect.fullscreen_games` (`skips_render_check`), the GPU
//! vendor's list, or a "game" verdict from the render check. Pure decisions;
//! `record_render` (record path) and `app_helpers::foreground_of` (HDR rules)
//! call them.

use crate::config::Config;
use crate::record_target::{classify_event, skips_render_check, Target};
use crate::watcher::WatchEvent;

/// A classified candidate and whether it needs the render check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Classified {
    pub target: Target,
    pub needs_check: bool,
}

/// [`classify_event`] plus the render-check bookkeeping and the hosted rule.
/// `can_check` = the render check can run for this pid (trace rights, no
/// failed start); `vendor` asks the GPU vendor's list (`None` while pending).
pub(crate) fn classify_checked(
    ev: &WatchEvent,
    cfg: &Config,
    available: bool,
    can_check: bool,
    vendor: impl FnOnce() -> Option<bool>,
) -> Option<Classified> {
    let target = classify_event(ev, cfg)?;
    // A Steam-library exe, an override, a `detect.fullscreen_games` entry or
    // a known game keeps the 300 ms rule with no check.
    let plain = !skips_render_check(ev, cfg);
    // The vendor is asked only when the check would run (without trace rights
    // every plain fullscreen window commits anyway), and for a hosted identity,
    // whose only other way in is the check.
    let vendor = (plain && (available || ev.hosted)).then(vendor).flatten();
    let needs_check = crate::record_render::needs_check(plain, vendor);
    if ev.hosted && needs_check && !can_check {
        return None;
    }
    Some(Classified { target, needs_check })
}

/// The render check had no probe for a due candidate (it would not start, or
/// trace rights went away): a plain window commits on today's rule, a hosted
/// identity does not (it needs a "game" verdict).
pub(crate) fn commits_without_probe(hosted: bool) -> bool {
    !hosted
}

/// The HDR rules' managed-monitor fullscreen flag for `ev`: a hosted identity
/// earns the fullscreen default only as a known game (an override still
/// applies through `rules::classify`).
pub(crate) fn hdr_fullscreen(ev: &WatchEvent) -> bool {
    ev.fullscreen_on_target && (!ev.hosted || crate::known_games::is_known_game(&ev.exe_stem, &ev.title))
}

#[cfg(test)]
#[path = "record_hosted_tests.rs"]
mod tests;
