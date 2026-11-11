//! The island's lines in the anonymous action trace (`recall_trace`): why it shows or hides,
//! and whether Windows let it draw and stay on top. Change-only, like the island log, and
//! without content: no program names, titles or the window above it, only states and codes.

use recall_trace::{act_on_change, V};

use crate::island::Content;
use crate::reducer::OverlayView;
use crate::session::Session;

/// What the island shows and the inputs that decided it.
pub fn state(content: Option<&Content>, view: &OverlayView, session: &Session, game_in_front: bool) {
    let shows = match content {
        Some(Content::Notice(_)) => "notice",
        Some(Content::Stats) => "stats",
        Some(Content::Leader(_)) => "hint",
        None => "nothing",
    };
    act_on_change(
        "island.state",
        &[
            ("shows", V::W(shows)),
            ("notice", view.notice.is_some().into()),
            ("visible", view.visible.into()),
            ("notices_on", session.notices.into()),
            ("stats_on", session.overlay_stats.into()),
            ("connected", view.connected.into()),
            ("game_in_front", game_in_front.into()),
            ("desktop", session.desktop.into()),
        ],
    );
}

/// The window was shown or hidden: `ok` false when Windows did not follow.
pub fn window(shown: bool, ok: bool, error: Option<&windows::core::Error>) {
    let mut fields = vec![("shown", V::B(shown)), ("ok", V::B(ok))];
    if let Some(error) = error {
        fields.push(("hr", V::Code(error.code().0 as u32)));
    }
    act_on_change("island.window", &fields);
}

/// A frame reached the screen, or `UpdateLayeredWindow` failed with `error`.
pub fn present(error: Option<&windows::core::Error>) {
    match error {
        None => act_on_change("island.present", &[("ok", V::B(true))]),
        Some(e) => act_on_change("island.present", &[("ok", V::B(false)), ("hr", V::Code(e.code().0 as u32))]),
    }
}

/// Another window covered the island and it raised itself (`ok` false: the raise failed).
/// Change-only too: a window that keeps covering it would otherwise write every frame.
pub fn raised(ok: bool) {
    act_on_change("island.raised", &[("ok", V::B(ok))]);
}
