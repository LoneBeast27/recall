//! Settings > Audio > "Merge audio": one full-width box under the source strips with three
//! equal setting cards (side by side, a column when the window is narrow), styled like the
//! strips: "Merge on export" (each clip's Merge switch default), "Mix level offset" (a gain
//! on the merged Mix track before the limiter, with a -12 to +12 dB stepper while on) and
//! "Prevent clipping" (the -1 dBFS look-ahead limiter). All saved in `editor.json`.
use gpui::{AnyElement, Context, Div, FontWeight, SharedString, Window, div, prelude::*, px};

use crate::{
    edit_model::MIX_OFFSET_STEP_DB,
    root::Root,
    services::editor_prefs::{MixBox, offset_step},
    theme::{self, c},
    ui::{self, Icon},
    views::pages::card,
};

/// Narrowest window (logical px) that keeps the three cards side by side: the page beside the
/// sidebar is then wide enough for three cards of about 200 px. A wrapping row is not used:
/// its parent would not grow for the second line.
const SIDE_BY_SIDE_MIN_W: f32 = 960.;

/// The stepper's value text: "+1.5 dB", "0 dB", "-12 dB".
pub(crate) fn offset_text(db: f32) -> String {
    let db = offset_step(db);
    let number = if db.fract() == 0.0 { format!("{}", db.abs() as i32) } else { format!("{:.1}", db.abs()) };
    match db {
        d if d > 0.0 => format!("+{number} dB"),
        d if d < 0.0 => format!("-{number} dB"),
        _ => "0 dB".to_owned(),
    }
}

/// One setting card: title, a one-line muted note and any `extra` on the left, `control` on
/// the right.
fn setting_card(title: &str, note: &str, extra: Option<AnyElement>, control: AnyElement) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .p(px(10.))
        .flex()
        .items_center()
        .gap(px(12.))
        .rounded(px(8.))
        .bg(c(theme::OVERLAY))
        .border_1()
        .border_color(c(theme::BORDER_SUBTLE))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(c(theme::TEXT)).child(title.to_owned()))
                .child(div().text_size(px(12.)).line_height(px(16.)).truncate().text_color(c(theme::TERTIARY)).child(note.to_owned()))
                .children(extra),
        )
        .child(control)
}

impl Root {
    pub(super) fn merge_audio_box(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let side_by_side = f32::from(window.viewport_size().width) >= SIDE_BY_SIDE_MIN_W;
        let merge = self.ed.merge_audio;
        let mix = self.ed.mix_box;
        let merge_switch = ui::switch("merge-audio", merge).child(crate::trace::target("merge-audio")).on_click(cx.listener(move |this, _, _, cx| {
            this.ed.set_merge_audio(!merge);
            cx.notify();
        }));
        let offset_switch = ui::switch("mix-offset", mix.offset_on).child(crate::trace::target("mix-offset")).on_click(cx.listener(move |this, _, _, cx| {
            this.ed.set_mix_box(MixBox { offset_on: !mix.offset_on, ..mix });
            cx.notify();
        }));
        let stepper = mix.offset_on.then(|| self.offset_stepper(mix, cx));
        let limiter_switch = ui::switch("mix-limiter", mix.limiter).child(crate::trace::target("mix-limiter")).on_click(cx.listener(move |this, _, _, cx| {
            this.ed.set_mix_box(MixBox { limiter: !mix.limiter, ..mix });
            cx.notify();
        }));
        card()
            .id("merge-audio-box")
            .relative()
            .child(crate::trace::target("merge-audio-box"))
            .gap(px(12.))
            .child(div().text_size(px(13.)).line_height(px(18.)).font_weight(FontWeight::SEMIBOLD).text_color(c(theme::TEXT)).child("Merge audio"))
            .child(
                div()
                    .flex()
                    .when(!side_by_side, |d| d.flex_col())
                    .gap(px(12.))
                    .child(setting_card("Merge on export", "Each clip's default", None, merge_switch.into_any_element()))
                    .child(setting_card("Mix level offset", "Gain on the merged track", stepper, offset_switch.into_any_element()))
                    .child(setting_card("Prevent clipping", "Keeps the mix under -1 dB", None, limiter_switch.into_any_element())),
            )
            .into_any_element()
    }

    /// The offset's "−  +1.5 dB  +" stepper, 0.5 dB a click, -12 to +12 dB.
    fn offset_stepper(&mut self, mix: MixBox, cx: &mut Context<Self>) -> AnyElement {
        let step = move |id: &'static str, kind: Icon, by: f32, cx: &mut Context<Self>| {
            let to = offset_step(mix.offset_db + by);
            let at_end = to == offset_step(mix.offset_db);
            ui::icon_button(SharedString::from(id), kind, 24., if at_end { theme::PLACEHOLDER } else { theme::TEXT }, false)
                .border_1()
                .border_color(c(theme::BORDER))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !at_end {
                        this.ed.set_mix_box(MixBox { offset_db: to, ..mix });
                        cx.notify();
                    }
                }))
        };
        div()
            .pt(px(6.))
            .flex()
            .items_center()
            .gap(px(4.))
            .child(step("mix-offset-down", Icon::Minus, -MIX_OFFSET_STEP_DB, cx))
            .child(div().w(px(52.)).text_center().text_size(px(12.)).text_color(c(theme::TEXT)).child(offset_text(mix.offset_db)))
            .child(step("mix-offset-up", Icon::Plus, MIX_OFFSET_STEP_DB, cx))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::offset_text;

    #[test]
    fn the_stepper_reads_in_half_db_steps_with_a_sign() {
        assert_eq!(offset_text(0.0), "0 dB");
        assert_eq!(offset_text(1.5), "+1.5 dB");
        assert_eq!(offset_text(-12.0), "-12 dB");
        assert_eq!(offset_text(40.0), "+12 dB");
        assert_eq!(offset_text(6.0), "+6 dB");
    }
}
