//! Audio page: the MIDI console card (the input chosen in Settings → General → MIDI device).
use gpui::{AnyElement, Context, div, prelude::*, px};

use crate::{
    root::Root,
    theme::{self, c},
    ui::{self, Icon, Variant, icon},
    views::pages::card,
};

impl Root {
    pub(crate) fn midi_card(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let midi = self.midi.as_ref();
        let port = midi.and_then(|m| m.monitored_port());
        let outputs = midi.map(|m| m.ports().outputs).unwrap_or_default();
        let recent = midi.map(|m| m.recent()).unwrap_or_default();
        card()
                .gap(px(14.))
                .child(ui::heading("MIDI console", match (midi, port.clone()) {
                    (None, _) => "Off · choose a device in Settings → General".to_owned(),
                    (Some(m), None) => format!("{} is not connected", m.wanted()),
                    (Some(_), Some(p)) => format!("Listening on {p}"),
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(ui::button("midi-test", Variant::Secondary).child(icon(Icon::Play, 13., theme::TEXT)).child("Send test note").on_click(cx.listener(|this, _, _, cx| {
                            // The output with the chosen input's name (devices list both sides).
                            let Some(target) = this.midi.as_ref().map(|m| m.wanted().to_owned()) else {
                                this.midi_result = Some(Err("MIDI is off".to_owned()));
                                cx.notify();
                                return;
                            };
                            // Opening the port and the 120 ms note run off the UI thread.
                            cx.spawn(async move |this, cx| {
                                let result = cx.background_executor().spawn(async move { crate::services::midi::send_test_note(&target).map(|()| target) }).await;
                                let _ = this.update(cx, |this, cx| {
                                    this.midi_result = Some(result);
                                    cx.notify();
                                });
                            })
                            .detach();
                        })))
                        .child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(match &self.midi_result {
                            Some(Ok(target)) => format!("Sent to {target}"),
                            Some(Err(error)) => error.clone(),
                            None => format!("{} output ports", outputs.len()),
                        })),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .font_family(theme::mono())
                        .text_size(px(12.))
                        .child(div().font_family(theme::sans()).text_color(c(theme::TERTIARY)).pb(px(4.)).child("Incoming"))
                        .when(recent.is_empty(), |d| d.child(div().text_color(c(theme::DISABLED)).child("No messages yet. Press a key or pad on the device.")))
                        .children(recent.into_iter().map(|m| {
                            let bytes: Vec<String> = m.bytes.iter().map(|b| format!("{b:02X}")).collect();
                            div()
                                .flex()
                                .gap(px(16.))
                                .child(div().w(px(80.)).text_color(c(theme::TERTIARY)).child(format!("{:.1} s", m.at_ms as f64 / 1000.0)))
                                .child(div().child(bytes.join(" ")))
                        })),
                )
                .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    /// 2026-11-07 audit A10: "Send test note" opened the MIDI output and slept 120 ms on the UI
    /// thread; the click handler now hands the send to a background task.
    #[test]
    fn the_test_note_never_sleeps_on_the_ui_thread() {
        let source = include_str!("audio_midi.rs");
        let handler = source.split("#[cfg(test)]").next().unwrap();
        assert!(!handler.contains(&["thread::", "sleep"].concat()), "the click handler sleeps on the UI thread");
        assert!(handler.contains("background_executor()"), "the send runs on a background task");
    }
}
