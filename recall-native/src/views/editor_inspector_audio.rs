//! The inspector's "Export audio" section: this clip's "Merge audio" switch on top, then each
//! track: name and format, the export mute and a Merge | Own toggle on the right, and the
//! export gain slider over an export level meter (the Settings > Audio strips' meter, lying
//! down). A track set to Own is greyed except its toggle; with Merge audio off every toggle is
//! dimmed and still and no row is greyed (each track is its own). Everything here is the
//! export's: saved with the edit. The listen-only lane levels left of the timeline
//! (`timeline_listen.rs`) never change it or the meter.
use gpui::{AnyElement, Context, FontWeight, MouseButton, MouseDownEvent, div, prelude::*, px};

use super::track_name;
use crate::{
    editor_state::{Drag, GAIN_MAX, GAIN_MIN},
    root::Root,
    theme::{self, c},
    ui::{self, Icon},
    views::audio_strip::meter_horizontal,
};

/// Dimming for a greyed row (a track kept as its own) and for still controls.
const GREYED: f32 = 0.4;

impl Root {
    pub(super) fn render_audio_tracks(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let tracks = self.ed.info.as_ref().map(|i| i.audio.clone()).unwrap_or_default();
        let count = tracks.len();
        let merge = self.ed.mix_down();
        let mut column = div().p(px(16.)).flex().flex_col().gap(px(14.)).child(ui::heading(
            "Export audio",
            match count {
                0 => "No audio".to_owned(),
                1 => "1 track".to_owned(),
                n => format!("{n} tracks"),
            },
        ));
        if count > 1 {
            column = column.child(merge_row(merge, cx));
        }
        for (index, track) in tracks.iter().enumerate() {
            let setting = self.ed.setting(track.stream_index).cloned();
            let gain = setting.as_ref().map_or(0.0, |s| s.gain_db);
            let muted = setting.as_ref().is_some_and(|s| s.muted);
            let in_mix = self.ed.in_mix(index);
            // Greyed: merging, and this track kept out of the merged track.
            let greyed = count > 1 && merge && !in_mix;
            let name = track_name(&track.name, index);
            let detail = format!("{} · {} kHz", channels(track.channels), track.sample_rate as f32 / 1000.0);
            let meter = self.ed.export_meter(index);
            if index > 0 {
                column = column.child(ui::hairline());
            }
            let info = div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .when(greyed, |d| d.opacity(GREYED))
                .child(div().font_weight(FontWeight::MEDIUM).text_color(c(if muted { theme::MUTED } else { theme::TEXT })).child(name))
                .child(div().text_size(px(12.)).line_height(px(16.)).text_color(c(theme::TERTIARY)).child(if muted { format!("Muted · {detail}") } else { detail }));
            let mute = ui::icon_button(("mute", index), if muted { Icon::VolumeX } else { Icon::Volume }, 30., theme::TEXT, muted)
                .when(!muted, |b| b.border_1().border_color(c(theme::BORDER)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.ed.toggle_mute(index);
                    cx.notify();
                }));
            let header = div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(info)
                .child(div().when(greyed, |d| d.opacity(GREYED)).child(mute))
                .when(count > 1, |d| d.child(merge_toggle(index, in_mix, merge, cx)));
            column = column.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(header)
                    .child(self.gain_row(index, gain, meter, muted || greyed, cx)),
            );
        }
        column.into_any_element()
    }

    /// The export gain slider over its level meter, then the value.
    fn gain_row(&mut self, index: usize, gain: f64, meter: Option<(f32, f32)>, dim: bool, cx: &mut Context<Self>) -> AnyElement {
        let span = self.ed.gain_spans.get(index).cloned();
        let fraction = ((gain - GAIN_MIN) / (GAIN_MAX - GAIN_MIN)) as f32;
        let (rms, hold) = meter.unwrap_or((-90.0, -90.0));
        let slider = div()
            .id(("gain", index))
            .h(px(16.))
            .relative()
            .cursor_pointer()
            .children(span.map(|span| {
                gpui::canvas(move |b, _, _| span.set(Some((f32::from(b.origin.x), f32::from(b.size.width)))), |_, _, _, _| {})
                    .absolute()
                    .inset_0()
            }))
            .child(div().absolute().left_0().right_0().top(px(6.)).h(px(4.)).rounded(px(2.)).bg(c(theme::TOP)))
            .child(div().absolute().left_0().top(px(6.)).h(px(4.)).rounded(px(2.)).w(gpui::relative(fraction)).bg(c(theme::MUTED)))
            .child(div().absolute().top(px(2.)).left(gpui::relative(fraction)).ml(px(-6.)).size(px(12.)).rounded(px(6.)).bg(c(theme::TEXT)))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                if event.click_count >= 2 {
                    this.ed.reset_gain(index);
                } else {
                    this.ed.begin_drag(Drag::Gain(index));
                    this.ed.drag_to(f32::from(event.position.x));
                }
                cx.notify();
            }));
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .when(dim, |d| d.opacity(GREYED))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(slider)
                    .child(div().relative().child(crate::trace::target(format!("export-meter-{index}"))).child(meter_horizontal(rms, hold, meter.is_some(), 6.))),
            )
            .child(div().w(px(58.)).text_right().font_family(theme::mono()).text_size(px(12.)).child(format_db(gain)))
            .into_any_element()
    }
}

/// This clip's "Merge audio": one undo step; Settings > Audio sets the default.
fn merge_row(merge: bool, cx: &mut Context<Root>) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(c(theme::TEXT)).child("Merge audio"))
                .child(div().text_size(px(12.)).line_height(px(16.)).text_color(c(theme::TERTIARY)).child(if merge { "Merge tracks into one" } else { "Every track is its own" })),
        )
        .child(ui::switch("clip-merge", merge).child(crate::trace::target("clip-merge")).on_click(cx.listener(move |this, _, _, cx| {
            this.ed.set_clip_merge(!merge);
            cx.notify();
        })))
        .into_any_element()
}

/// A track's Merge | Own toggle: in the merged track, or kept as its own. Dimmed and still
/// while the clip's Merge audio is off. One undo step per change.
fn merge_toggle(index: usize, in_mix: bool, merge: bool, cx: &mut Context<Root>) -> AnyElement {
    let part = |own: bool, cx: &mut Context<Root>| {
        let (key, label) = if own { ("own", "Own") } else { ("merge", "Merge") };
        let selected = in_mix != own;
        ui::segment((if own { "track-own" } else { "track-merge" }, index), label, selected)
            .flex_none()
            .relative()
            .px(px(8.))
            .child(crate::trace::target(format!("in-mix-{index}-{key}")))
            .when(merge && !selected, |s| {
                s.on_click(cx.listener(move |this, _, _, cx| {
                    this.ed.toggle_mix(index);
                    cx.notify();
                }))
            })
    };
    div()
        .relative()
        .child(crate::trace::target(format!("in-mix-{index}")))
        .when(!merge, |d| d.opacity(GREYED))
        .child(ui::segment_track().child(part(false, cx)).child(part(true, cx)))
        .into_any_element()
}

fn channels(n: u16) -> &'static str {
    match n {
        1 => "mono",
        2 => "stereo",
        _ => "multichannel",
    }
}

fn format_db(db: f64) -> String {
    if db.abs() < 0.05 { "0.0 dB".into() } else if db < 0.0 { format!("−{:.1} dB", -db) } else { format!("+{db:.1} dB") }
}
