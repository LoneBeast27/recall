//! Library group rows: a header that folds its group on click, and a packed row of small
//! groups side by side, each under a slim label (`library_rows.rs` decides which is which).
use gpui::{AnyElement, Context, FontWeight, Window, div, prelude::*, px};

use super::{CONTENT_PAD, GAP};
use crate::{
    root::Root,
    services::library::{self, DayGroup},
    theme::{self, c},
    ui::{Icon, icon},
};

/// Height of a packed row's label line and the space under it, for the scroll bar.
pub(super) const PACKED_LABEL: f32 = 28.0;

/// "3 clips · 4:12" (the length once every clip's is known).
fn group_meta(g: &DayGroup) -> String {
    let count = g.items.len();
    let mut meta = format!("{count} {}", if count == 1 { "clip" } else { "clips" });
    if g.durations_complete && g.total_secs > 0.0 {
        meta.push_str(&format!(" · {}", library::format_duration(g.total_secs)));
    }
    meta
}

impl Root {
    pub(super) fn render_group_header(&mut self, group: usize, first: bool, cx: &mut Context<Self>) -> AnyElement {
        let Some(g) = self.lib.groups.get(group) else { return div().into_any_element() };
        let collapsed = self.lib.is_collapsed(group);
        let target = format!("group:{}", g.key);
        div()
            .px(px(CONTENT_PAD))
            .pt(px(if first { 0. } else { 16. }))
            .pb(px(12.))
            .child(
                div()
                    .id(("group-header", group))
                    .child(crate::trace::target(target))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .child(icon(if collapsed { Icon::ChevronRight } else { Icon::ChevronDown }, 14., theme::TERTIARY))
                    .child(div().font_family(theme::display()).text_size(px(16.)).line_height(px(20.)).font_weight(FontWeight::BOLD).child(g.label.clone()))
                    .child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(group_meta(g)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.lib.toggle_collapsed(group);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    /// Small groups in one row. Each keeps its own label above its tiles; a thin line in the
    /// gap marks where the next group starts. Takes no more width than a row of tiles.
    pub(super) fn render_packed(&mut self, segments: &[(usize, usize)], items: &[usize], window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (w, _) = self.tile_size();
        let mut at = 0;
        let mut children = Vec::with_capacity(segments.len());
        for (n, &(group, count)) in segments.iter().enumerate() {
            let Some(g) = self.lib.groups.get(group) else { continue };
            let (label, meta, single_day) = (g.label.clone(), group_meta(g), g.single_day);
            let end = (at + count).min(items.len());
            let tiles: Vec<AnyElement> = items[at..end].iter().map(|&i| self.render_tile(i, single_day, window, cx)).collect();
            at = end;
            let width = w * count as f32 + GAP * count.saturating_sub(1) as f32;
            children.push(
                div()
                    .relative()
                    .w(px(width))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .when(n > 0, |d| d.child(div().absolute().left(px(-GAP / 2.)).top(px(2.)).bottom(px(0.)).w(px(1.)).bg(c(theme::BORDER_SUBTLE))))
                    .child(
                        div()
                            .h(px(20.))
                            .mb(px(PACKED_LABEL - 20.))
                            .flex()
                            .items_baseline()
                            .gap(px(8.))
                            .child(div().min_w_0().truncate().font_family(theme::display()).text_size(px(14.)).line_height(px(20.)).font_weight(FontWeight::BOLD).child(label))
                            .child(div().flex_none().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(meta)),
                    )
                    .child(div().flex().gap(px(GAP)).children(tiles))
                    .into_any_element(),
            );
        }
        div().px(px(CONTENT_PAD)).pb(px(20.)).flex().gap(px(GAP)).children(children).into_any_element()
    }
}
