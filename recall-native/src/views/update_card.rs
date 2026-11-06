//! The update prompt: version, changelog and one-click install, drawn with Recall's own
//! panel, caption and button pieces over a dimmed window.
use gpui::{AnyElement, Context, FontWeight, div, prelude::*, px};

use crate::{
    root::Root,
    services::update::{NoteLine, Phase, current, note_lines},
    theme::{self, c, ca},
    ui,
};

impl Root {
    pub fn render_update(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (release, busy, error) = match &self.update.phase {
            Phase::Idle => return None,
            Phase::Offered(r) => (r, false, None),
            Phase::Downloading(r) => (r, true, None),
            Phase::Failed(r, e) => (r, false, Some(e.clone())),
        };
        let size_mb = (release.zip.size as f64 / 1_048_576.0).round() as u64;
        let meta = format!("You have {} · {size_mb} MB", current().unwrap_or("a dev build"));
        let notes = note_lines(&release.notes);
        let notes = div()
            .id("update-notes")
            .max_h(px(260.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(4.))
            .children(notes.into_iter().map(|line| match line {
                NoteLine::Heading(t) => div().pt(px(6.)).child(ui::caption(t)).into_any_element(),
                NoteLine::Bullet(t) => div()
                    .flex()
                    .gap(px(8.))
                    .child(div().pt(px(7.)).child(ui::dot(theme::TERTIARY, 4.)))
                    .child(div().min_w_0().text_color(c(theme::MUTED)).child(t))
                    .into_any_element(),
                NoteLine::Text(t) => div().text_color(c(theme::MUTED)).child(t).into_any_element(),
            }));
        let status = if busy {
            Some(("Downloading. Recall closes and reopens by itself.".to_owned(), theme::MUTED))
        } else {
            error.map(|e| (format!("Update failed: {e}"), theme::ERROR))
        };
        let actions = div()
            .flex()
            .gap(px(8.))
            .justify_end()
            .child(ui::button("update-skip", ui::Variant::Ghost).child("Skip this version").on_click(cx.listener(|this, _, _, cx| {
                this.update.skip_version();
                cx.notify();
            })))
            .child(ui::button("update-later", ui::Variant::Secondary).child("Later").on_click(cx.listener(|this, _, _, cx| {
                this.update.later();
                cx.notify();
            })))
            .child(
                ui::button("update-now", ui::Variant::Primary)
                    .child(if busy { "Downloading…" } else if status.is_some() { "Try again" } else { "Update now" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !busy {
                            this.update.install();
                            cx.notify();
                        }
                    })),
            );
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
                            .child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(meta)),
                    ),
            )
            .child(ui::hairline())
            .child(notes)
            .child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child("Recording stops for about a minute while Recall updates."))
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
}
