//! Display & HDR, Audio and Settings pages.
use gpui::{AnyElement, Context, Div, FontWeight, SharedString, Window, div, prelude::*, px};
use serde_json::{Value, json};

use crate::{
    root::{Root, ToastKind},
    services::{
        daemon::{self, DaemonCommand},
        meters::MeterState,
    },
    theme::{self, c},
    ui::{self, Icon, Variant, icon},
};

impl Root {
    /// Page content inside the shell panel: a header aligned with the library header,
    /// then a scrolling, left-aligned column.
    pub(crate) fn page_shell(&self, title: &str, subtitle: &str, body: Vec<AnyElement>) -> AnyElement {
        self.page_shell_tabs(title, subtitle, None, body)
    }

    /// Header (+ optional fixed tab row) and a scrolling, left-aligned column.
    pub(crate) fn page_shell_tabs(&self, title: &str, subtitle: &str, tabs: Option<AnyElement>, body: Vec<AnyElement>) -> AnyElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_baseline()
                    .gap(px(12.))
                    .px(px(24.))
                    .pt(px(20.))
                    .pb(px(16.))
                    .child(div().font_family(theme::display()).text_size(px(22.)).line_height(px(28.)).font_weight(FontWeight::BOLD).child(title.to_owned()))
                    .child(div().text_size(px(14.)).text_color(c(theme::TERTIARY)).child(subtitle.to_owned())),
            )
            .children(tabs.map(|t| div().flex_none().px(px(24.)).pb(px(16.)).child(t)))
            .child(
                div().id("page-scroll").flex_1().min_h_0().overflow_y_scroll().child(
                    div().max_w(px(760.)).px(px(24.)).pb(px(24.)).flex().flex_col().gap(px(20.)).children(body),
                ),
            )
            .into_any_element()
    }

    pub(crate) fn offline_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.snap.connected {
            return None;
        }
        Some(
            card()
                .flex_row()
                .items_center()
                .gap(px(12.))
                .child(ui::dot(theme::DISABLED, 8.))
                .child(div().flex_1().flex().flex_col().child(div().font_weight(FontWeight::MEDIUM).child("The recorder is off")).child(
                    div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child("Start it to see live status and change these settings."),
                ))
                .child(ui::button("start-recorder", Variant::Primary).child("Start recorder").on_click(cx.listener(|this, _, _, cx| {
                    match daemon::start_controller() {
                        Ok(_) => this.show_toast(ToastKind::Info, "Starting the recorder…", None),
                        Err(error) => this.show_toast(ToastKind::Bad, "Couldn't start the recorder", Some(error)),
                    }
                    cx.notify();
                })))
                .into_any_element(),
        )
    }

    pub(crate) fn config_value(&self, section: &str, key: &str) -> Option<&Value> {
        self.snap.config.as_ref()?.get(section)?.get(key)
    }

    pub(crate) fn config_bool(&self, section: &str, key: &str) -> bool {
        self.config_value(section, key).and_then(Value::as_bool).unwrap_or(false)
    }

    pub(crate) fn set_config(&mut self, section: &str, key: &str, value: Value) {
        let mut patch = serde_json::Map::new();
        patch.insert(section.into(), json!({ key: value }));
        self.send(DaemonCommand::SetConfig(Value::Object(patch)));
        self.send(DaemonCommand::GetConfig);
    }

    // ---- Display & HDR ----------------------------------------------------

    pub fn render_hdr(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let session = self.snap.session.clone().unwrap_or_default();
        let hdr = session.hdr.clone();
        let select = self.snap.engine.as_ref().map(|e| e.select.clone()).unwrap_or_else(|| "auto".into());
        let auto = self.config_bool("behavior", "hdr_auto");
        let display = hdr.display.clone().unwrap_or_else(|| "Managed display".into());
        let owner = if hdr.manual_pin {
            "Pinned by hand · Toggle HDR again to release".to_owned()
        } else if let Some(target) = session.target.as_ref().filter(|_| hdr.auto) {
            format!("{} owns the display", target.name)
        } else if hdr.auto {
            "Automatic: HDR games turn HDR on".to_owned()
        } else {
            "Automatic switching is off".to_owned()
        };
        let mut body = vec![];
        body.extend(self.offline_banner(cx));
        body.push(
            card()
                .flex_row()
                .items_center()
                .gap(px(16.))
                .child(icon(Icon::Sun, 28., if hdr.hdr_on { theme::TEXT } else { theme::DISABLED }))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(if !self.snap.connected {
                            "HDR status unavailable".to_owned()
                        } else if !hdr.available {
                            "Display not found".to_owned()
                        } else if hdr.hdr_on {
                            format!("HDR is on · {display}")
                        } else {
                            format!("HDR is off · {display}")
                        }))
                        .child(div().text_color(c(theme::TERTIARY)).child(if !self.snap.connected { "Start the recorder to read and switch HDR.".to_owned() } else if hdr.available { owner } else { "Recall can't find the display it manages. Connect it, or check that it is on, to use these controls.".into() })),
                )
                .child(ui::button("toggle-hdr", Variant::Secondary).when(!hdr.available, |b| b.opacity(0.4)).child("Toggle HDR now").on_click(cx.listener(|this, _, _, cx| {
                    this.send(DaemonCommand::ToggleHdr);
                    cx.notify();
                })))
                .into_any_element(),
        );
        body.push(
            setting_row("Switch HDR for HDR games", "Turns HDR on while an HDR game is in front, and off afterwards. Off leaves the display alone.")
                .child(ui::switch("hdr-auto", auto).on_click(cx.listener(move |this, _, _, cx| {
                    this.set_config("behavior", "hdr_auto", json!(!auto));
                    cx.notify();
                })))
                .into_any_element(),
        );
        let modes = [("auto", "Auto"), ("sdr", "Force SDR"), ("hdr", "Force tone map")];
        let mut mode_track = ui::segment_track().w(px(320.));
        for (value, label) in modes {
            mode_track = mode_track.child(ui::segment(SharedString::from(format!("mode-{value}")), label, select == value).on_click(cx.listener(move |this, _, _, cx| {
                this.send(DaemonCommand::SetMode(value.into()));
                cx.notify();
            })));
        }
        body.push(setting_row("Recording tone mapping", "How HDR games are converted for SDR clips. Auto follows the display.").child(mode_track).into_any_element());
        let profile = self.ac_profile.clone().unwrap_or_default().to_lowercase();
        let mut profile_track = ui::segment_track().w(px(220.));
        for (value, label) in [("punchy", "Punchy"), ("accurate", "Accurate")] {
            profile_track = profile_track.child(ui::segment(SharedString::from(format!("ac-{value}")), label, profile.contains(value)).on_click(cx.listener(move |this, _, _, cx| {
                this.send(DaemonCommand::SetAcProfile(value.into()));
                cx.notify();
            })));
        }
        // The colour profiles are made for one monitor model: the row shows only where they are installed.
        if ac_profiles_installed() {
            body.push(setting_row("Desktop colour in HDR", "Gamma for SDR apps while HDR is on. Applies the next time HDR turns on.").child(profile_track).into_any_element());
        }
        let subtitle = format!("HDR switching for {}", if display == "Managed display" { "your main display" } else { display.as_str() });
        self.page_shell("Display & HDR", &subtitle, body)
    }

    // ---- Audio ------------------------------------------------------------

    // ---- Settings ---------------------------------------------------------
}

