//! Buttons, keycaps, switches, segmented controls and panels. Handlers are
//! attached by the caller (`.on_click(cx.listener(..))`) so these stay view-agnostic.
use gpui::{Div, ElementId, FontWeight, SharedString, Stateful, div, prelude::*, px};

use super::icon::{Icon, icon};
use crate::theme::{self, c};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// White fill, black text: the one main action on a surface.
    Primary,
    Secondary,
    Ghost,
}

pub fn button(id: impl Into<ElementId>, variant: Variant) -> Stateful<Div> {
    let id = id.into();
    let base = div()
        .child(crate::trace::target(id.to_string()))
        .id(id)
        .h(px(32.))
        .px(px(12.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .rounded(px(6.))
        .text_size(px(13.))
        .line_height(px(18.))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer();
    match variant {
        Variant::Primary => base
            .bg(c(theme::TEXT))
            .text_color(c(theme::BASE))
            .font_weight(FontWeight::SEMIBOLD)
            .hover(|s| s.bg(c(0xffffff)))
            .active(|s| s.bg(c(theme::MUTED))),
        Variant::Secondary => base
            .bg(c(theme::OVERLAY))
            .border_1()
            .border_color(c(theme::BORDER))
            .text_color(c(theme::TEXT))
            .hover(|s| s.bg(c(theme::FLOATING)))
            .active(|s| s.bg(c(theme::TOP))),
        Variant::Ghost => base
            .text_color(c(theme::MUTED))
            .hover(|s| s.bg(c(theme::OVERLAY)).text_color(c(theme::TEXT)))
            .active(|s| s.bg(c(theme::FLOATING))),
    }
}

/// Square icon-only button. `pressed` shows the latched state (e.g. muted).
pub fn icon_button(id: impl Into<ElementId>, kind: Icon, size: f32, color: u32, pressed: bool) -> Stateful<Div> {
    let id = id.into();
    let base = div()
        .child(crate::trace::target(id.to_string()))
        .id(id)
        .size(px(size))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .cursor_pointer()
        .child(icon(kind, (size * 0.5).round().max(14.), color));
    if pressed {
        base.bg(c(theme::TOP)).border_1().border_color(c(theme::BORDER_STRONG))
    } else {
        base.hover(|s| s.bg(c(theme::OVERLAY))).active(|s| s.bg(c(theme::FLOATING)))
    }
}

pub fn kbd(text: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .px(px(5.))
        .h(px(18.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .border_1()
        .border_color(c(theme::BORDER))
        .bg(c(theme::BASE))
        .font_family(theme::mono())
        .text_size(px(11.))
        .text_color(c(theme::MUTED))
        .child(text.into())
}

/// 28×16 switch.
pub fn switch(id: impl Into<ElementId>, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(28.))
        .h(px(16.))
        .flex_none()
        .relative()
        .rounded(px(8.))
        .cursor_pointer()
        .bg(c(if on { theme::TEXT } else { theme::TOP }))
        .child(
            div()
                .absolute()
                .top(px(2.))
                .left(px(if on { 14. } else { 2. }))
                .size(px(12.))
                .rounded(px(6.))
                .bg(c(if on { theme::BASE } else { theme::PLACEHOLDER })),
        )
}

/// Container for `segment` items (equal widths via `flex_1`).
pub fn segment_track() -> Div {
    div()
        .flex()
        .p(px(2.))
        .gap(px(2.))
        .rounded(px(7.))
        .bg(c(theme::RAISED))
        .border_1()
        .border_color(c(theme::BORDER_SUBTLE))
}

pub fn segment(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool) -> Stateful<Div> {
    let id = id.into();
    let base = div()
        .child(crate::trace::target(id.to_string()))
        .id(id)
        .flex_1()
        .h(px(26.))
        .px(px(6.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .child(label.into());
    if selected {
        base.bg(c(theme::TOP)).text_color(c(theme::TEXT))
    } else {
        base.text_color(c(theme::MUTED)).hover(|s| s.text_color(c(theme::TEXT)).bg(c(theme::OVERLAY)))
    }
}

/// Floating panel surface used by every main area.
pub fn panel() -> Div {
    div()
        .bg(c(theme::BASE))
        .border_1()
        .border_color(c(theme::BORDER_SUBTLE))
        .rounded(px(10.))
        .overflow_hidden()
}

pub fn hairline() -> Div {
    div().h(px(1.)).flex_none().bg(c(theme::BORDER_SUBTLE))
}

/// Small section caption ("Games", "Audio").
pub fn caption(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(12.))
        .line_height(px(16.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(c(theme::TERTIARY))
        .child(text.into())
}

/// Panel heading row: title left, meta right.
pub fn heading(title: impl Into<SharedString>, meta: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_size(px(13.))
                .line_height(px(18.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c(theme::TEXT))
                .child(title.into()),
        )
        .child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(meta.into()))
}

/// Status dot.
pub fn dot(color: u32, size: f32) -> Div {
    div().size(px(size)).flex_none().rounded(px(size / 2.)).bg(c(color))
}
