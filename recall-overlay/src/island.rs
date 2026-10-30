//! The island state machine: which content shows (a notice beats the stats,
//! the stats beat hidden) and the animated look that carries it. From
//! nothing it pops in as a circle and unravels (`presence.rs`). An action
//! notice transforms the open pill: it springs into a larger pill (both
//! dimensions, one overshoot) with one light sweep in the notice's tone, then
//! eases back to the stats pill or rolls up and leaves. A clip save carries
//! the shutter ring (`badge.rs`).

use std::time::Instant;

use crate::anim::{ease_out, Frame, Tween, DURATION};
use crate::badge::{Badge, SaveMark, SWEEP_AFTER};
use crate::cue::{Cue, Sheen, CUE_DELAY};
use crate::layout::{notice_scene, tone_color, Measure, Scene, StatsContent};
use crate::notice::{NoticeView, SavePhase};
use crate::presence::{Look, Presence, Style, FADE_OUT};

/// The two-step shortcut hint appears without the circle: it answers a key
/// press and its options must be readable within the 1.5 s chord window
/// (the circle's text shows at ~0.6 s). `--export-frames` renders both.
pub const HINT_STYLE: Style = Style::Instant;
use crate::reducer::{OverlayView, RecorderState, Reducer};
use crate::session::Session;
use crate::stats::frame_timing::FpsSource;
use crate::stats::StatsSnapshot;

/// The larger notice pill is at least this much wider (1440p px) than the
/// stats pill it transforms from.
pub const GROW_W: f32 = 40.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    Notice(NoticeView),
    Stats,
}

impl Content {
    pub fn name(&self) -> &'static str {
        match self {
            Content::Notice(_) => "notice",
            Content::Stats => "stats",
        }
    }

    /// An action notice: shown with the transformation.
    pub fn emphasized(&self) -> bool {
        matches!(self, Content::Notice(notice) if notice.emphasis)
    }
}

/// Hidden by default; a notice while one is active and notices are on
/// (`overlay.enabled`); the stats while `overlay_stats` is on and the target
/// game is in front (`game_in_front`; never over the desktop or other apps).
pub fn content(view: &OverlayView, session: &Session, game_in_front: bool) -> Option<Content> {
    if let Some(notice) = view.notice.as_ref().filter(|_| view.visible && session.notices) {
        return Some(Content::Notice(notice.clone()));
    }
    stats_shown(view, session, game_in_front).then_some(Content::Stats)
}

/// The stats pill shows (or would, under a notice).
pub fn stats_shown(view: &OverlayView, session: &Session, game_in_front: bool) -> bool {
    session.overlay_stats && view.connected && game_in_front
}

/// The pill an action notice transforms into: its `scene` (laid out at the
/// grown scale) and at least `GROW_W` wider than the stats pill under it
/// (`under_w`), content centred. `s` is the island scale.
pub fn expanded(scene: &Scene, under_w: Option<f32>, s: f32) -> (f32, f32) {
    let floor = under_w.map_or(0.0, |w| (w + GROW_W * s).ceil());
    // 44 × (58 / 44) is 58.000004 in f32: snap, so the window is not a row taller.
    let snap = |v: f32| (v * 64.0).round() / 64.0;
    (scene.w.max(floor), snap(scene.h))
}

/// A notice laid out at island scale `s`: the scene, the scale it was laid
/// out at, and the island's size. An action notice lays out at `s * grow`
/// and sizes over the stats pill under it (`under_w`, asked for only then).
pub fn notice_layout(
    notice: &NoticeView,
    s: f32,
    grow: f32,
    under_w: impl FnOnce() -> Option<f32>,
    m: &dyn Measure,
) -> (Scene, f32, (f32, f32)) {
    if !notice.emphasis {
        let scene = notice_scene(notice, s, m);
        let size = (scene.w, scene.h);
        return (scene, s, size);
    }
    let scene = notice_scene(notice, s * grow, m);
    let size = expanded(&scene, under_w(), s);
    (scene, s * grow, size)
}

/// The stats island's numbers from the session, the replay buffer and a sample.
pub fn stats_content(reducer: &Reducer, stats: &StatsSnapshot) -> StatsContent {
    let session = reducer.session();
    let recording = if session.state.is_empty() {
        reducer.view().state == RecorderState::Ready
    } else {
        session.recording()
    };
    StatsContent {
        recording,
        paused: session.paused(),
        replay_secs: reducer.buffer_secs(),
        fps: stats.fps,
        low1: stats.low1,
        fps_note: stats.fps.is_none().then(|| fps_note(&session.fps_source, stats.fps_source)).flatten(),
        gpu: stats.gpu,
        gpu_temp: stats.gpu_temp,
        cpu: stats.cpu,
    }
}

