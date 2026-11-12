//! Editor inspector, the right page: every export setting lives here. Per-track export gain and
//! mute; frame rate, resolution, size limit and fit; an Advanced area for set-once choices
//! (method, track mixing, custom frame rate); the output summary and the Export footer. The listen-only lane controls left of the timeline
//! (`timeline_listen.rs`) change the preview only.
use gpui::{
    AnyElement, Context, FontWeight, MouseButton, MouseDownEvent, MouseMoveEvent, SharedString, Window, canvas, div,
    prelude::*, px,
};

use super::{INSPECTOR_W, PANEL_MIN_H, TWO_COLUMNS_W, track_name};
use crate::{
    edit_model::{AudioOutput, ClipInfo, ExportMode, ExportPriority},
    editor_state::{Drag, GAIN_MAX, GAIN_MIN, timecode},
    root::Root,
    services::library,
    theme::{self, c},
    ui::{self, Icon, Variant, icon},
};

#[path = "editor_inspector_footer.rs"]
mod footer;
#[path = "editor_inspector_advanced.rs"]
mod advanced;

impl Root {
    /// The export page: beside the preview (`below` = None), or under the timeline in the
    /// portrait layout (`below` = the page width), where its two sections sit side by side when
    /// there is room and the footer keeps to the right. Same sections either way.
    pub(super) fn render_inspector(&mut self, window: &mut Window, cx: &mut Context<Self>, below: Option<f32>) -> AnyElement {
        let audio = self.render_audio_tracks(cx);
        let export = self.render_export_options(window, cx);
        let footer = self.render_export_footer(cx);
        let Some(width) = below else {
            return ui::panel()
                .w(px(INSPECTOR_W))
                .flex_none()
                .flex()
                .flex_col()
                .child(div().id("inspector-scroll").flex_1().min_h_0().overflow_y_scroll().child(audio).child(ui::hairline()).child(export))
                .child(footer)
                .into_any_element();
        };
        let sections = if width >= TWO_COLUMNS_W {
            div()
                .flex()
                .items_start()
                .child(div().flex_1().min_w_0().child(audio))
                .child(div().flex_1().min_w_0().border_l_1().border_color(c(theme::BORDER_SUBTLE)).child(export))
        } else {
            div().flex().flex_col().child(audio).child(ui::hairline()).child(export)
        };
        // Natural height (no stretching: the footer stays right under the settings); it shrinks
        // and scrolls when the page is short, so the timeline above keeps its height.
        ui::panel()
            .w_full()
            .flex_initial()
            .min_h(px(PANEL_MIN_H))
            .flex()
            .flex_col()
            .child(div().id("inspector-scroll").flex_1().min_h_0().overflow_y_scroll().child(sections))
            .child(div().flex().justify_end().child(div().w(px(INSPECTOR_W + 60.)).child(footer)))
            .into_any_element()
    }

