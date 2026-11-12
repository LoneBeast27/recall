//! Minimal single-line text field: append/backspace editing, Ctrl+A replace, paste. A new value
//! starts all selected; arrow keys, Home, End or a click keep it and go on typing at the end.
//! The owning view routes `KeyDownEvent`s here and decides what Submit/Cancel mean.
use gpui::{
    App, Div, ElementId, FocusHandle, KeyDownEvent, SharedString, Stateful, Window, div, prelude::*, px,
};

use crate::theme::{self, c};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldEvent {
    None,
    Changed,
    Submit,
    Cancel,
}

pub struct TextField {
    pub value: String,
    pub focus: FocusHandle,
    pub placeholder: SharedString,
    select_all: bool,
    max_len: usize,
    digits_only: bool,
    decimal: bool,
}

impl TextField {
    pub fn new(cx: &mut App, placeholder: impl Into<SharedString>, max_len: usize) -> Self {
        Self {
            value: String::new(),
            focus: cx.focus_handle(),
            placeholder: placeholder.into(),
            select_all: false,
            max_len,
            digits_only: false,
            decimal: false,
        }
    }

    pub fn digits_only(mut self) -> Self {
        self.digits_only = true;
        self
    }

    /// Digits and one decimal point; a comma counts as the point.
    pub fn decimal(mut self) -> Self {
        self.decimal = true;
        self
    }

    pub fn set(&mut self, value: impl Into<String>) {
        self.value = value.into();
        self.select_all = true;
    }

    /// Keeps the text and types after it.
    pub fn deselect(&mut self) {
        self.select_all = false;
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    pub fn insert(&mut self, text: &str) -> FieldEvent {
        let mut clean: String = text.chars().filter(|ch| !ch.is_control() && (!self.digits_only || ch.is_ascii_digit())).collect();
        if self.decimal {
            let mut point = !self.select_all && self.value.contains('.');
            clean = clean
                .chars()
                .map(|ch| if ch == ',' { '.' } else { ch })
                .filter(|ch| ch.is_ascii_digit() || (*ch == '.' && !std::mem::replace(&mut point, true)))
                .collect();
        }
        if clean.is_empty() {
            return FieldEvent::None;
        }
        if self.select_all {
            self.value.clear();
            self.select_all = false;
        }
        for ch in clean.chars() {
            if self.value.chars().count() >= self.max_len {
                break;
            }
            self.value.push(ch);
        }
        FieldEvent::Changed
    }

    /// Handles one key. `clipboard` supplies text for Ctrl+V.
    pub fn key_down(&mut self, event: &KeyDownEvent, clipboard: Option<String>) -> FieldEvent {
        let keystroke = &event.keystroke;
        let ctrl = keystroke.modifiers.control;
        match keystroke.key.as_str() {
            "enter" | "tab" => FieldEvent::Submit,
            "escape" => FieldEvent::Cancel,
            "backspace" => {
                if self.select_all || ctrl {
                    self.value.clear();
                    self.select_all = false;
                } else {
                    self.value.pop();
                }
                FieldEvent::Changed
            }
            "delete" if self.select_all => {
                self.value.clear();
                self.select_all = false;
                FieldEvent::Changed
            }
            "left" | "right" | "home" | "end" => {
                self.select_all = false;
                FieldEvent::None
            }
            "a" if ctrl => {
                self.select_all = true;
                FieldEvent::None
            }
            "v" if ctrl => clipboard.map_or(FieldEvent::None, |text| self.insert(&text)),
            _ if ctrl || keystroke.modifiers.alt => FieldEvent::None,
            _ => typed_text(keystroke).map_or(FieldEvent::None, |text| self.insert(&text)),
        }
    }

    /// The input box. The caller adds `.on_key_down` and `.on_click` (focus) listeners.
    pub fn render(&self, id: impl Into<ElementId>, window: &Window) -> Stateful<Div> {
        let focused = self.is_focused(window);
        let empty = self.value.is_empty();
        let text = div()
            .flex()
            .items_center()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_size(px(13.))
            .text_color(c(if empty { theme::PLACEHOLDER } else { theme::TEXT }))
            .when(self.select_all && focused && !empty, |d| d.bg(c(theme::TOP)).rounded(px(3.)))
            .child(if empty { self.placeholder.to_string() } else { self.value.clone() });
        div()
            .id(id)
            .track_focus(&self.focus)
            .flex()
            .items_center()
            .min_w_0()
            .cursor_text()
            .child(text)
            .when(focused && (!empty || !self.select_all), |d| {
                d.child(div().w(px(1.)).h(px(15.)).ml(px(1.)).flex_none().bg(c(theme::TEXT)))
            })
    }
}

/// The text a key types. gpui 0.2.2 on Windows reports the space bar as key "space" with no
/// `key_char`, so it is mapped here; every other key types its `key_char`. Ctrl and Alt
/// shortcuts never reach this (they are handled or ignored before).
fn typed_text(keystroke: &gpui::Keystroke) -> Option<String> {
    match (keystroke.key.as_str(), &keystroke.key_char) {
        (_, Some(text)) => Some(text.clone()),
        ("space", None) => Some(" ".into()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::typed_text;

    fn key(key: &str, key_char: Option<&str>) -> gpui::Keystroke {
        gpui::Keystroke { modifiers: Default::default(), key: key.into(), key_char: key_char.map(str::to_owned) }
    }

    #[test]
    fn the_space_bar_types_a_space() {
        assert_eq!(typed_text(&key("space", None)).as_deref(), Some(" "));
        assert_eq!(typed_text(&key("space", Some(" "))).as_deref(), Some(" "));
        assert_eq!(typed_text(&key("a", Some("a"))).as_deref(), Some("a"));
        assert_eq!(typed_text(&key("f5", None)), None);
    }
}
