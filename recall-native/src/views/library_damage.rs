//! Library page notice after library.json was found damaged (deep-audit round 2, R2-2):
//! how many favourites came back, where the damaged copy is, and "Keep going", the only
//! thing that resumes background saves and the storage limit (a star edit alone does not).
use gpui::{AnyElement, Context, FontWeight, div, prelude::*, px};

use crate::{
    root::{Root, ToastKind},
    theme::{self, c},
    ui::{self, Variant},
    views::pages::card,
};

impl Root {
    pub(super) fn render_library_damage(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let damage = self.lib.store.damage()?.clone();
        let noun = if damage.recovered == 1 { "favourite" } else { "favourites" };
        let kept = damage.backup.as_ref().and_then(|b| b.file_name()).map(|n| format!(" The damaged file is kept as {}.", n.to_string_lossy())).unwrap_or_default();
        let detail = format!("Check your stars, then keep going. Until then the storage limit removes nothing.{kept}");
        let backup = damage.backup.clone();
        Some(
            div()
                .flex_none()
                .px(px(24.))
                .pb(px(12.))
                .child(
                    card()
                        .flex_row()
                        .items_center()
                        .gap(px(12.))
                        .child(ui::dot(theme::WARNING, 8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(div().font_weight(FontWeight::MEDIUM).child(format!("Library data was damaged: {} {noun} recovered", damage.recovered)))
                                .child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(detail)),
                        )
                        .children(backup.map(|path| {
                            ui::button("damage-show", Variant::Ghost).child("Show file").on_click(move |_, _, _| crate::backend::show_in_folder(&path.to_string_lossy()))
                        }))
                        .child(ui::button("damage-keep-going", Variant::Primary).child("Keep going").on_click(cx.listener(|this, _, _, cx| {
                            match this.lib.store.keep_going() {
                                Ok(()) => this.show_toast(ToastKind::Good, "Library data saved", Some("The storage limit is back on".into())),
                                Err(error) => this.show_toast(ToastKind::Bad, "Couldn't save library data", Some(error)),
                            }
                            cx.notify();
                        }))),
                )
                .into_any_element(),
        )
    }
}
