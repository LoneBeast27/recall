//! Display & HDR, Audio and Settings pages.
use gpui::{AnyElement, Context, Div, FontWeight, SharedString, Window, div, prelude::*, px, relative};
use serde_json::{Value, json};

use crate::{
    root::{Root, ToastKind},
    services::{
        daemon::{self, DaemonCommand},
        meters::{MeterState, Meters, bar_fraction, meter_specs},
        midi::Midi,
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
                        .child(div().text_color(c(theme::TERTIARY)).child(if !self.snap.connected { "Start the recorder to read and switch HDR.".to_owned() } else if hdr.available { owner } else { "Recall manages HDR on the PG27AQDP. Connect it to use these controls.".into() })),
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

    /// Start or replace the meters and the MIDI monitor so they follow the settings: the
    /// recorded desktop output and microphone, and the chosen MIDI input ("" = none).
    pub(crate) fn sync_audio_monitors(&mut self) {
        let text = |this: &Self, section: &str, key: &str| this.config_value(section, key).and_then(Value::as_str).unwrap_or("").trim().to_owned();
        let mic_on = self.config_value("recording", "mic_enabled").and_then(Value::as_bool).unwrap_or(true);
        let specs = meter_specs(&text(self, "recording", "desktop_audio_device"), &text(self, "recording", "mic_device"), mic_on);
        if self.meters.as_ref().is_none_or(|m| m.specs() != specs.as_slice()) {
            self.meters = Some(Meters::spawn(specs));
        }
        let port = text(self, "midi", "input");
        if port.is_empty() {
            self.midi = None;
        } else if self.midi.as_ref().is_none_or(|m| m.wanted() != port) {
            self.midi = Some(Midi::spawn(port));
        }
    }

    pub fn render_audio(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.sync_audio_monitors();
        let readings = self.meters.as_ref().map(|m| m.snapshot()).unwrap_or_default();
        let shown = self.meter_ballistics(&readings);
        let strips = readings.into_iter().zip(shown).map(|(r, (rms_db, hold_db))| {
            let live = r.state == MeterState::Live;
            let rms = bar_fraction(rms_db);
            let peak = bar_fraction(hold_db);
            let clipping = live && hold_db > -1.0;
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.))
                .when(!live, |d| d.opacity(0.45))
                .child(
                    div()
                        .w(px(14.))
                        .h(px(160.))
                        .relative()
                        .rounded(px(4.))
                        .bg(c(theme::RAISED))
                        .border_1()
                        .border_color(c(theme::BORDER_SUBTLE))
                        .child(div().absolute().bottom_0().left_0().right_0().h(relative(rms)).rounded(px(3.)).bg(c(theme::MUTED)))
                        .when(live, |d| d.child(div().absolute().left_0().right_0().bottom(relative(peak)).h(px(2.)).bg(c(if clipping { theme::SIGNAL } else { theme::TEXT })))),
                )
                .child(div().font_weight(FontWeight::MEDIUM).child(r.label.clone()))
                .child(div().font_family(theme::mono()).text_size(px(11.)).text_color(c(theme::TERTIARY)).child(match &r.state {
                    MeterState::Live => format!("{:.0} dB", r.rms_db.max(-90.0)),
                    MeterState::Missing => "Not found".into(),
                    MeterState::Error(_) => "Error".into(),
                }))
        });
        let midi = self.midi.as_ref();
        let port = midi.and_then(|m| m.monitored_port());
        let outputs = midi.map(|m| m.ports().outputs).unwrap_or_default();
        let recent = midi.map(|m| m.recent()).unwrap_or_default();
        let mut body = vec![
            card()
                .gap(px(16.))
                .child(ui::heading("Levels", "Live"))
                .child(div().flex().gap(px(8.)).children(strips))
                .child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child("The desktop audio and microphone your clips record. Change them in Settings → Clip."))
                .into_any_element(),
        ];
        body.push(
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
                .into_any_element(),
        );
        self.page_shell("Audio", "Levels and MIDI", body)
    }

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
                .child(div().font_weight(FontWeight::MEDIUM).child(title.to_owned()))
                .when(!detail.is_empty(), |d| d.child(div().text_size(px(12.)).line_height(px(16.)).text_color(c(theme::TERTIARY)).child(detail.to_owned()))),
        )
}


impl Root {
    /// Meter ballistics per frame: fast rise (~30 ms), 26 dB/s fall, 0.8 s peak hold
    /// then 18 dB/s fall. Returns (displayed RMS dB, displayed peak dB) per meter.
    fn meter_ballistics(&mut self, readings: &[crate::services::meters::MeterReading]) -> Vec<(f32, f32)> {
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