    fn render_audio_tracks(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let tracks = self.ed.info.as_ref().map(|i| i.audio.clone()).unwrap_or_default();
        let count = tracks.len();
        let mut column = div().p(px(16.)).flex().flex_col().gap(px(14.)).child(ui::heading(
            "Export audio",
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
            let hint = self.ed.mix_hint(index);
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
                    )
                    // "Mix into main track" is set once: it lives under Advanced (`advanced.rs`).
                    // Its warning stays here, beside the track it is about.
                    .children(hint.map(|hint| div().text_size(px(12.)).line_height(px(16.)).text_color(c(theme::WARNING)).child(hint))),
            );
        }
        column.into_any_element()
    }

    /// The export's audio tracks in file order, e.g. "Mix (Game + Microphone) · Voice chat".
    fn export_audio_note(&self, info: &ClipInfo) -> String {
        let name = |stream: usize| {
            let index = info.audio.iter().position(|t| t.stream_index == stream).unwrap_or(0);
            track_name(info.audio.get(index).map_or("", |t| t.name.as_str()), index)
        };
        let indices: Vec<usize> = info.audio.iter().map(|t| t.stream_index).collect();
        let outputs: Vec<String> = self
            .ed
            .export_plan()
            .audio_outputs(&indices)
            .into_iter()
            .map(|output| match output {
                AudioOutput::Track(stream) => name(stream),
                AudioOutput::Mix(streams) => format!("Mix ({})", streams.into_iter().map(name).collect::<Vec<_>>().join(" + ")),
            })
            .collect();
        if outputs.is_empty() { "None".into() } else { outputs.join(" · ") }
    }

    /// The options changed from clip to clip: frame rate, resolution, size limit and what a limit
    /// keeps. Set-once choices sit under Advanced (`advanced.rs`). Picking a frame rate or a
    /// resolution yourself turns an automatic fit off (Manual); nothing here is ever locked.
    fn render_export_options(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let fps = self.ed.fps;
        let choices = self.ed.fps_choices();
        let recorded = self.ed.fps_ceiling();
        // A clip that finished loading after it opened can hold a rate outside the choices.
        if !choices.contains(&fps) { self.fps_custom = true; }
        let custom = self.fps_custom;
        let info = self.ed.info.clone();
        let kept = self.ed.plan.duration();
        let audio_note = info.as_ref().map_or_else(String::new, |i| self.export_audio_note(i));
        let priority = self.ed.export_settings.active_priority();
        let automatic = priority != ExportPriority::Manual;
        let mut track = ui::segment_track();
        for preset in choices {
            track = track.child(ui::segment(("fps", preset as usize), preset.to_string(), !custom && fps == preset)
                .child(crate::trace::target(format!("export-fps-{preset}")))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.fps_custom = false;
                    if automatic { this.ed.set_export_priority(ExportPriority::Manual); }
                    this.ed.set_fps(preset);
                    cx.notify();
                })));
        }
        let lossless = self.ed.lossless();
        let height = self.ed.export_settings.height;
        let cap = self.ed.export_settings.max_bytes;
        let mut sizes = ui::segment_track();
        // Ids keep the old labels: the check scripts click them.
        for (key, label, value) in [("Original", "Same as clip", None), ("1080p", "1080p", Some(1080u32)), ("720p", "720p", Some(720u32))] {
            sizes = sizes.child(ui::segment(SharedString::from(format!("h-{key}")), label, !automatic && height == value)
                .child(crate::trace::target(format!("export-resolution-{key}")))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if automatic { this.ed.set_export_priority(ExportPriority::Manual); }
                    this.ed.set_height(value);
                    cx.notify();
                })));
        }
        let mut caps = ui::segment_track();
        for (label, value) in [("No limit", None), ("10 MB", Some(10_000_000u64)), ("50 MB", Some(50_000_000u64)), ("500 MB", Some(500_000_000u64))] {
            let id = format!("cap-{label}");
            caps = caps.child(ui::segment(SharedString::from(id.clone()), label, cap == value).child(crate::trace::target(id)).on_click(cx.listener(move |this, _, _, cx| {
                this.ed.set_max_bytes(value);
                cx.notify();
            })));
        }
        // Manual is not a choice here: it is what picking a frame rate or resolution means.
        // Clicking the selected one again goes back to Manual.
        let mut priorities = ui::segment_track();
        for (key, label, value) in [("Motion", "Smooth motion", ExportPriority::Motion), ("Detail", "Sharp detail", ExportPriority::Detail)] {
            let id = format!("fit-{key}");
            priorities = priorities.child(ui::segment(SharedString::from(id.clone()), label, priority == value)
                .child(crate::trace::target(id))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let next = if this.ed.export_settings.active_priority() == value { ExportPriority::Manual } else { value };
                    this.ed.set_export_priority(next);
                    cx.notify();
                })));
        }
        let fit_note = match priority {
            ExportPriority::Motion => format!("Keeps {fps} fps and the largest picture that fits; below 720p it lowers the frame rate instead."),
            ExportPriority::Detail => format!("Can step down from {fps} fps to keep more detail."),
            ExportPriority::Manual => "Your frame rate and resolution stay; the limit sets the compression. Pick one to let Recall fit it.".into(),
        };
        let dimensions = self.ed.export_size().map_or_else(String::new, |(w, h)| {
            if priority == ExportPriority::Manual { format!("{w} × {h}") } else { format!("Up to {w} × {h}") }
        });
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
            .child(ui::heading("Export", if lossless { "Lossless trim" } else { "Re-encode" }));
        if lossless {
            column = column.child(note("Lossless trim copies the video untouched, in seconds. Each cut starts at the nearest earlier keyframe (shaded on the timeline). Frame rate, size and speed stay as recorded. Change the method under Advanced.".into()));
            if self.ed.lossless_blocked_by_speed() {
                column = column.child(
                    div().text_size(px(11.)).line_height(px(15.)).text_color(c(theme::WARNING)).child("A part has a speed change. Lossless can't change speed: set it back to 1× or export with Re-encode."),
                );
            }
        } else {
            let fps_note = if custom {
                format!("Custom {fps} fps, set under Advanced.")
            } else if automatic {
                String::new()
            } else {
                format!("Constant {fps} fps. Gaps hold the last captured frame. Recorded at {recorded} fps, the highest an export can use: for slow motion, set the part's Speed.")
            };
            column = column
                .child(field("Frame rate", track.into_any_element()).when(!fps_note.is_empty(), |d| d.child(note(fps_note))))
                .child(field("Resolution", sizes.into_any_element())
                    .when(automatic, |d| d.child(note("Recall picks it to fit the size limit. Pick one to choose it yourself.".into()))))
                .child(field("Size limit", caps.into_any_element()))
                .when(cap.is_some(), |d| d.child(field("With a limit, keep", priorities.into_any_element()).child(note(fit_note))));
        }
        column = column.child(self.render_export_advanced(window, cx));
        column
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .text_size(px(12.))
                    .child(detail_row("Length", format!("{} of {}", timecode(kept), timecode(self.ed.duration())), true))
                    .child(detail_row("Resolution", dimensions, false))
                    .child(detail_row("Audio", audio_note, false))
                    .child(detail_row(
                        "Saves to",
                        info.as_ref()
                            .and_then(|i| crate::backend::export_dir_for(std::path::Path::new(&i.path), &self.folders))
                            .map_or_else(|| "Recall’s export folder".to_owned(), |dir| dir.to_string_lossy().into_owned()),
                        false,
                    )),
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
