//! Timeline tracks: the interactive strip with waveform lanes, cut regions, the
//! selected segment's trim handles, bookmarks and the playhead.
use gpui::{
    AnyElement, Bounds, Context, FontWeight, MouseButton, MouseDownEvent, PathBuilder, Pixels, Window, canvas, div,
    fill, point, prelude::*, px, relative, size,
};

use super::{LANE_GAP, LANE_H, RULER_H, VIDEO_H, speed_label};
use crate::{
    editor_state::{Drag, short_timecode},
    root::Root,
    theme::{self, c, ca},
};

impl Root {
    pub(super) fn render_tracks(&mut self, tracks_h: f32, cx: &mut Context<Self>) -> AnyElement {
        let duration = self.ed.duration();
        let has_clip = self.ed.info.is_some();
        let frac = |t: f64| (t / duration).clamp(0.0, 1.0) as f32;
        let span = self.ed.track_span.clone();
        let ranges = self.ed.plan.ranges.clone();
        let selected = self.ed.selected.min(ranges.len().saturating_sub(1));
        let playhead = self.ed.position();
        let lanes_top = RULER_H + LANE_GAP;
        let audio_top = lanes_top + VIDEO_H + LANE_GAP;

        // Cut regions (gaps between kept ranges), painted with a hatch.
        let mut cuts = Vec::new();
        let mut cursor = 0.0;
        for r in &ranges {
            if r.start - cursor > 0.01 {
                cuts.push((cursor, r.start));
            }
            cursor = r.end;
        }
        if has_clip && duration - cursor > 0.01 {
            cuts.push((cursor, duration));
        }

        let mut tracks = div()
            .id("tracks")
            .flex_1()
            .min_w_0()
            .h(px(tracks_h))
            .relative()
            .cursor_pointer()
            .child(
                canvas(move |b, _, _| span.set(Some((f32::from(b.origin.x), f32::from(b.size.width)))), |_, _, _, _| {})
                    .absolute()
                    .inset_0(),
            )
            .child(crate::trace::target("tracks"))
            .child(self.render_ruler(duration))
            .child(self.render_filmstrip(lanes_top));

        if let Some(info) = self.ed.info.as_ref() {
            for (i, t) in info.audio.iter().enumerate() {
                let muted = self.ed.setting(t.stream_index).is_some_and(|s| s.muted);
                tracks = tracks.child(waveform_lane(t.waveform.clone(), audio_top + i as f32 * (LANE_H + LANE_GAP), muted, self.ed.loading.active()));
            }
        }
        for (start, end) in cuts.iter().copied() {
            tracks = tracks.child(cut_region(frac(start), frac(end), lanes_top, end - start));
        }
        tracks = tracks.children(self.render_loading(lanes_top, duration));
        if self.ed.lossless() {
            for i in 0..ranges.len() {
                if let Some(snapped) = self.ed.snapped_start(i).filter(|s| *s < ranges[i].start - 0.01) {
                    tracks = tracks.child(
                        div()
                            .absolute()
                            .top(px(lanes_top))
                            .h(px(VIDEO_H))
                            .left(relative(frac(snapped)))
                            .w(relative(frac(ranges[i].start) - frac(snapped)))
                            .bg(ca(theme::WARNING, 0.16))
                            .border_l_1()
                            .border_color(c(theme::WARNING)),
                    );
                }
            }
        }
        for r in ranges.iter().filter(|r| (r.speed - 1.0).abs() > 1e-6) {
            tracks = tracks.child(
                div()
                    .absolute()
                    .top(px(lanes_top + 4.))
                    .left(relative(frac(r.start)))
                    .ml(px(6.))
                    .px(px(5.))
                    .h(px(16.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .bg(ca(theme::WINDOW, 0.82))
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(speed_label(r.speed)),
            );
        }
        for (i, r) in ranges.iter().enumerate() {
            if i != selected {
                tracks = tracks.child(
                    div()
                        .absolute()
                        .top(px(lanes_top))
                        .h(px(VIDEO_H))
                        .left(relative(frac(r.start)))
                        .w(relative(frac(r.end) - frac(r.start)))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(ca(theme::TEXT, 0.22)),
                );
            }
        }
        if let Some(r) = ranges.get(selected).filter(|_| has_clip) {
            tracks = tracks.child(selected_segment(frac(r.start), frac(r.end), lanes_top, tracks_h - lanes_top, selected, cx));
        }
        for (n, mark) in self.ed.marks.clone().into_iter().enumerate() {
            tracks = tracks.child(
                div()
                    .id(("mark", n))
                    .absolute()
                    .top(px(RULER_H - 9.))
                    .left(relative(frac(mark)))
                    .ml(px(-5.))
                    .size(px(10.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .child(div().size(px(7.)).rounded(px(1.5)).bg(c(theme::TEXT)))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        this.ed.seek(mark);
                        this.ed.select_at(mark);
                        cx.stop_propagation();
                        cx.notify();
                    })),
            );
        }
        if has_clip {
            tracks = tracks.child(playhead_marker(frac(playhead), playhead, tracks_h));
        }
        tracks
            .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &MouseDownEvent, _, cx| {
                this.ed.begin_drag(Drag::Scrub);
                this.ed.drag_to(f32::from(event.position.x));
                cx.notify();
            }))
            .into_any_element()
    }
}

