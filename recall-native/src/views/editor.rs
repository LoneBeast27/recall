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
/// Portrait: the page this much taller than wide switches to it, this much wider switches back,
/// so a window resized near square does not flip back and forth.
const PORTRAIT_ON: f32 = 1.05;
const PORTRAIT_OFF: f32 = 0.95;
/// Portrait: smallest heights before the export panel scrolls instead.
const PREVIEW_MIN_H: f32 = 200.0;
const PANEL_MIN_H: f32 = 160.0;
/// Portrait: the export panel puts its two sections side by side from this width.
const TWO_COLUMNS_W: f32 = 880.0;

/// The editor's layout for a page of `width` x `height`, given the current one.
pub fn portrait_layout(portrait: bool, width: f32, height: f32) -> bool {
    if width <= 0.0 || height <= 0.0 {
        return portrait;
    }
    if portrait { height > width * PORTRAIT_OFF } else { height > width * PORTRAIT_ON }
}

#[path = "editor_inspector.rs"]
mod inspector;

impl Root {
    pub fn render_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let area = self.ed.area.clone();
        let (width, height) = area.get().unwrap_or_else(|| {
            let size = window.viewport_size();
            (f32::from(size.width), f32::from(size.height))
        });
        self.ed.portrait = portrait_layout(self.ed.portrait, width, height);
        let portrait = self.ed.portrait;
        // The page measures itself; a size that calls for the other layout redraws at once.
        let measure = canvas(
            move |bounds, window, _| {
                let size = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                if area.get() != Some(size) {
                    area.set(Some(size));
                    if portrait_layout(portrait, size.0, size.1) != portrait {
                        window.refresh();
                    }
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0();
        let page = div().id("editor").flex_1().min_w_0().min_h_0().relative().flex().flex_col().gap(px(8.)).px(px(8.)).pb(px(8.)).child(measure);
        if !portrait {
            return page
                .child(div().flex_1().min_h_0().flex().gap(px(8.)).child(self.render_preview(cx)).child(self.render_inspector(window, cx, None)))
                .child(self.render_timeline(cx))
                .into_any_element();
        }
        // Portrait: video at full width as tall as its shape needs, then the transport and the
        // timeline, then the same export panel at its natural height (it scrolls when short).
        let aspect = self.ed.info.as_ref().filter(|i| i.width > 0 && i.height > 0).map_or(16.0 / 9.0, |i| i.width as f32 / i.height as f32);
        let preview_h = (width - 16.0).max(1.0) / aspect + TRANSPORT_H;
        page.child(div().flex_basis(px(preview_h)).flex_shrink().min_h(px(PREVIEW_MIN_H)).flex().child(self.render_preview(cx)))
            .child(div().flex_none().child(self.render_timeline(cx)))
            .child(self.render_inspector(window, cx, Some(width)))
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
                    .children(self.ed.preview_quality().then(|| div().px(px(6.)).rounded(px(4.)).bg(ca(theme::WINDOW, 0.6)).text_color(c(theme::MUTED)).child("Preview quality")))
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

#[cfg(test)]
mod layout_tests {
    use super::portrait_layout;

    #[test]
    fn a_tall_page_goes_portrait_and_a_near_square_resize_does_not_flip() {
        assert!(portrait_layout(false, 1410.0, 2468.0), "a portrait monitor");
        assert!(!portrait_layout(false, 2560.0, 1440.0), "a landscape monitor");
        assert!(!portrait_layout(false, 1000.0, 1040.0), "4% taller: stays landscape");
        assert!(portrait_layout(false, 1000.0, 1060.0), "6% taller: portrait");
        assert!(portrait_layout(true, 1000.0, 980.0), "2% wider: stays portrait");
        assert!(!portrait_layout(true, 1000.0, 940.0), "6% wider: back to landscape");
        assert!(portrait_layout(true, 0.0, 0.0), "no size yet: keep the current layout");
    }
}
