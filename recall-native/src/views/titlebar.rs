//! Custom title bar: mark + breadcrumb, live recorder pill, Save replay, window controls.
use gpui::{
    AnyElement, Context, FontWeight, Window, WindowControlArea, div, prelude::*, px,
};

use crate::{
    root::{Root, Route, ToastKind},
    services::{daemon::{self, DaemonCommand}, library},
    theme::{self, c},
    ui::{self, Icon, Variant, icon},
};

pub const TITLEBAR_H: f32 = 44.0;
const TICKS: usize = 30;

impl Root {
    pub fn render_titlebar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let maximized = window.is_maximized();
        div()
            .h(px(TITLEBAR_H))
            .flex_none()
            .flex()
            .items_center()
            .pl(px(16.))
            .child(
                div()
                    .w(px(400.))
                    .h_full()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .window_control_area(WindowControlArea::Drag)
                    .child(mark(18.))
                    .child(self.render_breadcrumb(cx)),
            )
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .window_control_area(WindowControlArea::Drag)
                    .child(self.render_status_pill(cx)),
            )
            .child(
                div()
                    .w(px(400.))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .child(div().flex_1().h_full().window_control_area(WindowControlArea::Drag))
                    .child(self.render_save_button(cx))
                    .child(
                        div()
                            .flex()
                            .ml(px(4.))
                            .child(window_button("win-min", Icon::WinMin, WindowControlArea::Min, false))
                            .child(window_button(
                                "win-max",
                                if maximized { Icon::WinRestore } else { Icon::WinMax },
                                WindowControlArea::Max,
                                false,
                            ))
                            .child(window_button("win-close", Icon::WinClose, WindowControlArea::Close, true)),
                    ),
            )
            .into_any_element()
    }

    fn render_breadcrumb(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.route != Route::Editor {
            return div()
                .text_size(px(14.))
                .font_weight(FontWeight::SEMIBOLD)
                .child("Recall")
                .into_any_element();
        }
        let path = self.ed.info.as_ref().map(|i| i.path.clone()).or_else(|| self.ed.loading_path.clone());
        let record = path.as_deref().and_then(|p| self.lib.record(p));
        let title = record.map_or_else(|| "Clip".to_owned(), |r| r.display_title().to_owned());
        let when = record.map(|r| stamp_label(r.started)).unwrap_or_default();
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .min_w_0()
            .child(
                div()
                    .id("crumb-library")
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .ml(px(6.))
                    .px(px(6.))
                    .h(px(26.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_color(c(theme::MUTED))
                    .hover(|s| s.bg(c(theme::OVERLAY)).text_color(c(theme::TEXT)))
                    .child(icon(Icon::ChevronLeft, 14., theme::MUTED))
                    .child("Library")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.go(Route::Library);
                        cx.notify();
                    })),
            )
            .child(div().text_color(c(theme::BORDER_STRONG)).child("/"))
            .child(div().font_weight(FontWeight::SEMIBOLD).truncate().child(title))
            .child(div().text_color(c(theme::TERTIARY)).whitespace_nowrap().child(when))
            .into_any_element()
    }

    fn render_status_pill(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let snap = &self.snap;
        let session = snap.session.as_ref();
        let engine = snap.engine.as_ref();
        let pill = div()
            .id("status-pill")
            .h(px(28.))
            .flex()
            .items_center()
            .gap(px(12.))
            .pl(px(10.))
            .pr(px(12.))
            .rounded(px(14.))
            .bg(c(theme::RAISED))
            .border_1()
            .border_color(c(theme::BORDER_SUBTLE));
        if !snap.connected {
            return pill
                .cursor_pointer()
                .hover(|s| s.bg(c(theme::OVERLAY)))
                .child(ui::dot(theme::DISABLED, 7.))
                .child(div().text_color(c(theme::MUTED)).child("Recorder off"))
                .child(div().text_color(c(theme::TEXT)).font_weight(FontWeight::MEDIUM).child("Start"))
                .on_click(cx.listener(|this, _, _, cx| {
                    match daemon::start_controller() {
                        Ok(_) => this.show_toast(ToastKind::Info, "Starting the recorder…", None),
                        Err(error) => this.show_toast(ToastKind::Bad, "Couldn't start the recorder", Some(error)),
                    }
                    cx.notify();
                }))
                .into_any_element();
        }
        let failed = engine.is_some_and(|e| e.encoder == "failed" || e.encoder_error.is_some());
        let waiting = session.is_some_and(|s| s.state == "waiting") && !snap.replay_ready();
        let name = session
            .and_then(|s| s.target.as_ref().map(|t| t.name.clone()))
            .filter(|n| !n.is_empty())
            .or_else(|| engine.map(|e| e.source.clone()).filter(|n| !n.is_empty()))
            .unwrap_or_else(|| "Recorder".into());
        let mic_muted = session.is_some_and(|s| s.mic_muted) || engine.is_some_and(|e| e.mic_muted);
        if failed {
            let reason = engine.and_then(|e| e.encoder_error.clone()).unwrap_or_else(|| "Encoder failed".into());
            return pill
                .child(ui::dot(theme::ERROR, 7.))
                .child(div().text_color(c(theme::ERROR)).child("Encoder failed"))
                .child(div().max_w(px(260.)).truncate().text_color(c(theme::TERTIARY)).child(reason))
                .into_any_element();
        }
        if waiting {
            return pill
                .child(ui::dot(theme::MUTED, 7.))
                .child(div().text_color(c(theme::MUTED)).child("Waiting for a game"))
                .into_any_element();
        }
        let buffered = engine.map_or(0.0, |e| e.buffer_secs as f64);
        let capacity = session.map(|s| s.save_seconds).filter(|s| *s > 0).unwrap_or(120) as f64;
        let filled = ((buffered / capacity) * TICKS as f64).round().clamp(0.0, TICKS as f64) as usize;
        let ready = snap.replay_ready();
        let fps = engine.map_or(0, |e| e.fps);
        div()
            .child(
                pill.child(ui::dot(if ready { theme::SIGNAL } else { theme::WARNING }, 7.))
                    .child(div().font_weight(FontWeight::MEDIUM).max_w(px(180.)).truncate().child(name))
                    .child(div().flex().items_center().gap(px(2.)).children((0..TICKS).map(|i| {
                        div()
                            .w(px(2.))
                            .h(px(10.))
                            .rounded(px(1.))
                            .bg(c(if i < filled { theme::SIGNAL } else { theme::TOP }))
                    })))
                    .child(
                        div()
                            .font_family(theme::MONO)
                            .text_size(px(12.))
                            .child(if ready { library::format_duration(buffered) } else { "Starting".into() }),
                    )
                    .when(fps > 0, |d| {
                        d.child(div().font_family(theme::MONO).text_size(px(12.)).text_color(c(theme::TERTIARY)).child(format!("{fps} fps")))
                    })
                    .when(mic_muted, |d| d.child(icon(Icon::MicOff, 14., theme::TERTIARY))),
            )
            .into_any_element()
    }

    fn render_save_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let ready = self.snap.replay_ready();
        let hotkey = self
            .snap
            .session
            .as_ref()
            .map(|s| s.save_hotkey.replace('+', " "))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Alt N".into());
        ui::button("save-replay", Variant::Secondary)
            .h(px(30.))
            .pr(px(6.))
            .when(!ready, |b| b.opacity(0.45))
            .child("Save replay")
            .child(ui::kbd(hotkey))
            .on_click(cx.listener(move |this, _, _, cx| {
                if ready {
                    this.send(DaemonCommand::SaveClip { seconds: None });
                } else {
                    this.show_toast(ToastKind::Info, "Nothing to save yet", Some("The replay buffer fills while a game runs".into()));
                }
                cx.notify();
            }))
            .into_any_element()
    }

    pub fn render_toast(&self) -> Option<AnyElement> {
        let toast = self.toast.as_ref()?;
        let color = match toast.kind {
            ToastKind::Good => theme::ACTIVE,
            ToastKind::Info => theme::MUTED,
            ToastKind::Bad => theme::ERROR,
        };
        Some(
            div()
                .absolute()
                .bottom(px(20.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .max_w(px(420.))
                        .px(px(14.))
                        .py(px(10.))
                        .flex()
                        .gap(px(10.))
                        .items_start()
                        .rounded(px(10.))
                        .bg(c(theme::RAISED))
                        .border_1()
                        .border_color(c(theme::BORDER))
                        .shadow(vec![gpui::BoxShadow {
                            color: theme::ca(0x000000, 0.5),
                            offset: gpui::point(px(0.), px(8.)),
                            blur_radius: px(24.),
                            spread_radius: px(0.),
                        }])
                        .child(div().pt(px(6.)).child(ui::dot(color, 7.)))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w_0()
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(toast.text.clone()))
                                .children(toast.detail.clone().map(|d| {
                                    div().text_size(px(12.)).line_height(px(16.)).text_color(c(theme::TERTIARY)).child(d)
                                })),
                        ),
                )
                .into_any_element(),
        )
    }
}

/// Recall mark: a ring with a filled centre.
pub fn mark(size: f32) -> gpui::Div {
    div()
        .size(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(size / 2.))
        .border(px(1.5))
        .border_color(c(theme::TEXT))
        .child(div().size(px(size / 3.)).rounded(px(size / 6.)).bg(c(theme::TEXT)))
}

fn window_button(id: &'static str, kind: Icon, area: WindowControlArea, close: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .w(px(46.))
        .h(px(TITLEBAR_H))
        .flex()
        .items_center()
        .justify_center()
        .window_control_area(area)
        .hover(move |s| if close { s.bg(c(0xc42b1c)) } else { s.bg(c(theme::OVERLAY)) })
        .child(icon(kind, 12., theme::MUTED))
}

/// "Sat 17 Oct · 22:21"
pub fn stamp_label(stamp: library::LocalStamp) -> String {
    if stamp.year == 0 {
        return String::new();
    }
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let month = MONTHS.get(stamp.month.saturating_sub(1) as usize).copied().unwrap_or("");
    format!("{} {month} · {:02}:{:02}", stamp.day, stamp.hour, stamp.minute)
}
