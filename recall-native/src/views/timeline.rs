//! Source timeline: ruler, filmstrip, waveform lanes, cut regions, selected
//! segment with trim handles, and the signal-red playhead.
//! The interactive tracks (lanes, regions, handles, playhead) are in `timeline_tracks.rs`;
//! the listen-only lane controls left of it in `timeline_listen.rs`.
use std::path::PathBuf;

use gpui::{AnyElement, Context, ObjectFit, div, img, prelude::*, px, relative};

use crate::{
    editor_state::timecode,
    root::Root,
    theme::{self, c},
    ui::{self, Icon, icon},
    views::editor::track_name,
};

const GUTTER: f32 = 104.0;
const RULER_H: f32 = 22.0;
const VIDEO_H: f32 = 60.0;
const LANE_H: f32 = 40.0;
const LANE_GAP: f32 = 6.0;

#[path = "timeline_tracks.rs"]
mod tracks;
#[path = "timeline_loading.rs"]
mod loading;
#[path = "timeline_listen.rs"]
mod listen;

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
                    .child(self.render_gutter(lanes, cx))
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
                .child(crate::trace::target(id))
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
            .when(self.ed.info.is_some(), |d| d.child(self.render_speed(cx)))
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

    /// Tiles at the clip's own aspect ratio, each showing the most eventful frame of its stretch
    /// (see `scene_strip`), with its left edge at that frame's exact time. The last is cut by the edge.
    fn render_filmstrip(&self, top: f32) -> AnyElement {
        let frames = &self.ed.visuals.filmstrip;
        let strip = div()
            .absolute()
            .top(px(top))
            .left_0()
            .right_0()
            .h(px(VIDEO_H))
            .rounded(px(6.))
            .overflow_hidden()
            .bg(c(theme::RAISED));
        if frames.is_empty() {
            return strip
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(12.))
                .text_color(c(theme::TERTIARY))
                .child(if self.ed.info.is_some() && self.ed.loading.grey.is_empty() { "Preparing filmstrip…" } else { "" })
                .into_any_element();
        }
        let aspect = Some(self.ed.visuals.aspect).filter(|a| a.is_finite() && *a > 0.2 && *a < 6.0).unwrap_or(16.0 / 9.0);
        let tile_w = VIDEO_H * aspect;
        // The strip spans the tracks area; before its first paint, assume a typical width.
        let width = self.ed.track_span.get().map(|(_, w)| w).filter(|w| *w > 1.0).unwrap_or(1200.0);
        let tiles = crate::backend::scene_strip::anchored_tiles(frames, self.ed.duration(), width, tile_w);
        strip
            .children(tiles.into_iter().map(|(index, x)| {
                img(PathBuf::from(&frames[index].path))
                    .absolute()
                    .top_0()
                    .left(px(x))
                    .w(px(tile_w))
                    .h(px(VIDEO_H))
                    .object_fit(ObjectFit::Cover)
            }))
            .into_any_element()
    }
}

fn ruler_step(duration: f64) -> f64 {
    const STEPS: [f64; 10] = [1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0];
    STEPS.iter().copied().find(|s| duration / s <= 14.0).unwrap_or(600.0)
}

fn ruler_label(t: f64) -> String {
    let total = t.round() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

pub(super) fn short_name(name: &str) -> String {
    match name {
        "Microphone" => "Mic".into(),
        "Voice chat" => "Voice".into(),
        other => other.to_owned(),
    }
}

impl Root {
    /// Speed of the selected segment (re-encode exports only).
    fn render_speed(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let lossless = self.ed.lossless();
        let current = self.ed.plan.ranges.get(self.ed.selected).map_or(1.0, |r| r.speed);
        let mut track = ui::segment_track().w(px(230.));
        for speed in [0.25, 0.5, 1.0, 2.0, 4.0] {
            let selected = (current - speed).abs() < 1e-6;
            track = track.child(ui::segment(gpui::SharedString::from(format!("speed-{speed}")), speed_label(speed), selected).on_click(cx.listener(move |this, _, _, cx| {
                if !this.ed.lossless() {
                    this.ed.set_speed(speed);
                }
                cx.notify();
            })));
        }
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .mr(px(14.))
            .when(lossless, |d| d.opacity(0.35))
            .child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child("Speed"))
            .child(track)
            .into_any_element()
    }
}

fn speed_label(speed: f64) -> String {
    match speed {
        s if (s - 0.25).abs() < 1e-6 => "¼×".into(),
        s if (s - 0.5).abs() < 1e-6 => "½×".into(),
        s => format!("{s}×"),
    }
}
