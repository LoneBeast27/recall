//! The island state machine: which content shows (a notice beats the stats,
//! the stats beat hidden) and the animated frame that carries it. An action
//! notice transforms the island: it springs into a larger pill (both
//! dimensions, one overshoot) with one light sweep in the notice's tone, then
//! eases back to the stats pill or collapses away.

use std::time::Instant;

use crate::anim::{ease_out, Frame, Tween, DURATION};
use crate::cue::{Cue, Sheen};
use crate::layout::{notice_scene, tone_color, Measure, Scene, StatsContent};
use crate::notice::NoticeView;
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
    tween: Tween,
    /// The content being drawn (kept through the hide animation).
    content: Option<Content>,
    /// The window is on screen (true until a hide animation finishes).
    shown: bool,
    /// The island is heading towards visible.
    showing: bool,
    /// When the drawn content last changed (drives its fade-in).
    swapped_at: Instant,
    /// The last action notice's light sweep (runs once).
    cue: Option<Cue>,
}

impl Island {
    pub fn new(now: Instant) -> Self {
        Self {
            tween: Tween::settled(Frame { w: 0.0, h: 0.0, alpha: 0.0 }, now),
            content: None,
            shown: false,
            showing: false,
            swapped_at: now,
            cue: None,
        }
    }

    /// Show `content` at `size` (physical px), or hide with `None`. A change
    /// to an action notice springs into its size (carrying the current
    /// motion on) and starts its sweep; anything else eases.
    pub fn update(&mut self, next: Option<(Content, f32, f32)>, now: Instant) {
        let Some((content, w, h)) = next else {
            if self.showing {
                self.showing = false;
                let to = self.tween.target();
                self.tween.retarget(Frame::collapsed(to.w, to.h), now);
            }
            return;
        };
        let target = Frame { w, h, alpha: 1.0 };
        let swapped = self.content.as_ref() != Some(&content);
        let spring = swapped && content.emphasized();
        let from = if self.shown { self.tween.at(now) } else { Frame::collapsed(w, h) };
        if spring {
            let velocity = if self.shown { self.tween.velocity(now) } else { (0.0, 0.0) };
            self.tween = Tween::spring(from, target, velocity, now);
        } else if !self.showing {
            self.tween = Tween::new(from, target, now);
        } else {
            self.tween.retarget(target, now);
        }
        self.showing = true;
        self.shown = true;
        // One sweep at a time: a notice landing mid-sweep lets it finish.
        if let (true, Content::Notice(notice)) = (spring, &content) {
            if !self.cue.is_some_and(|cue| cue.running(now)) {
                self.cue = Some(Cue { start: now, color: tone_color(notice.tone) });
            }
        }
        if swapped {
            self.content = Some(content);
            self.swapped_at = now;
        }
    }

    /// Advance; returns true when the window just finished hiding.
    pub fn tick(&mut self, now: Instant) -> bool {
        if !self.showing && self.shown && self.tween.done(now) {
            self.shown = false;
            self.content = None;
            self.cue = None;
            return true;
        }
        false
    }

    pub fn frame(&self, now: Instant) -> Frame {
        self.tween.at(now)
    }

    pub fn shown(&self) -> bool {
        self.shown
    }

    pub fn content(&self) -> Option<&Content> {
        self.content.as_ref()
    }

    /// Opacity of the content inside the pill: new content fades in with the
    /// resize; a hiding island fades its content out with the pill.
    pub fn content_alpha(&self, now: Instant) -> f32 {
        if !self.showing {
            return self.frame(now).alpha;
        }
        let since = now.saturating_duration_since(self.swapped_at).as_secs_f32();
        ease_out(since / DURATION.as_secs_f32())
    }

    /// The sweep's frame, while it crosses the pill.
    pub fn sheen(&self, now: Instant) -> Option<Sheen> {
        let cue = self.cue.filter(|_| self.shown)?;
        cue.progress(now).map(|progress| Sheen { progress, color: cue.color })
    }

    /// Frames still need drawing at animation rate.
    pub fn animating(&self, now: Instant) -> bool {
        let sweeping = self.cue.is_some_and(|cue| cue.running(now));
        self.shown && (!self.tween.done(now) || self.content_alpha(now) < 1.0 || sweeping)
    }
}

#[cfg(test)]
#[path = "island_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "transform_tests.rs"]
mod transform_tests;