/// Why there is no framerate, short enough for the "1% low" slot: only a
/// missing permission (the controller's `fps_source`, or the overlay's own
/// refused session) has a note; anything else is a plain "—".
fn fps_note(controller: &str, overlay: FpsSource) -> Option<String> {
    (controller == "no_permission" || overlay == FpsSource::NoPermission).then(|| "No frame timing".into())
}

pub struct Island {
    /// The open pill's size (content swaps and transformations animate here).
    tween: Tween,
    /// Appearing from nothing / leaving: the circle that unravels.
    presence: Presence,
    /// The content being drawn (kept through the hide animation).
    content: Option<Content>,
    /// The window is on screen (true until a hide animation finishes).
    shown: bool,
    /// The island is heading towards visible.
    showing: bool,
    /// When the drawn content last changed (drives its fade-in).
    swapped_at: Instant,
    /// The title line fades in from here (alone, when only it changed).
    title_since: Instant,
    /// The detail line likewise (a save's outcome with a new detail).
    detail_since: Instant,
    /// The mark fades in from here (it shows at once with a fresh circle).
    icon_since: Instant,
    /// The content's opacity when a hide began (it fades out from there).
    fade_from: (Instant, f32),
    /// The last action notice's light sweep (runs once).
    cue: Option<Cue>,
    /// A clip save's shutter ring.
    mark: Option<SaveMark>,
    /// How the shortcut hint appears from nothing (`HINT_STYLE`).
    pub(crate) hint_style: Style,
}

impl Island {
    pub fn new(now: Instant) -> Self {
        Self {
            tween: Tween::settled(Frame { w: 0.0, h: 0.0, alpha: 1.0 }, now),
            presence: Presence::hidden(now),
            content: None,
            shown: false,
            showing: false,
            swapped_at: now,
            title_since: now,
            detail_since: now,
            icon_since: now,
            fade_from: (now, 0.0),
            cue: None,
            mark: None,
            hint_style: HINT_STYLE,
        }
    }

    /// How `content` appears from nothing.
    fn style_for(&self, content: &Content) -> Style {
        match content {
            Content::Notice(notice) if notice.chord => self.hint_style,
            _ => Style::Unravel,
        }
    }

    /// Show `content` at `size` (physical px), or hide with `None`. From
    /// nothing the island pops in as a circle and unravels; while open, a
    /// change to an action notice springs into its size (carrying the current
    /// motion on) and anything else eases.
    pub fn update(&mut self, next: Option<(Content, f32, f32)>, now: Instant) {
        let Some((content, w, h)) = next else { return self.hide(now) };
        let target = Frame { w, h, alpha: 1.0 };
        let swapped = self.content.as_ref() != Some(&content);
        let fresh = !self.shown;
        let open = !fresh && self.showing && self.presence.progress(now) >= 1.0;
        if fresh {
            self.tween = Tween::settled(target, now);
            self.presence = Presence::hidden(now);
            self.icon_since = now.checked_sub(DURATION).unwrap_or(now);
        } else if swapped && open && content.emphasized() {
            self.tween = Tween::spring(self.tween.at(now), target, self.tween.velocity(now), now);
        } else {
            self.tween.retarget(target, now);
        }
        if !self.showing {
            self.presence.show(now, self.style_for(&content));
        }
        self.showing = true;
        self.shown = true;
        if swapped {
            self.swap(content, open, fresh, now);
        }
    }

    fn hide(&mut self, now: Instant) {
        if !self.showing {
            return;
        }
        let alpha = self.content_alpha(now);
        self.showing = false;
        self.fade_from = (now, alpha);
        self.presence.hide(now, FADE_OUT.mul_f32(alpha));
    }

