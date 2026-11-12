//! The update prompt: version, changelog and one-click install, drawn with Recall's own
//! panel, caption and button pieces over a dimmed window.
use gpui::{AnyElement, Context, FontWeight, div, prelude::*, px};

use crate::{
    root::Root,
    update::{CONFIRM_TEXT, NoteLine, Phase, RecorderState, Resume, current, note_lines},
    theme::{self, c, ca},
    ui,
};

impl Root {
    /// What the recorder is doing now, for the update hand-off.
    pub fn recorder_state(&self) -> RecorderState {
        let session = self.snap.session.as_ref();
        let capturing = session.is_some_and(|s| s.capturing());
        RecorderState {
            running: self.snap.connected,
            recording: capturing,
            long_on: session.is_some_and(|s| s.long_recording.active),
            game: capturing && session.and_then(|s| s.target.as_ref()).is_some_and(|t| !t.exe.is_empty()),
        }
    }

    /// The page, the open clip and the recorder's state: what the app reopens to after the update.
    pub fn update_resume(&self) -> Resume {
        let clip = (self.route == crate::root::Route::Editor).then(|| self.ed.info.as_ref().map(|i| i.path.clone()).or_else(|| self.ed.loading_path.clone())).flatten();
        Resume::new(self.route.name(), clip, self.recorder_state())
    }

    pub fn render_update(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (release, busy, error) = match &self.update.phase {
            Phase::Idle => return None,
            Phase::Offered(r) | Phase::Confirm(r) => (r, false, None),
            Phase::Downloading(r) => (r, true, None),
            Phase::Failed(r, e) => (r, false, Some(e.clone())),
        };
        let confirming = matches!(self.update.phase, Phase::Confirm(_));
        let size_mb = (release.size as f64 / 1_048_576.0).round() as u64;
        let have = current().unwrap_or("a dev build");
        let meta = match release.hotfix {
            Some(_) => format!("You have the earlier {have} build · {size_mb} MB"),
            None => format!("You have {have} · {size_mb} MB"),
        };
        let notes = note_lines(&release.notes);
        let notes = div()
            .id("update-notes")
            .max_h(px(260.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(4.))
            .children(notes.into_iter().map(|line| {
                match line {
                    NoteLine::Heading(t) => {
                        div().pt(px(6.)).child(ui::caption(t)).into_any_element()
                    }
                    NoteLine::Bullet(t) => div()
                        .flex()
                        .gap(px(8.))
                        .child(div().pt(px(7.)).child(ui::dot(theme::TERTIARY, 4.)))
                        .child(div().min_w_0().text_color(c(theme::MUTED)).child(t))
                        .into_any_element(),
                    NoteLine::Text(t) => div()
                        .text_color(c(theme::MUTED))
                        .child(t)
                        .into_any_element(),
                }
            }));
        let status = if busy {
            Some((
                "Downloading and checking Recall Setup…".to_owned(),
                theme::MUTED,
            ))
        } else {
            error.map(|e| (format!("Update failed: {e}"), theme::ERROR))
        };
        let hotfix = release.hotfix.is_some();
        let actions = if confirming { self.confirm_actions(cx) } else { self.offer_actions(busy, status.is_some(), hotfix, cx) };
        let card = ui::panel()
            .w(px(480.))
            .p(px(20.))
            .flex()
            .flex_col()
            .gap(px(14.))
            .bg(c(theme::RAISED))
            .border_color(c(theme::BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(crate::views::titlebar::mark(22.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .font_family(theme::display())
                                    .text_size(px(18.))
                                    .line_height(px(24.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format!("{} is ready", release.title)),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(c(theme::TERTIARY))
                                    .child(meta),
                            ),
                    ),
            )
            .child(ui::hairline())
            .child(notes)
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(c(theme::TERTIARY))
                    .child(if confirming { CONFIRM_TEXT } else { "Recall asks before it closes for the update." }),
            )
            .children(status.map(|(t, col)| div().text_size(px(12.)).text_color(c(col)).child(t)))
            .child(actions);
        Some(
            div()
                .id("update-scrim")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(ca(0x000000, 0.55))
                // The scrim takes the press so the page under it does not act.
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(card)
                .into_any_element(),
        )
    }

    /// The offer's buttons: skip, later, and the update (which asks first, `Phase::Confirm`).
    fn offer_actions(&self, busy: bool, failed: bool, hotfix: bool, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex()
            .gap(px(8.))
            .justify_end()
            .child(
                ui::button("update-skip", ui::Variant::Ghost)
                    .child(if hotfix { "Skip this hotfix" } else { "Skip this version" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.update.skip_version();
                        cx.notify();
                    })),
            )
            .child(
                ui::button("update-later", ui::Variant::Secondary)
                    .child("Later")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.update.later();
                        cx.notify();
                    })),
            )
            .child(
                ui::button("update-now", ui::Variant::Primary)
                    .child(if busy {
                        "Downloading…"
                    } else if failed {
                        "Try again"
                    } else {
                        "Download and open Setup"
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !busy {
                            this.update.confirm();
                            cx.notify();
                        }
                    })),
            )
    }

    /// The confirm step: nothing is downloaded until "Save and update".
    fn confirm_actions(&self, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex()
            .gap(px(8.))
            .justify_end()
            .child(
                ui::button("update-cancel", ui::Variant::Secondary)
                    .child("Cancel")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.update.cancel_confirm();
                        cx.notify();
                    })),
            )
            .child(
                ui::button("update-save", ui::Variant::Primary)
                    .child("Save and update")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let resume = this.update_resume();
                        this.update.install(resume);
                        cx.notify();
                    })),
            )
    }
}
