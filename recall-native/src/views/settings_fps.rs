//! Settings → Clip → Frame rate: quick presets plus a Custom box for any whole rate from
//! `FPS_MIN` to `FPS_MAX`. The box saves on Enter or when it loses focus (each save restarts
//! the recorder, so typing alone never saves); Escape puts the saved rate back.
use gpui::{AnyElement, Context, KeyDownEvent, SharedString, Window, div, prelude::*, px};
use serde_json::{Value, json};

use crate::{
    root::{Root, Route},
    theme::{self, c},
    ui::{self, FieldEvent, TextField},
    views::settings::SettingsTab,
};

/// One click away. Anything else is typed.
pub const PRESETS: [u64; 5] = [30, 60, 120, 144, 240];
pub const DEFAULT_FPS: u64 = 60;
/// The recorder's accepted range (`recall-hdr` `record_config::FPS_MIN..=FPS_MAX`).
pub const FPS_MIN: u64 = 15;
pub const FPS_MAX: u64 = 240;

/// The Custom box's state.
#[derive(Default)]
pub struct FpsInput {
    pub field: Option<TextField>,
    /// Custom is picked, so the box shows even while the rate equals a preset.
    pub open: bool,
    /// The box had focus at the last frame; losing it saves.
    editing: bool,
    /// Why a typed rate was changed on save.
    pub note: Option<String>,
}

/// The rate a typed value saves as: clamped to `FPS_MIN..=FPS_MAX`. None for an empty box.
pub fn typed_fps(text: &str) -> Option<u64> {
    let digits = text.trim();
    if digits.is_empty() {
        return None;
    }
    Some(digits.parse::<u64>().unwrap_or(u64::MAX).clamp(FPS_MIN, FPS_MAX))
}

/// The line under the row when a typed rate was out of range.
pub fn clamp_note(typed: &str, saved: u64) -> Option<String> {
    let typed = typed.trim();
    (!typed.is_empty() && typed.trim_start_matches('0') != saved.to_string())
        .then(|| format!("Frame rate goes from {FPS_MIN} to {FPS_MAX} fps, so {typed} was saved as {saved}."))
}

impl Root {
    pub(crate) fn recording_fps(&self) -> u64 {
        self.config_value("recording", "fps").and_then(Value::as_u64).unwrap_or(DEFAULT_FPS)
    }

    fn fps_box_shown(&self) -> bool {
        self.route == Route::Settings && self.settings_tab == SettingsTab::Clip
    }

    /// Saves what the Custom box holds, if Custom is picked.
    fn commit_custom_fps(&mut self) {
        let current = self.recording_fps();
        let input = &mut self.settings_ui.fps;
        input.editing = false;
        // The box also shows for a saved rate that is no preset (after leaving and coming back,
        // `open` is false): a value typed there must still save.
        if !input.open && PRESETS.contains(&current) {
            return;
        }
        let Some(field) = input.field.as_mut() else { return };
        let typed = field.value.clone();
        let fps = typed_fps(&typed).unwrap_or(current);
        input.note = clamp_note(&typed, fps).filter(|_| typed_fps(&typed).is_some());
        field.set(fps.to_string());
        if fps != current {
            self.set_config("recording", "fps", json!(fps));
        }
    }

    /// Keys for the Custom box; false when it doesn't have focus.
    pub(crate) fn fps_input_key(&mut self, event: &KeyDownEvent, clipboard: Option<String>, window: &mut Window) -> bool {
        let current = self.recording_fps();
        let Some(field) = self.settings_ui.fps.field.as_mut().filter(|f| f.is_focused(window)) else { return false };
        match field.key_down(event, clipboard) {
            FieldEvent::Submit => {
                self.commit_custom_fps();
                window.focus(&self.focus);
            }
            FieldEvent::Cancel => {
                field.set(current.to_string());
                self.settings_ui.fps.editing = false;
                self.settings_ui.fps.note = None;
                window.focus(&self.focus);
            }
            FieldEvent::Changed | FieldEvent::None => {}
        }
        true
    }

    /// Runs before every frame: a box that lost focus (a click elsewhere, another page) saves,
    /// and a box nobody is typing in shows the saved rate.
    pub(crate) fn settle_fps_input(&mut self, window: &mut Window) {
        let shown = self.fps_box_shown();
        let current = self.recording_fps();
        let Some(field) = self.settings_ui.fps.field.as_ref() else { return };
        let mut focused = field.is_focused(window);
        if focused && !shown {
            window.focus(&self.focus);
            focused = false;
        }
        if self.settings_ui.fps.editing && !focused {
            self.commit_custom_fps();
        }
        let input = &mut self.settings_ui.fps;
        input.editing = focused;
        if !shown {
            input.open = false;
            input.note = None;
        }
        if let Some(field) = input.field.as_mut().filter(|_| !focused)
            && field.value != current.to_string()
        {
            field.set(current.to_string());
        }
    }

