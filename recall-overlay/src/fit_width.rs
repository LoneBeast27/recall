//! A horizontal notice never runs past its area: the detail line (then the
//! title) gives way with a "…" until the pill fits (portrait monitors, long
//! full-width game names). Side spots have their own limit (`stack_within`).

use crate::layout::Scene;
use crate::notice::NoticeView;
use crate::reducer::truncate;

type Laid = (Scene, f32, (f32, f32));

/// `layout` of `notice`, shortened until its width is at most `max_w`: the
/// longest detail that fits (a binary search over its length; a clip's
/// " · m:ss" stays whole, its name gives way), then the title if even one
/// detail character is too wide.
pub fn fit_notice(notice: &NoticeView, max_w: f32, layout: impl Fn(&NoticeView) -> Laid) -> Laid {
    let laid = layout(notice);
    if laid.2 .0 <= max_w {
        return laid;
    }
    // "<game> · m:ss": only the name shortens.
    let (head, tail) = match notice.detail.rsplit_once(" · ") {
        Some((head, length)) if length.contains(':') => (head, format!(" · {length}")),
        _ => (notice.detail.as_str(), String::new()),
    };
    let shortened = |detail: usize, title: usize| NoticeView {
        detail: format!("{}{tail}", truncate(head, detail)),
        title: truncate(&notice.title, title),
        ..notice.clone()
    };
    let title = notice.title.chars().count();
    let fits = |n: &NoticeView| layout(n).2 .0 <= max_w;
    let best = |len: usize, make: &dyn Fn(usize) -> NoticeView| {
        let (mut lo, mut hi) = (1, len); // the largest fitting length, else 1
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if fits(&make(mid)) { lo = mid } else { hi = mid - 1 }
        }
        lo
    };
    let detail = best(head.chars().count().max(1), &|k| shortened(k, title));
    let candidate = shortened(detail, title);
    if fits(&candidate) {
        return layout(&candidate);
    }
    let title = best(title.max(1), &|k| shortened(1, k));
    layout(&shortened(1, title))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::island::notice_layout;
    use crate::layout::{Measure, Role, TextMetrics};
    use crate::notice::Tone;
    use crate::placement::{scale, Rect, DEFAULT_SPOT, EDGE};

    /// Fixed glyph widths: ASCII 0.6 em, anything else (CJK) 1.2 em.
    struct Fake;

    impl Measure for Fake {
        fn measure(&self, role: Role, scale: f32, text: &str) -> TextMetrics {
            let em = role.size() * scale;
            let width = text.chars().map(|c| if c.is_ascii() { 0.6 } else { 1.2 }).sum::<f32>() * em;
            TextMetrics { width, height: em * 1.25, baseline: em * 0.95, cap: em * 0.7 }
        }
    }

    #[test]
    fn a_long_wide_name_on_a_portrait_monitor_stays_on_screen() {
        let monitor = Rect { left: 0, top: 0, right: 1080, bottom: 1920 };
        let s = scale(&monitor, &DEFAULT_SPOT);
        let detail: String = "原神星穹鉄道絶区零崩壊学園".chars().cycle().take(60).collect::<String>() + " · 0:30";
        let notice = NoticeView::new(Tone::Success, "Clip saved", detail, true).emphasized();
        let max = 1080.0 - 2.0 * EDGE * s;
        let (scene, _, (w, _)) = fit_notice(&notice, max, |n| notice_layout(n, s, 1.32, || None, &Fake));
        assert!(w <= max, "{w} > {max}");
        let text: String = scene.texts.iter().map(|t| t.text.as_str()).collect();
        assert!(text.contains('…') && text.ends_with("0:30"), "the cut is shown, the length kept: {text}");
    }

    #[test]
    fn a_notice_that_fits_is_left_alone() {
        let notice = NoticeView::new(Tone::Success, "Clip saved", "VALORANT · 0:30", true).emphasized();
        let layout = |n: &NoticeView| notice_layout(n, 1.0, 1.32, || None, &Fake);
        assert_eq!(fit_notice(&notice, 2000.0, layout).0, layout(&notice).0);
    }
}
