//! Source timeline: ruler, filmstrip, waveform lanes, cut regions, selected
//! segment with trim handles, and the signal-red playhead.
use std::path::PathBuf;

use gpui::{
    AnyElement, Bounds, Context, FontWeight, MouseButton, MouseDownEvent, ObjectFit, PathBuilder, Pixels, Window,
    canvas, div, fill, img, point, prelude::*, px, relative, size,
};

use crate::{
    editor_state::{Drag, short_timecode, timecode},
    root::Root,
    theme::{self, c, ca},
    ui::{self, Icon, icon},
    views::editor::track_name,
};

const GUTTER: f32 = 88.0;
const RULER_H: f32 = 22.0;
const VIDEO_H: f32 = 60.0;
const LANE_H: f32 = 40.0;
const LANE_GAP: f32 = 6.0;

impl Root {
    pub fn render_timeline(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let lanes = self.ed.info.as_ref().map_or(2, |i| i.audio.len());
        let tracks_h = RULER_H + LANE_GAP + VIDEO_H + lanes as f32 * (LANE_GAP + LANE_H);
        ui::panel()
            .flex_none()
            .flex()
            .flex_col()
            .child(self.render_tools(cx))
            .child(
                div()
                    .h(px(tracks_h + 20.))
                    .flex()
                    .pt(px(8.))
                    .pb(px(12.))
                    .pr(px(16.))
                    .child(self.render_gutter(lanes))
                    .child(self.render_tracks(tracks_h, cx)),
            )
            .into_any_element()
    }

