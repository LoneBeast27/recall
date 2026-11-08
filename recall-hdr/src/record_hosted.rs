//! Official proof for a fullscreen-rule candidate (Rohan 2026-11-07 07:46
//! UTC: "use official proof + support for manual application registering").
//! A window `classify_event` made a candidate is recorded on its own proof:
//! a Steam or launcher install, an Xbox / Store game package, an override or
//! `detect.fullscreen_games` entry, a known game (`skips_render_check`), the
//! GPU vendor's list, or a "game" render verdict where the check can run.
//! With no verdict possible (no trace rights, or the check would not start
//! for the pid) and none of the rest, it is not recorded: [`Checked::NoVerdict`]
//! (the record path names it once, "Not recording <exe>: add it as a game in
//! Settings"). There is no "any fullscreen app is a game" fallback any more,
//! for a plain window or for an identity Recall resolved itself
//! (`WatchEvent::hosted`, a UWP frame's CoreWindow process, G6).
//! Pure decisions; `record_render` (record path) and
//! `app_helpers::foreground_of` (HDR rules) call them.

use crate::config::Config;
use crate::record_target::{classify_event, skips_render_check, Target};
use crate::watcher::WatchEvent;

/// A classified candidate and whether it needs the render check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Classified {
    pub target: Target,
    pub needs_check: bool,
}

/// What the proof rule says about a foreground window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Checked {
    /// A candidate: proven, or the render check decides.
    Candidate(Classified),
    /// It would be a candidate, but nothing proves it a game and the render
    /// check cannot run for it: not recorded (the G4 notice names it).
    NoVerdict,
    /// Not a candidate at all (denied, private, `none`, windowed, ...).
    No,
}

/// [`classify_event`] plus the proof rule. `can_check` = the render check
/// can run for this pid (trace rights, no failed start); `vendor` asks the GPU
/// vendor's list (`None` while pending), only for a window with no other proof.
pub(crate) fn check_proof(ev: &WatchEvent, cfg: &Config, can_check: bool, vendor: impl FnOnce() -> Option<bool>) -> Checked {
    let Some(target) = classify_event(ev, cfg) else { return Checked::No };
    // A Steam/launcher/Store install, an override, a `detect.fullscreen_games`
    // entry or a known game keeps the 300 ms rule with no check.
    let plain = !skips_render_check(ev, cfg);
    let vendor = plain.then(vendor).flatten();
    let needs_check = crate::record_render::needs_check(plain, vendor);
    if needs_check && !can_check {
        return Checked::NoVerdict;
    }
    Checked::Candidate(Classified { target, needs_check })
}

/// [`check_proof`] as an option: `None` = not a candidate, or no verdict.
/// `_available` (trace rights at all) is folded into `can_check` (tests).
#[cfg(test)]
pub(crate) fn classify_checked(
    ev: &WatchEvent,
    cfg: &Config,
    _available: bool,
    can_check: bool,
    vendor: impl FnOnce() -> Option<bool>,
) -> Option<Classified> {
    match check_proof(ev, cfg, can_check, vendor) {
        Checked::Candidate(c) => Some(c),
        Checked::NoVerdict | Checked::No => None,
    }
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
