//! The editor inspector's Export footer (`editor_inspector.rs`).
#[allow(unused_imports)]
use super::*;

impl Root {
    pub(super) fn render_export_footer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let (status_icon, status) = if let Some(error) = &self.ed.save_error {
            (Icon::Alert, format!("Edits not saved: {error}"))
        } else if self.ed.saved {
            (Icon::Check, "Edits saved · original untouched".to_owned())
        } else {
            (Icon::Refresh, "Saving edits…".to_owned())
        };
        let exported = self.ed.exported.clone();
        div()
            .p(px(16.))
            .pt(px(12.))
            .border_t_1()
            .border_color(c(theme::BORDER_SUBTLE))
            .flex()
            .flex_col()
            .gap(px(10.))
            .when(self.ed.info.is_some(), |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_size(px(12.))
                        .text_color(c(theme::TERTIARY))
                        .child(icon(status_icon, 13., theme::TERTIARY))
                        .child(div().truncate().child(status)),
                )
            })
            .children(self.ed.export_error.clone().map(|e| div().text_size(px(12.)).text_color(c(theme::ERROR)).line_clamp(3).child(e)))
            .children(exported.map(|summary| {
                let path = summary.path.clone();
                let drag_path = summary.path.clone();
                div()
                    .id("exported")
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .p(px(8.))
                    .rounded(px(6.))
                    .bg(c(theme::RAISED))
                    .cursor_pointer()
                    .hover(|s| s.bg(c(theme::OVERLAY)))
                    .child(ui::dot(theme::ACTIVE, 7.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().font_weight(FontWeight::MEDIUM).child(match summary.fps { Some(fps) => format!("Exported · {fps} fps"), None => "Exported · lossless".to_owned() }))
                            .child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(format!(
                                "{:.2} MB · {} × {} · {}", summary.bytes as f64 / 1_000_000.0, summary.width, summary.height, library::format_duration(summary.seconds)
                            )))
                            .child(div().text_size(px(11.)).text_color(c(theme::TERTIARY)).child("Drag into Discord · click to show in folder")),
                    )
                    // A press that moves becomes a file drag of the export (Discord, Explorer, chats);
                    // a plain click shows it in Explorer.
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, _, _| {
                        this.drag_origin = Some((drag_path.clone(), f32::from(event.position.x), f32::from(event.position.y)));
                    }))
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                        if event.pressed_button != Some(MouseButton::Left) {
                            return;
                        }
                        if let Some((path, x0, y0)) = this.drag_origin.clone() {
                            let (x, y) = (f32::from(event.position.x), f32::from(event.position.y));
                            if (x - x0).abs() + (y - y0).abs() > 8.0 {
                                this.drag_origin = None;
                                this.drag_paths(vec![path], cx);
                            }
                        }
                    }))
                    .on_click(cx.listener(move |_, _, _, _| crate::backend::show_in_folder(&path)))
            }))
            .children(self.ed.pre_encode_line().filter(|_| !self.ed.exporting).map(|line| {
                let ready = self.ed.pre_encode_ready();
                div()
                    .id("pre-encode")
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(12.))
                    .text_color(c(if ready { theme::ACTIVE } else { theme::TERTIARY }))
                    .child(icon(if ready { Icon::Check } else { Icon::Refresh }, 13., if ready { theme::ACTIVE } else { theme::TERTIARY }))
                    .child(div().truncate().child(line))
            }))
            .child(
                ui::button("export", Variant::Primary)
                    .h(px(36.))
                    .w_full()
                    .when(self.ed.info.is_none() || self.ed.exporting, |b| b.opacity(0.5))
                    .child(icon(if self.ed.exporting { Icon::Refresh } else { Icon::Upload }, 15., theme::BASE))
                    .child(if self.ed.exporting { self.ed.export_label() } else { "Export clip".to_owned() })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.ed.export(&this.workers);
                        cx.notify();
                    })),
            )
            .when(self.ed.exporting, |d| {
                d.child(
                    ui::button("export-cancel", Variant::Ghost)
                        .h(px(32.))
                        .w_full()
                        .child(if self.ed.cancelling { "Cancelling…" } else { "Cancel export" })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.ed.cancel_export();
                            cx.notify();
                        })),
                )
            })
            .into_any_element()
    }
}
