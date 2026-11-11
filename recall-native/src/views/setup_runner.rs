//! Guided setup uses the same storage, audio, and video controls as Recall's normal pages.
use gpui::{AnyElement, Context, FontWeight, Window, div, prelude::*, px};
use serde_json::json;
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CYSCREEN};

use crate::{
    root::{Root, Route, ToastKind},
    services::daemon::{self, DaemonCommand},
    theme::{self, c},
    ui::{self, Variant},
    views::settings::SettingsTab,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SetupStep {
    Storage,
    Audio,
    Quality,
}

impl SetupStep {
    const ALL: [Self; 3] = [Self::Storage, Self::Audio, Self::Quality];

    fn title(self) -> &'static str {
        match self {
            Self::Storage => "Storage",
            Self::Audio => "Audio",
            Self::Quality => "Recording quality",
        }
    }

    fn next(self) -> Option<Self> {
        match self {
            Self::Storage => Some(Self::Audio),
            Self::Audio => Some(Self::Quality),
            Self::Quality => None,
        }
    }

    fn previous(self) -> Option<Self> {
        match self {
            Self::Storage => None,
            Self::Audio => Some(Self::Storage),
            Self::Quality => Some(Self::Audio),
        }
    }
}

pub(crate) struct SetupRunner {
    step: SetupStep,
    record_after: bool,
    bitrate: u64,
    advanced_quality: bool,
    quality_detail: bool,
}

fn recommended_bitrate(display_height: i32) -> u64 {
    match display_height {
        1..=1200 => 15,
        1801.. => 40,
        _ => 25,
    }
}

impl Root {
    pub(crate) fn begin_setup(&mut self, record_after: bool) {
        let height = unsafe { GetSystemMetrics(SM_CYSCREEN) };
        self.setup = Some(SetupRunner {
            step: SetupStep::Storage,
            record_after,
            bitrate: recommended_bitrate(height),
            advanced_quality: false,
            quality_detail: false,
        });
        self.setup_page(SetupStep::Storage);
    }

    pub(crate) fn start_setup_recorder(&mut self) {
        match daemon::start_controller_for_setup() {
            Ok(true) => self.show_toast(ToastKind::Info, "Connecting to the paused recorder…", None),
            Ok(false) => {}
            Err(error) => self.show_toast(ToastKind::Bad, "Couldn't open recorder settings", Some(error)),
        }
    }

    pub(crate) fn setup_page(&mut self, step: SetupStep) {
        let Some(setup) = self.setup.as_mut() else { return };
        setup.step = step;
        self.settings_ui.quality_preview = None;
        match step {
            SetupStep::Storage => {
                self.settings_tab = SettingsTab::Storage;
                self.go(Route::Settings);
            }
            SetupStep::Audio => self.go(Route::Audio),
            SetupStep::Quality => {
                self.settings_tab = SettingsTab::Recording;
                self.go(Route::Settings);
            }
        }
    }

    fn finish_setup(&mut self) {
        let Some(setup) = self.setup.take() else { return };
        if !crate::TESTING {
            self.finish_setup_recording(setup.record_after);
        }
        self.settings_ui.quality_preview = None;
        self.go(Route::Library);
    }

