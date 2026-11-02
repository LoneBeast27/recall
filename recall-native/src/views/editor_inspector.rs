//! Editor inspector: per-track gain/mute, export options and the export footer.
use gpui::{
    AnyElement, Context, FontWeight, MouseButton, MouseDownEvent, MouseMoveEvent, SharedString, Window, canvas, div,
    prelude::*, px,
};

use super::{INSPECTOR_W, track_name};
use crate::{
    edit_model::ExportMode,
    editor_state::{Drag, FPS_PRESETS, GAIN_MAX, GAIN_MIN, timecode},
    root::Root,
    services::library,
    theme::{self, c},
    ui::{self, Icon, Variant, icon},
};

impl Root {
    pub(super) fn render_inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        ui::panel()
            .w(px(INSPECTOR_W))
            .flex_none()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("inspector-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(self.render_audio_tracks(cx))
                    .child(ui::hairline())
                    .child(self.render_export_options(window, cx)),
            )
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
                                    .font_family(theme::mono())
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
        let lossless = self.ed.lossless();
        let height = self.ed.export_settings.height;
        let cap = self.ed.export_settings.max_bytes;
        let mut method = ui::segment_track();
        for (label, mode) in [("Re-encode", ExportMode::Reencode), ("Lossless", ExportMode::Lossless)] {
            method = method.child(ui::segment(SharedString::from(format!("mode-{label}")), label, self.ed.export_settings.mode == mode).on_click(cx.listener(move |this, _, _, cx| {
                this.ed.set_export_mode(mode);
                cx.notify();
            })));
        }
        let mut sizes = ui::segment_track();
        for (label, value) in [("Original", None), ("1080p", Some(1080u32)), ("720p", Some(720u32))] {
            sizes = sizes.child(ui::segment(SharedString::from(format!("h-{label}")), label, height == value).on_click(cx.listener(move |this, _, _, cx| {
                this.ed.set_height(value);
                cx.notify();
            })));
        }
        let mut caps = ui::segment_track();
        for (label, value) in [("No limit", None), ("10 MB", Some(10_000_000u64)), ("50 MB", Some(50_000_000u64)), ("500 MB", Some(500_000_000u64))] {
            caps = caps.child(ui::segment(SharedString::from(format!("cap-{label}")), label, cap == value).on_click(cx.listener(move |this, _, _, cx| {
                this.ed.set_max_bytes(value);
                cx.notify();
            })));
        }
        let field = |label: &str, control: gpui::AnyElement| {
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(div().text_size(px(12.)).text_color(c(theme::MUTED)).child(label.to_owned()))
                .child(control)
        };
        let note = |text: String| div().text_size(px(11.)).line_height(px(15.)).text_color(c(theme::TERTIARY)).child(text);
        let mut column = div()
            .p(px(16.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(ui::heading("Export", if lossless { "Stream copy" } else { "NVENC" }))
            .child(field("Method", method.into_any_element()));
        if lossless {
            column = column.child(note("Copies the video untouched, in seconds. Each cut starts at the nearest earlier keyframe (shaded on the timeline). Frame rate, size and speed stay as recorded.".into()));
        } else {
            column = column
                .child(
                    field("Frame rate", track.into_any_element())
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
                        .child(note(format!("Constant {fps} fps. Gaps hold the last captured frame."))),
                )
                .child(field("Resolution", sizes.into_any_element()))
                .child(field("Size limit", caps.into_any_element()));
        }
        column
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .text_size(px(12.))
                    .child(detail_row("Length", format!("{} of {}", timecode(kept), timecode(self.ed.duration())), true))
                    .child(detail_row("Size", info.as_ref().map_or_else(String::new, |i| format!("{} × {}", i.width, i.height)), false))
                    .child(detail_row("Audio", audio_note, false))
                    .child(detail_row(
                        "Saves to",
                        info.as_ref()
                            .and_then(|i| crate::services::storage::export_dir(std::path::Path::new(&i.path), &crate::backend::library_dir_from(&self.folders)))
                            .map_or_else(|| "exports folder beside the clip".to_owned(), |dir| dir.to_string_lossy().into_owned()),
                        false,
                    )),
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
                let drag_path = summary.path.clone();
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
                            .child(div().font_weight(FontWeight::MEDIUM).child(match summary.fps { Some(fps) => format!("Exported · {fps} fps"), None => "Exported · lossless".to_owned() }))
                            .child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(format!("{} frames · {}", summary.frames, library::format_duration(summary.seconds))))
                            .child(div().text_size(px(11.)).text_color(c(theme::TERTIARY)).child("Drag into Discord or a folder")),
                    )
                    .child(icon(Icon::Folder, 15., theme::MUTED))
                    // A press that moves becomes a file drag of the export (Discord, Explorer, chats);
                    // a plain click shows it in Explorer.
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, _, _| {
                        this.drag_origin = Some((drag_path.clone(), f32::from(event.position.x), f32::from(event.position.y)));
                    }))
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                        if event.pressed_button != Some(MouseButton::Left) {
                            return;
                        }
                        if let Some((path, x0, y0)) = this.drag_origin.clone() {
                            let (x, y) = (f32::from(event.position.x), f32::from(event.position.y));
                            if (x - x0).abs() + (y - y0).abs() > 8.0 {
                                this.drag_origin = None;
                                this.drag_paths(vec![path], cx);
                            }
                        }
                    }))
                    .on_click(cx.listener(move |_, _, _, _| crate::backend::show_in_folder(&path)))
            }))
            .children(self.ed.pre_encode_line().filter(|_| !self.ed.exporting).map(|line| {
                let ready = self.ed.pre_encode_ready();
                div()
                    .id("pre-encode")
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(12.))
                    .text_color(c(if ready { theme::ACTIVE } else { theme::TERTIARY }))
                    .child(icon(if ready { Icon::Check } else { Icon::Refresh }, 13., if ready { theme::ACTIVE } else { theme::TERTIARY }))
                    .child(div().truncate().child(line))
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
            .when(self.ed.exporting, |d| {
                d.child(
                    ui::button("export-cancel", Variant::Ghost)
                        .h(px(32.))
                        .w_full()
                        .child(if self.ed.cancelling { "Cancelling…" } else { "Cancel export" })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.ed.cancel_export();
                            cx.notify();
                        })),
                )
            })
            .into_any_element()
    }
}

fn detail_row(label: &str, value: String, mono: bool) -> AnyElement {
    div()
        .flex()
        .justify_between()
        .gap(px(16.))
        .child(div().text_color(c(theme::TERTIARY)).child(label.to_owned()))
        .child(div().min_w_0().truncate().when(mono, |d| d.font_family(theme::mono())).child(value))
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