    fn render_tools(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let segments = self.ed.plan.ranges.len();
        let kept = self.ed.plan.duration();
        let tool = |id: &'static str, kind: Icon, label: &'static str, key: &'static str| {
            div()
                .id(id)
                .h(px(30.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(7.))
                .rounded(px(6.))
                .cursor_pointer()
                .hover(|s| s.bg(c(theme::OVERLAY)))
                .child(icon(kind, 15., theme::MUTED))
                .child(label)
                .child(ui::kbd(key))
        };
        div()
            .h(px(44.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(2.))
            .px(px(12.))
            .border_b_1()
            .border_color(c(theme::BORDER_SUBTLE))
            .child(tool("t-split", Icon::Scissors, "Split", "C").on_click(cx.listener(|this, _, _, cx| {
                this.ed.split();
                cx.notify();
            })))
            .child(tool("t-in", Icon::SetIn, "Set in", "I").on_click(cx.listener(|this, _, _, cx| {
                this.ed.set_in();
                cx.notify();
            })))
            .child(tool("t-out", Icon::SetOut, "Set out", "O").on_click(cx.listener(|this, _, _, cx| {
                this.ed.set_out();
                cx.notify();
            })))
            .child(tool("t-del", Icon::Trash, "Delete", "Del").when(segments <= 1, |d| d.opacity(0.4)).on_click(cx.listener(|this, _, _, cx| {
                this.ed.delete_selected();
                cx.notify();
            })))
            .child(div().w(px(1.)).h(px(18.)).mx(px(6.)).bg(c(theme::BORDER)))
            .child(
                ui::icon_button("t-undo", Icon::Undo, 30., if self.ed.can_undo() { theme::MUTED } else { theme::DISABLED }, false).on_click(cx.listener(|this, _, _, cx| {
                    this.ed.undo();
                    cx.notify();
                })),
            )
            .child(
                ui::icon_button("t-redo", Icon::Redo, 30., if self.ed.can_redo() { theme::MUTED } else { theme::DISABLED }, false).on_click(cx.listener(|this, _, _, cx| {
                    this.ed.redo();
                    cx.notify();
                })),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .text_size(px(12.))
                    .text_color(c(theme::TERTIARY))
                    .child(format!("{segments} {} ·", if segments == 1 { "segment" } else { "segments" }))
                    .child(div().font_family(theme::mono()).text_color(c(theme::MUTED)).child(timecode(kept)))
                    .child("kept"),
            )
            .into_any_element()
    }

    fn render_gutter(&self, lanes: usize) -> AnyElement {
        let names: Vec<(String, bool)> = self
            .ed
            .info
            .as_ref()
            .map(|info| {
                info.audio
                    .iter()
                    .enumerate()
                    .map(|(i, t)| (track_name(&t.name, i), self.ed.setting(t.stream_index).is_some_and(|s| s.muted)))
                    .collect()
            })
            .unwrap_or_else(|| (0..lanes).map(|i| (format!("Audio {}", i + 1), false)).collect());
        let label = |kind: Icon, text: String, h: f32, dim: bool| {
            div()
                .h(px(h))
                .flex()
                .items_center()
                .gap(px(7.))
                .text_size(px(12.))
                .text_color(c(if dim { theme::TERTIARY } else { theme::MUTED }))
                .child(icon(kind, 14., if dim { theme::TERTIARY } else { theme::MUTED }))
                .child(div().truncate().child(text))
        };
        div()
            .w(px(GUTTER))
            .flex_none()
            .pl(px(16.))
            .flex()
            .flex_col()
            .gap(px(LANE_GAP))
            .child(div().h(px(RULER_H)))
            .child(label(Icon::Film, "Video".into(), VIDEO_H, false))
            .children(names.into_iter().map(|(name, muted)| label(if muted { Icon::VolumeX } else { Icon::Volume }, short_name(&name), LANE_H, muted)))
            .into_any_element()
    }

    fn render_tracks(&mut self, tracks_h: f32, cx: &mut Context<Self>) -> AnyElement {
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
            .child(self.render_ruler(duration))
            .child(self.render_filmstrip(lanes_top));

        if let Some(info) = self.ed.info.as_ref() {
            for (i, t) in info.audio.iter().enumerate() {
                let muted = self.ed.setting(t.stream_index).is_some_and(|s| s.muted);
                tracks = tracks.child(waveform_lane(t.waveform.clone(), audio_top + i as f32 * (LANE_H + LANE_GAP), muted));
            }
        }
        for (start, end) in cuts.iter().copied() {
            tracks = tracks.child(cut_region(frac(start), frac(end), lanes_top, end - start));
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

    fn render_ruler(&self, duration: f64) -> AnyElement {
        let step = ruler_step(duration);
        let minor = step / 5.0;
        let mut ruler = div().absolute().top_0().left_0().right_0().h(px(RULER_H));
        if self.ed.info.is_none() {
            return ruler.into_any_element();
        }
        let mut t = 0.0;
        let mut i = 0usize;
        while t <= duration + 1e-6 && i < 2000 {
            let major = i % 5 == 0;
            let x = (t / duration) as f32;
            ruler = ruler.child(
                div()
                    .absolute()
                    .bottom_0()
                    .left(relative(x))
                    .w(px(1.))
                    .h(px(if major { 8. } else { 4. }))
                    .bg(c(if major { theme::BORDER_STRONG } else { theme::BORDER })),
            );
            if major && x < 0.97 {
                ruler = ruler.child(
                    div()
                        .absolute()
                        .top_0()
                        .left(relative(x))
                        .pl(px(4.))
                        .font_family(theme::mono())
                        .text_size(px(10.))
                        .line_height(px(14.))
                        .text_color(c(theme::TERTIARY))
                        .child(ruler_label(t)),
                );
            }
            i += 1;
            t = i as f64 * minor;
        }
        ruler.into_any_element()
    }

    fn render_filmstrip(&self, top: f32) -> AnyElement {
        let frames = self.ed.visuals.filmstrip.clone();
        let strip = div()
            .absolute()
            .top(px(top))
            .left_0()
            .right_0()
            .h(px(VIDEO_H))
            .rounded(px(6.))
            .overflow_hidden()
            .bg(c(theme::RAISED))
            .flex();
        if frames.is_empty() {
            return strip
                .items_center()
                .justify_center()
                .text_size(px(12.))
                .text_color(c(theme::TERTIARY))
                .child(if self.ed.info.is_some() { "Preparing filmstrip…" } else { "" })
                .into_any_element();
        }
        strip
            .children(frames.into_iter().map(|path| img(PathBuf::from(path)).flex_1().min_w_0().h(px(VIDEO_H)).object_fit(ObjectFit::Cover)))
            .into_any_element()
    }
}

fn waveform_lane(peaks: Vec<f32>, top: f32, muted: bool) -> AnyElement {
    let color = if muted { theme::DISABLED } else { theme::MUTED };
    div()
        .absolute()
        .top(px(top))
        .left_0()
        .right_0()
        .h(px(LANE_H))
        .rounded(px(6.))
        .bg(c(theme::RAISED))
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

fn ruler_step(duration: f64) -> f64 {
    const STEPS: [f64; 10] = [1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0];
    STEPS.iter().copied().find(|s| duration / s <= 14.0).unwrap_or(600.0)
}

fn ruler_label(t: f64) -> String {
    let total = t.round() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

fn short_name(name: &str) -> String {
    match name {
        "Microphone" => "Mic".into(),
        "Voice chat" => "Voice".into(),
        other => other.to_owned(),
    }
}
