//! Timeline while a clip loads: the parts not on the SSD yet are greyed out under a slow moving
//! stripe, and the grey shrinks as the clip arrives.
use gpui::{AnyElement, Bounds, PathBuilder, Pixels, Window, canvas, div, point, prelude::*, px, relative};

use crate::{
    root::Root,
    theme::{self, c, ca},
};

/// Pixels the stripes move per second.
const STRIPE_SPEED: f32 = 18.0;
const STRIPE_GAP: f32 = 10.0;

impl Root {
    /// The grey parts, over the lanes from `top` down. Empty once the clip is all here.
    pub(super) fn render_loading(&self, top: f32, duration: f64) -> Vec<AnyElement> {
        let loading = &self.ed.loading;
        let moved = loading.since.map_or(0.0, |since| since.elapsed().as_secs_f32()) * STRIPE_SPEED;
        let widest = loading.grey.iter().map(|span| span.1 - span.0).fold(0.0, f64::max);
        let percent = (loading.share * 100.0).floor() as u32;
        let frac = |t: f64| (t / duration).clamp(0.0, 1.0) as f32;
        let mut labelled = false;
        loading
            .grey
            .iter()
            .map(|(start, end)| {
                let label = (!labelled && end - start >= widest && (end - start) / duration > 0.12).then(|| format!("Loading {percent}%"));
                labelled |= label.is_some();
                grey_span(frac(*start), frac(*end), top, moved, label)
            })
            .collect()
    }

    /// While the clip loads, the transport says how far it has got.
    pub(crate) fn loading_label(&self) -> Option<String> {
        let loading = &self.ed.loading;
        (loading.active() && !loading.grey.is_empty()).then(|| format!("Loading {}%", (loading.share * 100.0).floor() as u32))
    }
}

fn grey_span(start: f32, end: f32, top: f32, moved: f32, label: Option<String>) -> AnyElement {
    div()
        .absolute()
        .top(px(top))
        .bottom_0()
        .left(relative(start))
        .w(relative((end - start).max(0.0)))
        .bg(ca(theme::WINDOW, 0.72))
        .overflow_hidden()
        .child(
            canvas(
                |_, _, _| {},
                move |bounds: Bounds<Pixels>, _, window: &mut Window, _| {
                    let (x0, y0) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
                    let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                    // The parent's overflow_hidden clips the diagonals to the span.
                    let mut offset = -h - STRIPE_GAP + moved % STRIPE_GAP;
                    while offset < w {
                        let mut path = PathBuilder::stroke(px(1.));
                        path.move_to(point(px(x0 + offset), px(y0 + h)));
                        path.line_to(point(px(x0 + offset + h), px(y0)));
                        if let Ok(p) = path.build() {
                            window.paint_path(p, ca(theme::TEXT, 0.06));
                        }
                        offset += STRIPE_GAP;
                    }
                },
            )
            .absolute()
            .inset_0(),
        )
        .children(label.map(|text| {
            div().absolute().top(px(18.)).left_0().right_0().flex().justify_center().child(
                div()
                    .px(px(8.))
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .bg(c(theme::BASE))
                    .border_1()
                    .border_color(c(theme::BORDER))
                    .text_size(px(11.))
                    .text_color(c(theme::MUTED))
                    .whitespace_nowrap()
                    .child(text),
            )
        }))
        .into_any_element()
}
