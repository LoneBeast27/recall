//! Sidebar SSD row under the clip-drive meter: cache used of its budget on the SSD, as one bar
//! split into kept copies, previews, heads and the rest, and a line naming each part. Sizes are in
//! binary units, the way Settings shows the budget: a 2 GiB budget reads "2 GB".
use gpui::{AnyElement, div, prelude::*, px};

use crate::{
    services::storage::SsdUsage,
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

const MIB: f64 = 1_048_576.0;
const GIB: f64 = 1_073_741_824.0;

/// "439 MB", "4.2 MB", "1.4 GB", "2 GB" (binary units, a trailing ".0" dropped).
fn size(bytes: u64) -> String {
    let mib = bytes as f64 / MIB;
    if mib >= 1000.0 {
        format!("{} GB", trim(format!("{:.1}", bytes as f64 / GIB)))
    } else if mib >= 10.0 {
        format!("{mib:.0} MB")
    } else {
        format!("{} MB", trim(format!("{mib:.1}")))
    }
}

fn trim(number: String) -> String {
    number.strip_suffix(".0").map(str::to_owned).unwrap_or(number)
}

/// "1.4 of 2 GB" (one unit when both share it), "439 MB of 2 GB" otherwise.
fn header(used: u64, budget: u64) -> String {
    let (used, budget) = (size(used), size(budget));
    let shared = used.strip_suffix(" GB").filter(|_| budget.ends_with(" GB"));
    format!("{} of {budget}", shared.unwrap_or(&used))
}

/// The part lines, skipping parts that hold nothing.
fn breakdown(usage: &SsdUsage) -> Vec<String> {
    let mut lines: Vec<(&str, u64)> = parts(usage).into_iter().take(3).map(|(label, bytes, _)| (label, bytes)).collect();
    lines.push(("Exports", usage.exports));
    lines.into_iter().filter(|(_, bytes)| *bytes > 0).map(|(label, bytes)| format!("{label} {}", size(bytes))).collect()
}

pub fn ssd_meter(usage: SsdUsage) -> AnyElement {
    let scale = usage.budget.max(usage.used()).max(1) as f32;
    let full = usage.budget > 0 && usage.used() as f32 >= usage.budget as f32 * 0.95;
    let bar = parts(&usage)
        .into_iter()
        .filter(|(_, bytes, _)| *bytes > 0)
        .map(|(_, bytes, colour)| div().h_full().w(gpui::relative(bytes as f32 / scale)).bg(c(if full { theme::WARNING } else { colour })));
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
                .child(div().flex_1().min_w_0().truncate().child("SSD cache"))
                .child(div().flex_none().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(header(usage.used(), usage.budget))),
        )
        .child(div().ml(px(26.)).h(px(3.)).rounded(px(2.)).overflow_hidden().flex().bg(c(theme::BORDER_SUBTLE)).children(bar))
        .child(div().ml(px(26.)).flex().flex_col().text_size(px(11.)).text_color(c(theme::TERTIARY)).children(breakdown(&usage)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_in_binary_units_like_settings() {
        assert_eq!(size(2 << 30), "2 GB", "a 2 GiB budget reads 2 GB");
        assert_eq!(size(1_525_532_770), "1.4 GB");
        assert_eq!(size(460_032_607), "439 MB");
        assert_eq!(size(4_404_019), "4.2 MB");
        assert_eq!(size(0), "0 MB");
        assert_eq!(size(1_065_470_878), "1 GB", "999+ MiB rounds up into GB, not 1016 MB");
    }

    #[test]
    fn the_header_is_compact() {
        assert_eq!(header(1_525_532_770, 2 << 30), "1.4 of 2 GB");
        assert_eq!(header(460_032_607, 2 << 30), "439 MB of 2 GB");
        assert_eq!(header(20 << 30, 20 << 30), "20 of 20 GB");
    }

    #[test]
    fn parts_holding_nothing_are_hidden() {
        let usage = SsdUsage { previews: 460_032_607, kept: 1_065_470_878, heads: 0, other: 29_285, budget: 2 << 30, exports: 0, free: None };
        assert_eq!(breakdown(&usage), ["Kept copies 1 GB", "Previews 439 MB"]);
        let usage = SsdUsage { heads: 5 << 20, exports: 3 << 30, ..usage };
        assert_eq!(breakdown(&usage), ["Kept copies 1 GB", "Previews 439 MB", "Heads 5 MB", "Exports 3 GB"]);
    }
}
