//! Editor page: native preview stage + transport, inspector (audio, export), timeline.
use gpui::{
    AnyElement, Context, FontWeight, MouseButton, MouseDownEvent, MouseMoveEvent, Window, canvas, div, prelude::*,
    px,
};

use crate::{
    editor_state::{Drag, FPS_PRESETS, GAIN_MAX, GAIN_MIN, timecode},
    root::Root,
    services::library,
    theme::{self, c},
    ui::{self, Icon, Variant, icon},
    views::library::empty_state,
};

const INSPECTOR_W: f32 = 300.0;
const TRANSPORT_H: f32 = 56.0;

impl Root {
    pub fn render_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("editor")
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(8.))
            .pb(px(8.))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if this.ed.drag.is_some() {
                    if event.pressed_button != Some(MouseButton::Left) {
                        this.ed.end_drag();
                    } else {
                        this.ed.drag_to(f32::from(event.position.x));
                    }
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.ed.end_drag();
                cx.notify();
            }))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .gap(px(8.))
                    .child(self.render_preview(cx))
                    .child(self.render_inspector(window, cx)),
            )
            .child(self.render_timeline(cx))
            .into_any_element()
    }

    fn render_preview(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let rect = self.ed.preview_rect.clone();
        let aspect = self.ed.info.as_ref().filter(|i| i.width > 0 && i.height > 0).map_or(16.0 / 9.0, |i| i.width as f32 / i.height as f32);
        let ready = self.ed.preview.as_ref().is_some_and(|p| p.is_ready());
        let message = if self.ed.info.is_none() {
            Some(if self.ed.status.is_empty() { "Choose a clip in the Library".to_owned() } else { self.ed.status.clone() })
        } else if !ready {
            Some(if self.ed.status.is_empty() { "Loading preview…".to_owned() } else { self.ed.status.clone() })
        } else {
            None
        };
        // No fill here: the native video window sits under GPUI's surface and must show through.
        let stage = div()
            .flex_1()
            .min_h_0()
            .relative()
            .child(
                canvas(
                    move |bounds, window, _| {
                        let scale = window.scale_factor();
                        let (bw, bh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                        let (w, h) = if bw / bh.max(1.0) > aspect { (bh * aspect, bh) } else { (bw, bw / aspect) };
                        let x = f32::from(bounds.origin.x) + (bw - w) / 2.0;
                        let y = f32::from(bounds.origin.y) + (bh - h) / 2.0;
                        rect.set(Some(((x * scale).round() as i32, (y * scale).round() as i32, (w * scale).round() as i32, (h * scale).round() as i32)));
                    },
                    |_, _, _, _| {},
                )
                .size_full(),
            )
            .children(message.map(|m| div().absolute().inset_0().child(empty_state(Icon::Film, &m, None))));
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .rounded(px(10.))
            .border_1()
            .border_color(c(theme::BORDER_SUBTLE))
            .child(stage)
            .child(self.render_transport(cx))
            .into_any_element()
    }

    fn render_transport(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let pos = self.ed.position();
        let edited = self.ed.edited_time(pos);
        let total = self.ed.plan.duration();
        let playing = self.ed.playing;
        div()
            .h(px(TRANSPORT_H))
            .flex_none()
            .bg(c(theme::BASE))
            .rounded_b(px(10.))
            .border_t_1()
            .border_color(c(theme::BORDER_SUBTLE))
            .flex()
            .items_center()
            .px(px(16.))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_baseline()
                    .gap(px(6.))
                    .child(div().font_family(theme::MONO).text_size(px(15.)).line_height(px(20.)).child(timecode(edited)))
                    .child(div().font_family(theme::MONO).text_size(px(12.)).text_color(c(theme::TERTIARY)).child(format!("/ {}", timecode(total)))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(ui::icon_button("back5", Icon::Back5, 32., theme::MUTED, false).on_click(cx.listener(|this, _, _, cx| {
                        this.ed.nudge(-5.0);
                        cx.notify();
                    })))
                    .child(
                        div()
                            .id("play")
                            .size(px(38.))
                            .rounded(px(19.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(c(theme::TEXT))
                            .cursor_pointer()
                            .hover(|s| s.bg(c(0xffffff)))
                            .child(icon(if playing { Icon::Pause } else { Icon::Play }, 16., theme::BASE))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.ed.toggle_play();
                                cx.notify();
                            })),
                    )
                    .child(ui::icon_button("fwd5", Icon::Forward5, 32., theme::MUTED, false).on_click(cx.listener(|this, _, _, cx| {
                        this.ed.nudge(5.0);
                        cx.notify();
                    }))),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .justify_end()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(12.))
                    .text_color(c(theme::TERTIARY))
                    .child("Source")
                    .child(div().font_family(theme::MONO).text_color(c(theme::MUTED)).child(timecode(pos))),
            )
            .into_any_element()
    }

    fn render_inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        ui::panel()
            .w(px(INSPECTOR_W))
            .flex_none()
            .flex()
            .flex_col()
            .child(self.render_audio_tracks(cx))
            .child(ui::hairline())
            .child(self.render_export_options(window, cx))
            .child(div().flex_1())
            .child(self.render_export_footer(cx))
            .into_any_element()
    }

    fn render_audio_tracks(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let tracks = self.ed.info.as_ref().map(|i| i.audio.clone()).unwrap_or_default();
        let count = tracks.len();
        let mut column = div().p(px(16.)).flex().flex_col().gap(px(14.)).child(ui::heading(
            "Audio",
            match count {
                0 => "No audio".to_owned(),
                1 => "1 track".to_owned(),
                n => format!("{n} tracks"),
            },
        ));
        for (index, track) in tracks.iter().enumerate() {
            let setting = self.ed.setting(track.stream_index).cloned();
            let gain = setting.as_ref().map_or(0.0, |s| s.gain_db);
            let muted = setting.as_ref().is_some_and(|s| s.muted);
            let name = track_name(&track.name, index);
            let detail = format!("{} · {} kHz", channels(track.channels), track.sample_rate as f32 / 1000.0);
            let span = self.ed.gain_spans.get(index).cloned();
            let fraction = ((gain - GAIN_MIN) / (GAIN_MAX - GAIN_MIN)) as f32;
            if index > 0 {
                column = column.child(ui::hairline());
            }
            column = column.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(
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
                                    .child(div().font_weight(FontWeight::MEDIUM).text_color(c(if muted { theme::MUTED } else { theme::TEXT })).child(name))
                                    .child(div().text_size(px(12.)).line_height(px(16.)).text_color(c(theme::TERTIARY)).child(if muted { format!("Muted · {detail}") } else { detail })),
                            )
                            .child(
                                ui::icon_button(("mute", index), if muted { Icon::VolumeX } else { Icon::Volume }, 30., theme::TEXT, muted)
                                    .when(!muted, |b| b.border_1().border_color(c(theme::BORDER)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.ed.toggle_mute(index);
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .when(muted, |d| d.opacity(0.4))
                            .child(
                                div()
                                    .id(("gain", index))
                                    .flex_1()
                                    .h(px(16.))
                                    .relative()
                                    .cursor_pointer()
                                    .children(span.map(|span| {
                                        canvas(move |b, _, _| span.set(Some((f32::from(b.origin.x), f32::from(b.size.width)))), |_, _, _, _| {})
                                            .absolute()
                                            .inset_0()
                                    }))
                                    .child(div().absolute().left_0().right_0().top(px(6.)).h(px(4.)).rounded(px(2.)).bg(c(theme::TOP)))
                                    .child(div().absolute().left_0().top(px(6.)).h(px(4.)).rounded(px(2.)).w(gpui::relative(fraction)).bg(c(theme::MUTED)))
                                    .child(
                                        div()
                                            .absolute()
                                            .top(px(2.))
                                            .left(gpui::relative(fraction))
                                            .ml(px(-6.))
                                            .size(px(12.))
                                            .rounded(px(6.))
                                            .bg(c(theme::TEXT)),
                                    )
                                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                        if event.click_count >= 2 {
                                            this.ed.reset_gain(index);
                                        } else {
                                            this.ed.begin_drag(Drag::Gain(index));
                                            this.ed.drag_to(f32::from(event.position.x));
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div()
                                    .w(px(58.))
                                    .text_right()
                                    .font_family(theme::MONO)
                                    .text_size(px(12.))
                                    .child(format_db(gain)),
                            ),
                    ),
            );
        }
        column.into_any_element()
    }

    fn render_export_options(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let fps = self.ed.fps;
        let custom = self.fps_custom;
        let info = self.ed.info.clone();
        let kept = self.ed.plan.duration();
        let audio_note = info.as_ref().map_or_else(String::new, |i| {
            let names: Vec<String> = i
                .audio
                .iter()
                .enumerate()
                .map(|(n, t)| {
                    let muted = self.ed.setting(t.stream_index).is_some_and(|s| s.muted);
                    let name = track_name(&t.name, n);
                    if muted { format!("{} muted", name.to_lowercase()) } else { name }
                })
                .collect();
            if names.is_empty() { "None".into() } else { names.join(" · ") }
        });
        let mut track = ui::segment_track();
        for preset in FPS_PRESETS {
            track = track.child(ui::segment(("fps", preset as usize), preset.to_string(), !custom && fps == preset).on_click(cx.listener(move |this, _, _, cx| {
                this.fps_custom = false;
                this.ed.set_fps(preset);
                cx.notify();
            })));
        }
        track = track.child(ui::segment("fps-custom", "Custom", custom).on_click(cx.listener(|this, _, window, cx| {
            this.fps_custom = true;
            this.fps_field.set(this.ed.fps.to_string());
            window.focus(&this.fps_field.focus);
            cx.notify();
        })));
        div()
            .p(px(16.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(ui::heading("Export", "H.264 · NVENC"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(div().text_size(px(12.)).text_color(c(theme::MUTED)).child("Frame rate"))
                    .child(track)
                    .when(custom, |d| {
                        d.child(
                            div()
                                .h(px(30.))
                                .px(px(10.))
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .rounded(px(6.))
                                .bg(c(theme::RAISED))
                                .border_1()
                                .border_color(c(if self.fps_field.is_focused(window) { theme::BORDER_STRONG } else { theme::BORDER }))
                                .child(self.fps_field.render("fps-input", window).flex_1().on_click(cx.listener(|this, _, window, cx| {
                                    window.focus(&this.fps_field.focus);
                                    cx.notify();
                                })))
                                .child(div().text_color(c(theme::TERTIARY)).child("fps")),
                        )
                    })
                    .child(
                        div()
                            .text_size(px(11.))
                            .line_height(px(15.))
                            .text_color(c(theme::TERTIARY))
                            .child(format!("Constant {fps} fps. Gaps hold the last captured frame.")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .text_size(px(12.))
                    .child(detail_row("Length", format!("{} of {}", timecode(kept), timecode(self.ed.duration())), true))
                    .child(detail_row("Size", info.as_ref().map_or_else(String::new, |i| format!("{} × {}", i.width, i.height)), false))
                    .child(detail_row("Audio", audio_note, false))
                    .child(detail_row("Saves to", "exports folder beside the clip".into(), false)),
            )
            .into_any_element()
    }

    fn render_export_footer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let (status_icon, status) = if let Some(error) = &self.ed.save_error {
            (Icon::Alert, format!("Edits not saved: {error}"))
        } else if self.ed.saved {
            (Icon::Check, "Edits saved · original untouched".to_owned())
        } else {
            (Icon::Refresh, "Saving edits…".to_owned())
        };
        let exported = self.ed.exported.clone();
        div()
            .p(px(16.))
            .pt(px(12.))
            .border_t_1()
            .border_color(c(theme::BORDER_SUBTLE))
            .flex()
            .flex_col()
            .gap(px(10.))
            .when(self.ed.info.is_some(), |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_size(px(12.))
                        .text_color(c(theme::TERTIARY))
                        .child(icon(status_icon, 13., theme::TERTIARY))
                        .child(div().truncate().child(status)),
                )
            })
            .children(self.ed.export_error.clone().map(|e| div().text_size(px(12.)).text_color(c(theme::ERROR)).line_clamp(3).child(e)))
            .children(exported.map(|summary| {
                let path = summary.path.clone();
                div()
                    .id("exported")
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .p(px(8.))
                    .rounded(px(6.))
                    .bg(c(theme::RAISED))
                    .cursor_pointer()
                    .hover(|s| s.bg(c(theme::OVERLAY)))
                    .child(ui::dot(theme::ACTIVE, 7.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().font_weight(FontWeight::MEDIUM).child(format!("Exported · {} fps", summary.fps)))
                            .child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(format!("{} frames · {}", summary.frames, library::format_duration(summary.seconds)))),
                    )
                    .child(icon(Icon::Folder, 15., theme::MUTED))
                    .on_click(cx.listener(move |_, _, _, _| crate::backend::show_in_folder(&path)))
            }))
            .child(
                ui::button("export", Variant::Primary)
                    .h(px(36.))
                    .w_full()
                    .when(self.ed.info.is_none() || self.ed.exporting, |b| b.opacity(0.5))
                    .child(icon(if self.ed.exporting { Icon::Refresh } else { Icon::Upload }, 15., theme::BASE))
                    .child(if self.ed.exporting { format!("Exporting at {} fps…", self.ed.fps) } else { "Export clip".to_owned() })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.ed.export(&this.workers);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
}

fn detail_row(label: &str, value: String, mono: bool) -> AnyElement {
    div()
        .flex()
        .justify_between()
        .gap(px(16.))
        .child(div().text_color(c(theme::TERTIARY)).child(label.to_owned()))
        .child(div().min_w_0().truncate().when(mono, |d| d.font_family(theme::MONO)).child(value))
        .into_any_element()
}

/// Recall recordings carry Game then Mic; other sources keep their tag or a number.
pub fn track_name(tag: &str, index: usize) -> String {
    let lower = tag.to_lowercase();
    if lower.contains("game") {
        "Game".into()
    } else if lower.contains("mic") {
        "Microphone".into()
    } else if lower.contains("voice") || lower.contains("discord") {
        "Voice chat".into()
    } else if !tag.trim().is_empty() && lower != "und" {
        tag.to_owned()
    } else {
        format!("Audio {}", index + 1)
    }
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
