//! Sidebar SSD row under the clip-drive meter: cache used of its budget on the SSD, as one bar
//! split into kept copies, previews, heads and the rest, and a line naming each part.
use gpui::{AnyElement, div, prelude::*, px};

use crate::{
    services::{library::format_bytes, storage::SsdUsage},
    theme::{self, c},
    ui::{Icon, icon},
};

/// (label, bytes, bar colour) for each part shown, largest share first in the bar's order.
fn parts(usage: &SsdUsage) -> [(&'static str, u64, u32); 4] {
    [
        ("Kept copies", usage.kept, theme::MUTED),
        ("Previews", usage.previews, theme::PLACEHOLDER),
        ("Heads", usage.heads, theme::BORDER_STRONG),
        ("Other", usage.other, theme::TOP),
    ]
}

pub fn ssd_meter(usage: SsdUsage) -> AnyElement {
    let scale = usage.budget.max(usage.used()).max(1) as f32;
    let full = usage.budget > 0 && usage.used() as f32 >= usage.budget as f32 * 0.95;
    let bar = parts(&usage)
        .into_iter()
        .filter(|(_, bytes, _)| *bytes > 0)
        .map(|(_, bytes, colour)| div().h_full().w(gpui::relative(bytes as f32 / scale)).bg(c(if full { theme::WARNING } else { colour })));
    let mut breakdown: Vec<String> = parts(&usage).into_iter().take(3).map(|(label, bytes, _)| format!("{label} {}", format_bytes(bytes))).collect();
    if usage.exports > 0 {
        breakdown.push(format!("Exports {}", format_bytes(usage.exports)));
    }
    div()
        .pt(px(8.))
        .px(px(10.))
        .pb(px(6.))
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .text_color(c(theme::MUTED))
                .child(icon(Icon::Drive, 16., theme::MUTED))
                .child(div().flex_1().child("SSD cache"))
                .child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(format!("{} / {}", format_bytes(usage.used()), format_bytes(usage.budget)))),
        )
        .child(div().ml(px(26.)).h(px(3.)).rounded(px(2.)).overflow_hidden().flex().bg(c(theme::BORDER_SUBTLE)).children(bar))
        .child(div().ml(px(26.)).flex().flex_col().text_size(px(11.)).text_color(c(theme::TERTIARY)).children(breakdown))
        .into_any_element()
}
