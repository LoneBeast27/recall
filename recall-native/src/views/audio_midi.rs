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
                            // One send at a time: two at once open the same port twice, and the
                            // second one's error would replace the first one's "Sent to ...".
                            if !should_send(this.midi_sending) {
                                return;
                            }
                            let Some(target) = this.midi.as_ref().map(|m| m.wanted().to_owned()) else {
                                this.midi_result = Some(Err("MIDI is off".to_owned()));
                                cx.notify();
                                return;
                            };
                            this.midi_sending = true;
                            // Opening the port and the 120 ms note run off the UI thread.
                            cx.spawn(async move |this, cx| {
                                let result = cx.background_executor().spawn(async move { send_caught(move || crate::services::midi::send_test_note(&target).map(|()| target)) }).await;
                                let _ = this.update(cx, |this, cx| {
                                    this.midi_sending = false;
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

/// Runs a test-note send, a panic becoming an error: the click's `update` then always runs and
/// clears `midi_sending`, so a failed send never leaves the button dead until a restart.
pub(crate) fn send_caught(send: impl FnOnce() -> Result<String, String>) -> Result<String, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(send)).unwrap_or_else(|_| Err("The test note failed (see the log)".to_owned()))
}

/// A "Send test note" click starts a send only when none is in flight.
pub(crate) fn should_send(in_flight: bool) -> bool {
    !in_flight
}

#[cfg(test)]
mod tests {
    /// 2026-11-07 audit R2-deep-audio-4: a second click while a test note is in flight is
    /// ignored, and the click handler checks it before it spawns the send.
    #[test]
    fn a_second_click_waits_for_the_test_note_in_flight() {
        assert!(super::should_send(false));
        assert!(!super::should_send(true));
        let source = include_str!("audio_midi.rs");
        let guard = source.find(&["if !should_send(", "this.midi_sending)"].concat()).expect("the click checks the guard");
        let spawn = source.find(&["cx.spawn(async move |this, cx|"].concat()).expect("the send task");
        assert!(guard < spawn, "the guard runs before the send is spawned");
        assert!(source.contains(&["this.midi_sending = false;"].concat()), "the guard clears when the send ends");
    }

    /// 2026-11-07 audit R3-deep-audio-4: a send that panics still frees "Send test note": the
    /// panic becomes an error the click shows, and the guard clears as for any result.
    #[test]
    fn a_panicking_test_note_frees_the_button() {
        let source = include_str!("audio_midi.rs");
        let handler = source.split("#[cfg(test)]").next().unwrap();
        assert!(handler.contains("send_caught(move ||"), "a panicking send leaves midi_sending stuck");
        assert_eq!(super::send_caught(|| panic!("port vanished")), Err("The test note failed (see the log)".to_owned()));
        assert_eq!(super::send_caught(|| Ok("Box".to_owned())), Ok("Box".to_owned()));
    }

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