pub(crate) fn card() -> Div {
    div().p(px(16.)).rounded(px(10.)).bg(c(theme::RAISED)).border_1().border_color(c(theme::BORDER_SUBTLE)).flex().flex_col()
}

pub(crate) fn section_title(text: &str) -> AnyElement {
    div().pt(px(8.)).child(ui::caption(text.to_owned())).into_any_element()
}

/// The Punchy/Accurate MHC2 profiles (`recall-hdr` `ac_profile_file`) are in the Windows colour
/// store. Checked once per launch.
fn ac_profiles_installed() -> bool {
    static FOUND: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FOUND.get_or_init(|| {
        let store = std::env::var_os("WINDIR").map(std::path::PathBuf::from).unwrap_or_default().join(r"System32\spool\drivers\color");
        ["PG27AQDP_Punchy-Gamma2.2_MHC2.icm", "PG27AQDP_sRGB-to-Gamma2.2_MHC2.icm"].iter().all(|f| store.join(f).is_file())
    })
}

pub(crate) fn setting_row(title: &str, detail: &str) -> Div {
    setting_row_with(title, None, detail)
}

/// [`setting_row`] with an element (an info button) right after the title.
pub(crate) fn setting_row_with(title: &str, after_title: Option<AnyElement>, detail: &str) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(24.))
        .py(px(4.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(div().font_weight(FontWeight::MEDIUM).child(title.to_owned()))
                        .children(after_title),
                )
                .when(!detail.is_empty(), |d| d.child(div().text_size(px(12.)).line_height(px(16.)).text_color(c(theme::TERTIARY)).child(detail.to_owned()))),
        )
}


impl Root {
    /// Meter ballistics per frame: fast rise (~30 ms), 26 dB/s fall, 0.8 s peak hold
    /// then 18 dB/s fall. Returns (displayed RMS dB, displayed peak dB) per meter.
    pub(crate) fn meter_ballistics(&mut self, readings: &[crate::services::meters::MeterReading]) -> Vec<(f32, f32)> {
        let now = std::time::Instant::now();
        let dt = now.duration_since(self.frame_at).as_secs_f32().min(0.1);
        self.frame_at = now;
        if self.meter_display.len() != readings.len() {
            self.meter_display = readings.iter().map(|_| (-90.0, -90.0, now)).collect();
        }
        readings
            .iter()
            .zip(self.meter_display.iter_mut())
            .map(|(r, (rms, hold, at))| {
                let target = if r.state == MeterState::Live { r.rms_db.max(-90.0) } else { -90.0 };
                if target > *rms {
                    *rms += (target - *rms) * (dt / 0.03).min(1.0);
                } else {
                    *rms = (*rms - 26.0 * dt).max(target);
                }
                let peak = if r.state == MeterState::Live { r.peak_db.max(-90.0) } else { -90.0 };
                if peak >= *hold {
                    *hold = peak;
                    *at = now;
                } else if now.duration_since(*at).as_secs_f32() > 0.8 {
                    *hold = (*hold - 18.0 * dt).max(*rms).max(-90.0);
                }
                (*rms, *hold)
            })
            .collect()
    }
}
