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
                            let result = this.midi.as_ref().map_or(Err("MIDI is off".to_owned()), |m| {
                                let target = m.wanted().to_owned();
                                m.send(&target, &[0x90, 60, 127]).map(|_| target)
                            });
                            if let Some(midi) = this.midi.as_ref() {
                                if let Ok(target) = &result {
                                    std::thread::sleep(std::time::Duration::from_millis(120));
                                    let _ = midi.send(target, &[0x80, 60, 0]);
                                }
                            }
                            this.midi_result = Some(result);
                            cx.notify();
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
