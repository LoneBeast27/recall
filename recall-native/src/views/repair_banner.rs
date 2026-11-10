//! The recorder's repair notice with its Reinstall button (`services::repair`): its engine file is
//! older than the app, so it records no microphone or picked device until Recall is reinstalled.
use gpui::{AnyElement, Context, FontWeight, div, prelude::*, px};

use crate::repair;
use crate::{
    root::Root,
    theme::{self, c},
    ui::{self, Variant},
    views::pages::card,
};

impl Root {
    /// The notice and its Reinstall button, while the recorder asks for one (`session.repair`).
    pub(crate) fn repair_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let notice = repair::reinstall_notice(self.snap.session.as_ref()?.repair.as_ref())?;
        Some(
            card()
                .flex_row()
                .items_center()
                .gap(px(12.))
                .child(ui::dot(theme::WARNING, 8.))
                .child(div().flex_1().font_weight(FontWeight::MEDIUM).text_color(c(theme::TEXT)).child(notice.title.clone()))
                .child(ui::button("reinstall-recall", Variant::Primary).child(repair::BUTTON).on_click(cx.listener(|_, _, _, cx| {
                    if let Some(why) = repair::on_reinstall_click(repair::reinstall_from_notice) {
                        eprintln!("[recall-native] {why}");
                    }
                    cx.notify();
                })))
                .into_any_element(),
        )
    }
}
