//! Left of the timeline: each lane's name and its listen-only controls (mute and a volume
//! bar). They change what the preview plays and nothing else; the export's gain, mute and
//! mix choice are on the right page (`editor_inspector.rs`).
use gpui::{AnyElement, Context, MouseButton, MouseDownEvent, canvas, div, prelude::*, px, relative};

use super::{GUTTER, LANE_GAP, LANE_H, RULER_H, VIDEO_H, short_name};
use crate::{
    editor_state::{
        Drag,
        monitor::{LISTEN_MAX, LISTEN_MIN},
    },
    root::Root,
    theme::{self, c},
    ui::{self, Icon, icon},
    views::editor::track_name,
};

impl Root {
    pub(super) fn render_gutter(&mut self, lanes: usize, cx: &mut Context<Self>) -> AnyElement {
        let lanes_info: Vec<(String, bool)> = self
            .ed
            .info
            .as_ref()
            .map(|info| info.audio.iter().enumerate().map(|(i, t)| (track_name(&t.name, i), self.ed.setting(t.stream_index).is_some_and(|s| s.muted))).collect())
            .unwrap_or_else(|| (0..lanes).map(|i| (format!("Audio {}", i + 1), false)).collect());
        let has_clip = self.ed.info.is_some();
        let mut gutter = div()
            .w(px(GUTTER))
            .flex_none()
            .pl(px(16.))
            .pr(px(8.))
            .flex()
            .flex_col()
            .gap(px(LANE_GAP))
            .child(div().h(px(RULER_H)).flex().items_end().text_size(px(10.)).text_color(c(theme::TERTIARY)).child("Listen"))
            .child(
                div()
                    .h(px(VIDEO_H))
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .text_size(px(12.))
                    .text_color(c(theme::MUTED))
                    .child(icon(Icon::Film, 14., theme::MUTED))
                    .child("Video"),
            );
        for (index, (name, export_muted)) in lanes_info.into_iter().enumerate() {
            gutter = gutter.child(self.listen_lane(index, &name, export_muted, has_clip, cx));
        }
        gutter.into_any_element()
    }

    fn listen_lane(&self, index: usize, name: &str, export_muted: bool, has_clip: bool, cx: &mut Context<Self>) -> AnyElement {
        let level = self.ed.listen_level(index);
        let span = self.ed.listen_spans.get(index).cloned();
        let fraction = ((level.gain_db - LISTEN_MIN) / (LISTEN_MAX - LISTEN_MIN)) as f32;
        let unity = ((0.0 - LISTEN_MIN) / (LISTEN_MAX - LISTEN_MIN)) as f32;
        let text = if export_muted { theme::TERTIARY } else { theme::MUTED };
        div()
            .h(px(LANE_H))
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(5.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .child(
                        ui::icon_button(("listen-mute", index), if level.muted { Icon::VolumeX } else { Icon::Volume }, 20., theme::MUTED, level.muted)
                            .child(crate::trace::target(format!("listen-mute-{index}")))
                            .when(has_clip, |b| {
                                b.on_click(cx.listener(move |this, _, _, cx| {
                                    this.ed.toggle_listen_mute(index);
                                    cx.notify();
                                }))
                            }),
                    )
                    .child(div().min_w_0().truncate().text_size(px(12.)).text_color(c(text)).child(short_name(name))),
            )
            .child(
                div()
                    .id(("listen-gain", index))
                    .h(px(10.))
                    .relative()
                    .cursor_pointer()
                    .when(level.muted, |d| d.opacity(0.4))
                    .children(span.map(|span| canvas(move |b, _, _| span.set(Some((f32::from(b.origin.x), f32::from(b.size.width)))), |_, _, _, _| {}).absolute().inset_0()))
                    .child(div().absolute().left_0().right_0().top(px(4.)).h(px(2.)).rounded(px(1.)).bg(c(theme::TOP)))
                    .child(div().absolute().top(px(2.)).left(relative(unity)).w(px(1.)).h(px(6.)).bg(c(theme::TERTIARY)))
                    .child(div().absolute().left_0().top(px(4.)).h(px(2.)).rounded(px(1.)).w(relative(fraction)).bg(c(theme::MUTED)))
                    .child(div().absolute().top(px(1.)).left(relative(fraction)).ml(px(-4.)).size(px(8.)).rounded(px(4.)).bg(c(theme::TEXT)))
                    .child(crate::trace::target(format!("listen-gain-{index}")))
                    .when(has_clip, |d| {
                        d.on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            if event.click_count >= 2 {
                                this.ed.set_listen_gain(index, 0.0);
                            } else {
                                this.ed.begin_drag(Drag::Listen(index));
                                this.ed.drag_to(f32::from(event.position.x));
                            }
                            cx.stop_propagation();
                            cx.notify();
                        }))
                    }),
            )
            .into_any_element()
    }
}