    /// The Frame rate row's control: presets, Custom, and the box while Custom is picked.
    pub(crate) fn render_fps_control(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let fps = self.recording_fps();
        let custom = self.settings_ui.fps.open || !PRESETS.contains(&fps);
        let mut track = ui::segment_track().w(px(330.));
        for (i, preset) in PRESETS.into_iter().enumerate() {
            track = track.child(ui::segment((SharedString::from("rec-fps"), i), preset.to_string(), !custom && fps == preset).on_click(cx.listener(
                move |this, _, _, cx| {
                    let input = &mut this.settings_ui.fps;
                    input.open = false;
                    input.editing = false;
                    input.note = None;
                    if this.recording_fps() != preset {
                        this.set_config("recording", "fps", json!(preset));
                    }
                    cx.notify();
                },
            )));
        }
        track = track.child(ui::segment("rec-fps-custom", "Custom", custom).on_click(cx.listener(move |this, _, window, cx| {
            let input = &mut this.settings_ui.fps;
            input.open = true;
            input.note = None;
            let field = input.field.get_or_insert_with(|| TextField::new(cx, "fps", 3).digits_only());
            field.set(fps.to_string());
            window.focus(&field.focus);
            input.editing = true;
            cx.notify();
        })));
        let mut control = div().flex().items_center().gap(px(8.)).child(track);
        if custom {
            let field = self.settings_ui.fps.field.get_or_insert_with(|| TextField::new(cx, "fps", 3).digits_only());
            if !field.is_focused(window) && field.value != fps.to_string() {
                field.set(fps.to_string());
            }
            let focused = field.is_focused(window);
            control = control.child(
                div()
                    .w(px(78.))
                    .h(px(30.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .rounded(px(6.))
                    .bg(c(theme::RAISED))
                    .border_1()
                    .border_color(c(if focused { theme::BORDER_STRONG } else { theme::BORDER }))
                    .child(crate::trace::target("rec-fps-input"))
                    .child(field.render("rec-fps-input", window).flex_1().on_click(cx.listener(|this, _, window, cx| {
                        if let Some(field) = this.settings_ui.fps.field.as_ref() {
                            window.focus(&field.focus);
                        }
                        cx.notify();
                    })))
                    .child(div().text_color(c(theme::TERTIARY)).child("fps")),
            );
        }
        control.into_any_element()
    }

    /// The Frame rate row's grey line.
    pub(crate) fn fps_detail(&self) -> String {
        if let Some(note) = self.settings_ui.fps.note.clone() {
            return note;
        }
        // Recall records each frame the game draws, up to this rate: a higher rate costs the game
        // more and spreads the same bitrate over more frames.
        format!("Frames per second in your clips. Custom takes {FPS_MIN} to {FPS_MAX}. Higher rates cost the game more and spread the same bitrate over more frames.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_rates_are_clamped_to_the_recorder_range() {
        assert_eq!(typed_fps(""), None);
        assert_eq!(typed_fps("  "), None);
        assert_eq!(typed_fps("60"), Some(60));
        assert_eq!(typed_fps("165"), Some(165));
        assert_eq!(typed_fps("240"), Some(240));
        assert_eq!(typed_fps("999"), Some(240));
        assert_eq!(typed_fps("0"), Some(FPS_MIN));
        assert_eq!(typed_fps("5"), Some(FPS_MIN));
        assert_eq!(typed_fps("060"), Some(60));
        assert_eq!(typed_fps("99999999999999999999999"), Some(FPS_MAX));
    }

    #[test]
    fn only_a_changed_rate_gets_a_note() {
        assert_eq!(clamp_note("165", 165), None);
        assert_eq!(clamp_note("060", 60), None);
        assert_eq!(clamp_note("", 60), None);
        assert_eq!(clamp_note("999", 240).as_deref(), Some("Frame rate goes from 15 to 240 fps, so 999 was saved as 240."));
        assert_eq!(clamp_note("5", 15).as_deref(), Some("Frame rate goes from 15 to 240 fps, so 5 was saved as 15."));
    }

    #[test]
    fn presets_and_default_sit_inside_the_range() {
        assert!(PRESETS.iter().all(|p| (FPS_MIN..=FPS_MAX).contains(p)));
        assert!(PRESETS.contains(&DEFAULT_FPS));
    }
}
