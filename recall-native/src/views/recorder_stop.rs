//! Settings → Recorder → "Stop recorder" while the recorder finishes a clip (R7-overlay-1).
//! A plain stop waits for the save (clicking it again only joins that wait); while the
//! recorder reports `finishing_clip` the row says so and offers a separate "Stop now"
//! that sends `{"cmd":"quit","force":true}` after a confirm, losing the clip.
use gpui::{AnyElement, Context, FontWeight, div, prelude::*};

use crate::{
    root::Root,
    services::daemon::DaemonCommand,
    theme::{self, c},
    ui::{self, Icon, Variant, icon},
    views::pages::setting_row,
};

/// The row's label while the recorder finishes a clip.
pub const FINISHING: &str = "Finishing a clip…";
/// The button that stops at once.
pub const STOP_NOW: &str = "Stop now (lose the clip)";
/// The confirm question for it, and its two answers.
pub const STOP_NOW_QUESTION: &str = "Stop now? The clip being saved is lost.";
pub const STOP_NOW_CONFIRM: &str = "Stop and lose it";
pub const STOP_NOW_KEEP: &str = "Keep saving";

/// What the "Stop recorder" row shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopRow {
    /// The plain "Stop recorder" button.
    Stop,
    /// "Finishing a clip…" with a "Stop now (lose the clip)" button.
    Finishing,
    /// The confirm for "Stop now".
    Confirming,
}

/// The row for the recorder's `finishing_clip` and whether "Stop now" was clicked.
pub fn stop_row(finishing: bool, confirming: bool) -> StopRow {
    match (finishing, confirming) {
        (false, _) => StopRow::Stop,
        (true, false) => StopRow::Finishing,
        (true, true) => StopRow::Confirming,
    }
}

/// The confirm only lives while the clip is being finished.
pub fn keep_confirm(finishing: bool, confirming: bool) -> bool {
    finishing && confirming
}

impl Root {
    /// Settings → Recorder → "Quit Recall".
    pub(crate) fn render_stop_row(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let finishing = self.snap.session.as_ref().is_some_and(|s| s.finishing_clip);
        self.stop_confirm = keep_confirm(finishing, self.stop_confirm);
        let row = setting_row("Quit Recall", "Stops recording and closes the tray icon. A clip being saved is finished first.");
        let row = match stop_row(finishing, self.stop_confirm) {
            StopRow::Stop => row.child(ui::button("quit-recorder", Variant::Ghost).border_1().border_color(c(theme::BORDER)).child(icon(Icon::Power, 14., theme::MUTED)).child("Stop recorder").on_click(cx.listener(|this, _, _, cx| {
                // A 2nd click only joins the recorder's wait for the save (no `force`).
                this.send(DaemonCommand::Quit);
                cx.notify();
            }))),
            StopRow::Finishing => row.child(div().text_color(c(theme::MUTED)).child(FINISHING)).child(ui::button("quit-recorder-now", Variant::Ghost).border_1().border_color(c(theme::BORDER)).child(STOP_NOW).on_click(cx.listener(|this, _, _, cx| {
                this.stop_confirm = true;
                cx.notify();
            }))),
            StopRow::Confirming => row
                .child(div().font_weight(FontWeight::SEMIBOLD).child(STOP_NOW_QUESTION))
                .child(ui::button("quit-now-keep", Variant::Ghost).child(STOP_NOW_KEEP).on_click(cx.listener(|this, _, _, cx| {
                    this.stop_confirm = false;
                    cx.notify();
                })))
                .child(ui::button("quit-now-confirm", Variant::Primary).child(STOP_NOW_CONFIRM).on_click(cx.listener(|this, _, _, cx| {
                    this.stop_confirm = false;
                    this.send(DaemonCommand::QuitNow);
                    cx.notify();
                }))),
        };
        row.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clip_being_finished_offers_a_separate_stop_now_behind_a_confirm() {
        assert_eq!(stop_row(false, false), StopRow::Stop);
        assert_eq!(stop_row(false, true), StopRow::Stop, "the save ended: no stale confirm");
        assert_eq!(stop_row(true, false), StopRow::Finishing);
        assert_eq!(stop_row(true, true), StopRow::Confirming);
        assert!(!keep_confirm(false, true) && keep_confirm(true, true));
    }
}
