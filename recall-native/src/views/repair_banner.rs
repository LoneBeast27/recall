//! The recorder's repair notice with its Reinstall button (`services::repair`): its engine file is
//! older than the app, so it records no microphone or picked device until Recall is reinstalled.
use gpui::{AnyElement, Context, FontWeight, div, prelude::*, px};

use crate::repair;
use crate::{
    root::{Root, ToastKind},
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
                .child(ui::button("reinstall-recall", Variant::Primary).child(if repair::reinstalling() { repair::RUNNING } else { repair::BUTTON }).on_click(cx.listener(|this, _, _, cx| {
                    let text = repair::reinstall_from_notice(this.update_resume());
                    this.show_toast(ToastKind::Info, text, None); // started, or why not
                    cx.notify();
                })))
                .into_any_element(),
        )
    }
}
