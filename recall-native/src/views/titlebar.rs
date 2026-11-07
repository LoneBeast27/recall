//! Custom title bar: mark + breadcrumb, live recorder pill, Save replay, window controls.
use gpui::{AnyElement, Context, FontWeight, MouseButton, Window, div, prelude::*, px};

use crate::{
    views::window_frame::{Caption, caption_button},
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
            .on_mouse_move(cx.listener(Self::title_move))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.title_drag = None))
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
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::title_down))
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
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::title_down))
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
                    .child(div().flex_1().h_full().on_mouse_down(MouseButton::Left, cx.listener(Self::title_down)))
                    .child(self.render_stats_button(cx))
                    .child(self.render_record_button(cx))
                    .child(self.render_save_button(cx))
                    .child(
                        div()
                            .flex()
                            .ml(px(4.))
                            .child(caption_button("win-min", Icon::WinMin, Caption::Minimize))
                            .child(caption_button("win-max", if maximized { Icon::WinRestore } else { Icon::WinMax }, Caption::Maximize))
                            .child(caption_button("win-close", Icon::WinClose, Caption::Close)),
                    ),
            )
            .into_any_element()
    }

    fn render_breadcrumb(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.route != Route::Editor {
            return div()
                .font_family(theme::display())
                .text_size(px(16.))
                .font_weight(FontWeight::BOLD)
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
                    .occlude()
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
                    .child(crate::trace::target("crumb-library"))
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
            .occlude()
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
        if self.recorder_unresponsive {
            // Connected, but no status reply for 5 s: the last snapshot is not the truth.
            return pill
                .child(ui::dot(theme::WARNING, 7.))
                .child(div().text_color(c(theme::MUTED)).child("Recorder not responding"))
                .into_any_element();
        }
        if session.is_some_and(|s| s.state == "paused") {
            return pill
                .cursor_pointer()
                .hover(|s| s.bg(c(theme::OVERLAY)))
                .child(ui::dot(theme::DISABLED, 7.))
                .child(div().text_color(c(theme::MUTED)).child("Recording paused"))
                .child(div().text_color(c(theme::TEXT)).font_weight(FontWeight::MEDIUM).child("Resume"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.send(DaemonCommand::Resume);
                    cx.notify();
                }))
                .into_any_element();
        }
        let failed = engine.is_some_and(|e| e.encoder == "failed" || e.encoder_error.is_some());
        // The game closed: its last clip stays saveable while this counts down to 0:00.
        let drain = session
            .filter(|s| s.state == "draining")
            .and_then(|s| Some((s.drain.as_ref()?, s.drain_left(daemon::unix_ms(), std::time::Instant::now())?)))
            .filter(|(_, left)| *left > 0);
        let waiting = drain.is_none() && session.is_some_and(|s| s.state == "waiting" || s.state == "draining");
        let name = drain
            .map(|(d, _)| d.name.clone())
            .or_else(|| session.and_then(|s| s.target.as_ref().map(|t| t.name.clone())))
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
        let buffered = drain.map_or_else(|| engine.map_or(0.0, |e| e.buffer_secs as f64), |(_, left)| left as f64);
        let save_seconds = session.map(|s| s.save_seconds).filter(|s| *s > 0).unwrap_or(120);
        let capacity = drain.map_or(save_seconds as u64, |(d, _)| d.total_secs.max(1)) as f64;
        let filled = ((buffered / capacity) * TICKS as f64).round().clamp(0.0, TICKS as f64) as usize;
        let ready = drain.is_some() || snap.replay_ready();
        let fps = if drain.is_some() { 0 } else { engine.map_or(0, |e| e.fps) };
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
                            .font_family(theme::mono())
                            .text_size(px(12.))
                            .child(if ready { library::format_duration(buffered) } else { "Starting".into() }),
                    )
                    .when(drain.is_some(), |d| d.child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child("left to save")))
                    .when(fps > 0, |d| {
                        d.child(div().font_family(theme::mono()).text_size(px(12.)).text_color(c(theme::TERTIARY)).child(format!("{fps} fps")))
                    })
                    .when(mic_muted, |d| d.child(icon(Icon::MicOff, 14., theme::TERTIARY))),
            )
            .into_any_element()
    }

    /// Quick toggle for the in-game performance-stats island; pressed while it is on.
    fn render_stats_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let on = self.stats_on();
        ui::icon_button("stats-toggle", Icon::Sliders, 30., if on { theme::TEXT } else { theme::MUTED }, on)
            .when(!on, |b| b.border_1().border_color(c(theme::BORDER)).bg(c(theme::RAISED)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.send(DaemonCommand::ToggleStats);
                cx.notify();
            }))
            .into_any_element()
    }

    /// Manual long recording: a record dot while idle; the running time while recording.
    fn render_record_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let session = self.snap.session.as_ref();
        let long = session.map(|s| s.long_recording.clone()).unwrap_or_default();
        let ready = self.snap.replay_ready();
        let toggle = cx.listener(move |this, _, _, cx| {
            if ready || long.active {
                this.send(DaemonCommand::RecordToggle);
            } else {
                this.show_toast(ToastKind::Info, "Nothing to record yet", Some("Start a game, then record it in full.".into()));
            }
            cx.notify();
        });
        if long.active {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
            let elapsed = long.elapsed_at(now, session.and_then(|s| s.since_status(std::time::Instant::now())));
            return ui::button("long-rec", Variant::Secondary)
                .h(px(30.))
                .gap(px(8.))
                .border_color(c(theme::SIGNAL))
                .child(div().size(px(9.)).rounded(px(2.)).bg(c(theme::SIGNAL)))
                .child(div().font_family(theme::mono()).text_size(px(12.)).text_color(c(theme::SIGNAL)).child(library::format_duration(elapsed)))
                .on_click(toggle)
                .into_any_element();
        }
        div()
            .id("long-rec")
            .size(px(30.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .border_1()
            .border_color(c(theme::BORDER))
            .bg(c(theme::RAISED))
            .cursor_pointer()
            .hover(|s| s.bg(c(theme::OVERLAY)).border_color(c(theme::BORDER_STRONG)))
            .when(!ready, |b| b.opacity(0.45))
            .child(ui::dot(theme::SIGNAL, 10.))
            .on_click(toggle)
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

    pub fn render_toast(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let toast = self.toast.as_ref()?;
        // A source removed on the Audio page can come back from its toast.
        let undo = self.settings_ui.audio.undo_open().then(|| {
            ui::button("toast-undo", ui::Variant::Secondary).child("Undo").on_click(cx.listener(|this, _, _, cx| {
                this.undo_remove_source();
                cx.notify();
            }))
        });
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
                        )
                        .children(undo),
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


/// "Sat 17 Oct · 22:21"
pub fn stamp_label(stamp: library::LocalStamp) -> String {
    if stamp.year == 0 {
        return String::new();
    }
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let month = MONTHS.get(stamp.month.saturating_sub(1) as usize).copied().unwrap_or("");
    format!("{} {month} · {:02}:{:02}", stamp.day, stamp.hour, stamp.minute)
}