    /// New content: its fade-in, the save mark, and an action's one sweep.
    /// A save moving on a stage (either outcome) fades in only its new title
    /// and, when it changed, its detail line; the mark, the pill and the
    /// rest carry on.
    fn swap(&mut self, content: Content, open: bool, fresh: bool, now: Instant) {
        let save = match &content {
            Content::Notice(notice) => notice.save,
            Content::Stats => None,
        };
        let continuing = save.is_some() && self.mark.is_some();
        match (save, self.mark.as_mut()) {
            (Some(phase), Some(mark)) => mark.follow(phase, now),
            (Some(phase), None) => self.mark = Some(SaveMark::new(phase, now)),
            (None, _) => self.mark = None,
        }
        if open && !continuing {
            self.icon_since = now;
        }
        if let Content::Notice(notice) = &content {
            let lands = notice.emphasis && save != Some(SavePhase::Saving);
            if lands && !self.cue.is_some_and(|cue| cue.running(now)) {
                // After the pill is open, and after a save's fill (whichever is later).
                let open = self.presence.opened_at().saturating_duration_since(now) + CUE_DELAY;
                let fill = if save.is_some() { SWEEP_AFTER } else { CUE_DELAY };
                self.cue = Some(Cue { start: now, color: tone_color(notice.tone), delay: open.max(fill) });
            }
        }
        let same_detail = matches!((&self.content, &content), (Some(Content::Notice(a)), Content::Notice(b)) if a.detail == b.detail);
        if continuing {
            self.title_since = now;
            if !same_detail {
                self.detail_since = now;
            }
        } else {
            // An instant appear shows its content with the window, no fade.
            let at_once = fresh && self.presence.text_ready().is_none();
            self.swapped_at = if at_once { now.checked_sub(DURATION).unwrap_or(now) } else { now };
            (self.title_since, self.detail_since) = (self.swapped_at, self.swapped_at);
        }
        self.content = Some(content);
    }

    /// Advance; returns true when the window just finished hiding.
    pub fn tick(&mut self, now: Instant) -> bool {
        if !self.showing && self.shown && self.presence.gone(now) {
            self.shown = false;
            self.content = None;
            self.cue = None;
            self.mark = None;
            return true;
        }
        false
    }

    /// The open pill's size (what the content is laid out for).
    pub fn frame(&self, now: Instant) -> Frame {
        self.tween.at(now)
    }

    /// What to draw now: the pill (a circle while appearing / leaving), the
    /// window around it and the opacities; `max_h` caps a spring's overshoot.
    pub fn look(&self, now: Instant, max_h: f32) -> Look {
        let frame = self.frame(now);
        let frame = Frame { h: frame.h.min(max_h), ..frame };
        let (pop, unroll) = self.presence.shape(now);
        let fade_in = |since: Instant| ease_out(now.saturating_duration_since(since).as_secs_f32() / DURATION.as_secs_f32());
        let content_alpha = self.content_alpha(now);
        let shaped = Look::shaped(frame, pop, unroll);
        Look {
            alpha: shaped.alpha * self.presence.window_alpha(now),
            content_alpha,
            title_alpha: content_alpha.min(fade_in(self.title_since)),
            detail_alpha: content_alpha.min(fade_in(self.detail_since)),
            icon_alpha: fade_in(self.icon_since),
            ..shaped
        }
    }

    pub fn shown(&self) -> bool {
        self.shown
    }

    pub fn content(&self) -> Option<&Content> {
        self.content.as_ref()
    }

    /// Opacity of the content inside the pill: it fades in once the pill is
    /// open (and again after a swap); a leaving island fades it out first.
    pub fn content_alpha(&self, now: Instant) -> f32 {
        if !self.showing {
            let (at, from) = self.fade_from;
            let fade = FADE_OUT.as_secs_f32() * from;
            let gone = if fade <= 0.0 { 1.0 } else { now.saturating_duration_since(at).as_secs_f32() / fade };
            return from * (1.0 - gone.min(1.0));
        }
        let start = match self.presence.text_ready() {
            Some(open) => self.swapped_at.max(open),
            None => self.swapped_at,
        };
        let since = now.checked_duration_since(start).map_or(0.0, |d| d.as_secs_f32());
        ease_out(since / DURATION.as_secs_f32())
    }

    /// The sweep's frame, while it crosses the pill.
    pub fn sheen(&self, now: Instant) -> Option<Sheen> {
        let cue = self.cue.filter(|_| self.shown)?;
        cue.progress(now).map(|progress| Sheen { progress, color: cue.color })
    }

    /// The shutter ring's frame while the content is a clip save.
    pub fn badge(&self, now: Instant) -> Option<Badge> {
        self.mark.filter(|_| self.shown).map(|mark| mark.badge(now))
    }

    /// Frames still need drawing at animation rate.
    pub fn animating(&self, now: Instant) -> bool {
        let sweeping = self.cue.is_some_and(|cue| cue.running(now));
        let ring = self.mark.is_some_and(|mark| mark.animating(now));
        let fading = if self.showing { self.content_alpha(now) < 1.0 } else { self.content_alpha(now) > 0.0 };
        let moving = self.presence.moving(now) || !self.tween.done(now);
        self.shown && (moving || fading || sweeping || ring || self.look(now, f32::INFINITY).icon_alpha < 1.0)
    }
}

#[cfg(test)]
#[path = "island_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "transform_tests.rs"]
mod transform_tests;

#[cfg(test)]
#[path = "save_tests.rs"]
mod save_tests;
