//! Editor page: native preview stage + transport, inspector (audio, export), timeline.
use gpui::{AnyElement, Context, Window, canvas, div, prelude::*, px};

use crate::{
    editor_state::timecode,
    root::Root,
    theme::{self, c, ca},
    ui::{self, Icon, icon},
    views::library::empty_state,
};

const INSPECTOR_W: f32 = 300.0;
const TRANSPORT_H: f32 = 56.0;

#[path = "editor_inspector.rs"]
mod inspector;

impl Root {
    pub fn render_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("editor")
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(8.))
            .pb(px(8.))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .gap(px(8.))
                    .child(self.render_preview(cx))
                    .child(self.render_inspector(window, cx)),
            )
            .child(self.render_timeline(cx))
            .into_any_element()
    }

    fn render_preview(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let rect = self.ed.preview_rect.clone();
        let aspect = self.ed.info.as_ref().filter(|i| i.width > 0 && i.height > 0).map_or(16.0 / 9.0, |i| i.width as f32 / i.height as f32);
        let ready = self.ed.preview.as_ref().is_some_and(|p| p.is_ready());
        let message = if self.ed.info.is_none() {
            Some(if self.ed.status.is_empty() { "Choose a clip in the Library".to_owned() } else { self.ed.status.clone() })
        } else if !ready {
            Some(if self.ed.status.is_empty() { "Loading preview…".to_owned() } else { self.ed.status.clone() })
        } else {
            None
        };
        // No fill here: the native video window sits under GPUI's surface and must show through.
        let stage = div()
            .flex_1()
            .min_h_0()
            .relative()
            .child(
                canvas(
                    move |bounds, window, _| {
                        let scale = window.scale_factor();
                        let (bw, bh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                        let (w, h) = if bw / bh.max(1.0) > aspect { (bh * aspect, bh) } else { (bw, bw / aspect) };
                        let x = f32::from(bounds.origin.x) + (bw - w) / 2.0;
                        let y = f32::from(bounds.origin.y) + (bh - h) / 2.0;
                        rect.set(Some(((x * scale).round() as i32, (y * scale).round() as i32, (w * scale).round() as i32, (h * scale).round() as i32)));
                    },
                    |_, _, _, _| {},
                )
                .size_full(),
            )
            .children(message.map(|m| div().absolute().inset_0().child(empty_state(Icon::Film, &m, None))))
            .children(self.ed.loading_note().filter(|_| ready).map(|note| {
                div().absolute().bottom(px(12.)).left_0().right_0().flex().justify_center().child(
                    div().px(px(10.)).h(px(26.)).flex().items_center().rounded(px(6.)).bg(ca(theme::WINDOW, 0.86)).text_size(px(12.)).text_color(c(theme::MUTED)).child(note),
                )
            }));
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .rounded(px(10.))
            .border_1()
            .border_color(c(theme::BORDER_SUBTLE))
            .child(stage)
            .child(self.render_transport(cx))
            .into_any_element()
    }

    fn render_transport(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let pos = self.ed.position();
        let edited = self.ed.edited_time(pos);
        let total = self.ed.plan.duration();
        let playing = self.ed.playing;
        div()
            .h(px(TRANSPORT_H))
            .flex_none()
            .bg(c(theme::BASE))
            .rounded_b(px(10.))
            .border_t_1()
            .border_color(c(theme::BORDER_SUBTLE))
            .flex()
            .items_center()
            .px(px(16.))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_baseline()
                    .gap(px(6.))
                    .child(div().font_family(theme::mono()).text_size(px(15.)).line_height(px(20.)).child(timecode(edited)))
                    .child(div().font_family(theme::mono()).text_size(px(12.)).text_color(c(theme::TERTIARY)).child(format!("/ {}", timecode(total)))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(ui::icon_button("back5", Icon::Back5, 32., theme::MUTED, false).on_click(cx.listener(|this, _, _, cx| {
                        this.ed.nudge(-5.0);
                        cx.notify();
                    })))
                    .child(
                        div()
                            .id("play")
                            .size(px(38.))
                            .rounded(px(19.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(c(theme::TEXT))
                            .cursor_pointer()
                            .hover(|s| s.bg(c(0xffffff)))
                            .child(icon(if playing { Icon::Pause } else { Icon::Play }, 16., theme::BASE))
                            .child(crate::trace::target("play"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.ed.toggle_play();
                                cx.notify();
                            })),
                    )
                    .child(ui::icon_button("fwd5", Icon::Forward5, 32., theme::MUTED, false).on_click(cx.listener(|this, _, _, cx| {
                        this.ed.nudge(5.0);
                        cx.notify();
                    }))),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .justify_end()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(12.))
                    .text_color(c(theme::TERTIARY))
                    .children(self.loading_label())
                    .child("Source")
                    .child(div().font_family(theme::mono()).text_color(c(theme::MUTED)).child(timecode(pos))),
            )
            .into_any_element()
    }
}

/// Recall recordings carry Game then Mic; other sources keep their tag or a number.
pub fn track_name(tag: &str, index: usize) -> String {
    let lower = tag.to_lowercase();
    if lower.contains("game") {
        "Game".into()
    } else if lower.contains("mic") {
        "Microphone".into()
    } else if lower.contains("voice") || lower.contains("discord") {
        "Voice chat".into()
    } else if !tag.trim().is_empty() && lower != "und" {
        tag.to_owned()
    } else {
        format!("Audio {}", index + 1)
    }
}