fn waveform_lane(peaks: Vec<f32>, top: f32, muted: bool, loading: bool) -> AnyElement {
    let color = if muted { theme::DISABLED } else { theme::MUTED };
    div()
        .absolute()
        .top(px(top))
        .left_0()
        .right_0()
        .h(px(LANE_H))
        .rounded(px(6.))
        .bg(c(theme::RAISED))
        .when(peaks.is_empty() && loading, |d| {
            d.flex().items_center().justify_center().text_size(px(12.)).text_color(c(theme::TERTIARY)).child("Loading sound…")
        })
        .child(
            canvas(
                |_, _, _| {},
                move |bounds: Bounds<Pixels>, _, window: &mut Window, _| {
                    let w = f32::from(bounds.size.width);
                    let h = f32::from(bounds.size.height);
                    if peaks.is_empty() || w < 4.0 {
                        return;
                    }
                    let pitch = 3.0;
                    let bars = (w / pitch).floor().max(1.0) as usize;
                    let max = peaks.iter().cloned().fold(0.0f32, f32::max).max(1e-4);
                    for b in 0..bars {
                        let lo = b * peaks.len() / bars;
                        let hi = ((b + 1) * peaks.len() / bars).max(lo + 1).min(peaks.len());
                        let peak = peaks[lo..hi].iter().cloned().fold(0.0f32, f32::max);
                        // Square-root scale keeps quiet tracks readable.
                        let amp = ((peak / max).sqrt() * (h - 6.0)).max(1.0);
                        let x = f32::from(bounds.origin.x) + b as f32 * pitch;
                        let y = f32::from(bounds.origin.y) + (h - amp) / 2.0;
                        window.paint_quad(fill(Bounds::new(point(px(x), px(y)), size(px(2.), px(amp))), c(color)));
                    }
                },
            )
            .size_full(),
        )
        .into_any_element()
}

fn cut_region(start: f32, end: f32, top: f32, seconds: f64) -> AnyElement {
    div()
        .absolute()
        .top(px(top))
        .bottom_0()
        .left(relative(start))
        .w(relative((end - start).max(0.0)))
        .bg(ca(theme::WINDOW, 0.78))
        .border_l_1()
        .border_r_1()
        .border_color(c(theme::BORDER))
        .overflow_hidden()
        .child(
            canvas(
                |_, _, _| {},
                |bounds: Bounds<Pixels>, _, window: &mut Window, _| {
                    let (x0, y0) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
                    let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                    // The parent's overflow_hidden clips the diagonals to the region.
                    let mut offset = -h;
                    while offset < w {
                        let mut path = PathBuilder::stroke(px(1.));
                        path.move_to(point(px(x0 + offset), px(y0 + h)));
                        path.line_to(point(px(x0 + offset + h), px(y0)));
                        if let Ok(p) = path.build() {
                            window.paint_path(p, ca(theme::TEXT, 0.07));
                        }
                        offset += 7.0;
                    }
                },
            )
            .absolute()
            .inset_0(),
        )
        .child(
            div().absolute().top(px(VIDEO_H / 2. - 9.)).left_0().right_0().flex().justify_center().child(
                div()
                    .px(px(6.))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .bg(c(theme::BASE))
                    .border_1()
                    .border_color(c(theme::BORDER))
                    .font_family(theme::mono())
                    .text_size(px(10.))
                    .text_color(c(theme::MUTED))
                    .whitespace_nowrap()
                    .child(format!("cut {:.1} s", seconds)),
            ),
        )
        .into_any_element()
}

fn selected_segment(start: f32, end: f32, top: f32, height: f32, index: usize, cx: &mut Context<Root>) -> AnyElement {
    let handle = |id: &'static str, left: bool| {
        div()
            .id(id)
            .absolute()
            .top(px(VIDEO_H / 2. - 14.))
            .when(left, |d| d.left(px(-5.)))
            .when(!left, |d| d.right(px(-5.)))
            .w(px(9.))
            .h(px(28.))
            .rounded(px(3.))
            .bg(c(theme::TEXT))
            .cursor_col_resize()
            .flex()
            .items_center()
            .justify_center()
            .child(div().w(px(1.)).h(px(12.)).bg(c(theme::BASE)))
            .child(crate::trace::target(id))
    };
    div()
        .absolute()
        .top(px(top - 1.))
        .h(px(height + 2.))
        .left(relative(start))
        .w(relative((end - start).max(0.0)))
        .rounded(px(7.))
        .border(px(1.5))
        .border_color(c(theme::TEXT))
        .child(handle("trim-start", true).on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, _, cx| {
            this.ed.begin_drag(Drag::TrimStart(index));
            cx.stop_propagation();
            cx.notify();
        })))
        .child(handle("trim-end", false).on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, _, cx| {
            this.ed.begin_drag(Drag::TrimEnd(index));
            cx.stop_propagation();
            cx.notify();
        })))
        .into_any_element()
}

fn playhead_marker(x: f32, seconds: f64, height: f32) -> AnyElement {
    div()
        .absolute()
        .top_0()
        .left(relative(x))
        .h(px(height))
        .w(px(0.))
        .child(div().absolute().top(px(16.)).bottom_0().left(px(-0.75)).w(px(1.5)).bg(c(theme::SIGNAL)))
        .child(
            div().absolute().top_0().left(px(-60.)).w(px(120.)).flex().justify_center().child(
                div()
                    .h(px(17.))
                    .px(px(5.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .bg(c(theme::SIGNAL))
                    .text_color(c(theme::ON_SIGNAL))
                    .font_family(theme::mono())
                    .font_weight(FontWeight::MEDIUM)
                    .text_size(px(10.))
                    .child(short_timecode(seconds)),
            ),
        )
        .into_any_element()
}