    fn setup_quality(&mut self, bitrate: u64, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mut body = Vec::new();
        body.extend(self.offline_banner(cx));
        let selected = self.config_value("recording", "codec").and_then(|v| v.as_str()) == Some("h264")
            && self.config_value("recording", "fps").and_then(|v| v.as_u64()) == Some(60)
            && self.config_value("recording", "bitrate_mbps").and_then(|v| v.as_u64()) == Some(bitrate)
            && self.config_value("recording", "quality").and_then(|v| v.as_str()) == Some("balanced");
        body.push(
            div()
                .flex().flex_col().gap(px(8.))
                .child(div().font_weight(FontWeight::SEMIBOLD).child("Recommended for this display"))
                .child(div().text_color(c(theme::MUTED)).child(format!("H.264 · 60 fps · {bitrate} Mbps video target · Balanced encoder effort")))
                .child(
                    div().flex().items_center().gap(px(10.))
                        .child(ui::button("setup-quality-use", Variant::Secondary)
                            .child(if selected { "Recommended settings active" } else { "Use recommended settings" })
                            .child(crate::trace::target("setup-quality-use".to_owned()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.send(DaemonCommand::SetConfig(json!({
                                    "recording": { "codec": "h264", "fps": 60, "bitrate_mbps": bitrate, "quality": "balanced" }
                                })));
                                this.daemon.send(DaemonCommand::GetConfig);
                                cx.notify();
                            }))),
                )
                .into_any_element(),
        );
        let detail = self.setup.as_ref().is_some_and(|setup| setup.quality_detail);
        body.push(
            div().flex().items_center().gap(px(12.))
                .child(div().font_weight(FontWeight::SEMIBOLD).child("Recorded video sample"))
                .child(ui::button("setup-quality-detail", Variant::Ghost)
                    .child(if detail { "Show full frame" } else { "Show detail crop" })
                    .child(crate::trace::target("setup-quality-detail".to_owned()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(setup) = this.setup.as_mut() {
                            setup.quality_detail = !setup.quality_detail;
                        }
                        cx.notify();
                    })))
                .into_any_element(),
        );
        body.push(crate::views::quality_preview::setup_comparison(bitrate, detail));
        body.push(div().text_size(px(12.)).text_color(c(theme::MUTED))
            .child("Audio uses a separate 192 kbps AAC target per track.").into_any_element());
        let advanced = self.setup.as_ref().is_some_and(|setup| setup.advanced_quality);
        body.push(
            div()
                .child(ui::button("setup-quality-advanced", Variant::Secondary)
                    .child(if advanced { "Hide advanced settings" } else { "Advanced quality settings" })
                    .child(crate::trace::target("setup-quality-advanced".to_owned()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(setup) = this.setup.as_mut() {
                            setup.advanced_quality = !setup.advanced_quality;
                        }
                        this.settings_ui.quality_preview = None;
                        cx.notify();
                    })))
                .into_any_element(),
        );
        if advanced {
            body.extend(self.render_video_section(window, cx));
        }
        self.page_shell("Recording quality", "Try the recommendation or adjust the settings below.", body)
    }

    pub(crate) fn render_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let setup = self.setup.as_ref().expect("setup route has a runner");
        let (step, bitrate) = (setup.step, setup.bitrate);
        let page = match step {
            SetupStep::Storage if crate::TESTING => self.render_testing_storage(cx),
            SetupStep::Storage => {
                let mut body = Vec::new();
                body.extend(self.offline_banner(cx));
                body.extend(self.settings_storage(window, cx));
                self.page_shell("Storage", "Choose where clips go, the storage limit, and the SSD cache size.", body)
            }
            SetupStep::Audio => self.render_audio(window, cx),
            SetupStep::Quality => self.setup_quality(bitrate, window, cx),
        };
        let rail = div()
            .w(px(188.)).flex_none().px(px(18.)).pt(px(24.))
            .flex().flex_col().gap(px(10.))
            .child(div().font_family(theme::display()).text_size(px(20.)).font_weight(FontWeight::BOLD).child(if crate::TESTING { "RecallTesting" } else { "Recall setup" }))
            .children(SetupStep::ALL.into_iter().enumerate().map(|(i, candidate)| {
                div().px(px(8.)).py(px(7.)).rounded(px(5.))
                    .when(candidate == step, |d| d.bg(c(theme::FLOATING)))
                    .text_color(c(if candidate == step { theme::TEXT } else { theme::MUTED }))
                    .child(format!("{}. {}", i + 1, candidate.title()))
            }));
        let footer = div()
            .flex_none().flex().items_center().gap(px(10.)).px(px(24.)).py(px(12.))
            .border_t_1().border_color(c(theme::BORDER_SUBTLE))
            .when_some(step.previous(), |d, previous| {
                d.child(ui::button("setup-back", Variant::Secondary)
                    .child("Back")
                    .child(crate::trace::target("setup-back".to_owned()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.setup_page(previous);
                        cx.notify();
                    })))
            })
            .child(div().flex_1())
            .child(ui::button("setup-next", Variant::Primary)
                .child(if step.next().is_some() { "Next" } else if crate::TESTING { "Open demo library" } else { "Open Recall" })
                .child(crate::trace::target("setup-next".to_owned()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(next) = step.next() {
                        this.setup_page(next);
                    } else {
                        this.finish_setup();
                    }
                    cx.notify();
                })));
        div().size_full().flex().pr(px(8.)).pb(px(8.))
            .child(rail)
            .child(ui::panel().flex_1().min_w_0().min_h_0().flex().flex_col()
                .child(div().flex_1().min_h_0().child(page))
                .child(footer))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recommendation_uses_supported_recording_bitrates() {
        for (height, bitrate) in [(0, 25), (1080, 15), (1440, 25), (2160, 40)] {
            assert_eq!(recommended_bitrate(height), bitrate);
            assert!(crate::views::settings_capture::BITRATES.contains(&bitrate));
        }
    }

    #[test]
    fn setup_runs_storage_audio_quality_then_exits() {
        assert_eq!(SetupStep::Storage.next(), Some(SetupStep::Audio));
        assert_eq!(SetupStep::Audio.previous(), Some(SetupStep::Storage));
        assert_eq!(SetupStep::Audio.next(), Some(SetupStep::Quality));
        assert_eq!(SetupStep::Quality.next(), None);
    }
}
