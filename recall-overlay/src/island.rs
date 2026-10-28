//! The island state machine: which content shows (a notice beats the stats,
//! the stats beat hidden) and the animated frame that carries it.

use std::time::Instant;

use crate::anim::{ease_out, Frame, Tween, DURATION};
use crate::layout::StatsContent;
use crate::notice::NoticeView;
use crate::reducer::{OverlayView, RecorderState, Reducer};
use crate::session::Session;
use crate::stats::frame_timing::FpsSource;
use crate::stats::StatsSnapshot;

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
}

/// Hidden by default; a notice while one is active and notices are on
/// (`overlay.enabled`); the stats while `overlay_stats` is on and the target
/// game is in front (`game_in_front`; never over the desktop or other apps).
pub fn content(view: &OverlayView, session: &Session, game_in_front: bool) -> Option<Content> {
    if let Some(notice) = view.notice.as_ref().filter(|_| view.visible && session.notices) {
        return Some(Content::Notice(notice.clone()));
    }
    (session.overlay_stats && view.connected && game_in_front).then_some(Content::Stats)
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
}

impl Island {
    pub fn new(now: Instant) -> Self {
        Self {
            tween: Tween::settled(Frame { w: 0.0, h: 0.0, alpha: 0.0 }, now),
            content: None,
            shown: false,
            showing: false,
            swapped_at: now,
        }
    }

    /// Show `content` at `size` (physical px), or hide with `None`.
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
        if !self.showing {
            let start = if self.shown { self.tween.at(now) } else { Frame::collapsed(w, h) };
            self.tween = Tween::new(start, target, now);
            self.showing = true;
            self.shown = true;
        } else {
            self.tween.retarget(target, now);
        }
        if self.content.as_ref() != Some(&content) {
            self.content = Some(content);
            self.swapped_at = now;
        }
    }

    /// Advance; returns true when the window just finished hiding.
    pub fn tick(&mut self, now: Instant) -> bool {
        if !self.showing && self.shown && self.tween.done(now) {
            self.shown = false;
            self.content = None;
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

    /// Frames still need drawing at animation rate.
    pub fn animating(&self, now: Instant) -> bool {
        self.shown && (!self.tween.done(now) || self.content_alpha(now) < 1.0)
    }
}

#[cfg(test)]
#[path = "island_tests.rs"]
mod tests;
