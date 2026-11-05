//! Settings → Clip (video format, frame rate, bitrate, encoder effort, a link to the Audio
//! page's sources) and
//! Settings → Media (storage limit). Values are the controller's config keys; a change
//! restarts the recorder (after any save or long recording in flight).
use gpui::{AnyElement, Context, SharedString, div, prelude::*, px};
use serde_json::{Value, json};

use crate::{
    root::Root,
    services::library::format_bytes,
    theme::{self, c},
    ui,
    views::pages::{section_title, setting_row},
};

const CODECS: [(&str, &str); 3] = [("h264", "H.264"), ("hevc", "HEVC"), ("av1", "AV1")];
const RATES: [u64; 4] = [30, 60, 120, 144];
const BITRATES: [u64; 4] = [15, 25, 40, 60];
const EFFORT: [(&str, &str); 3] = [("performance", "Performance"), ("balanced", "Balanced"), ("quality", "Quality")];
use crate::views::storage_limit::{self, CAPS};

impl Root {
    fn segments<T: Copy + PartialEq + 'static>(
        &self,
        id: &str,
        width: f32,
        options: &[(T, String)],
        current: T,
        section: &'static str,
        key: &'static str,
        to_json: fn(T) -> Value,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut track = ui::segment_track().w(px(width));
        for (i, (value, label)) in options.iter().enumerate() {
            let value = *value;
            track = track.child(ui::segment((SharedString::from(format!("{id}-{i}")), i), label.clone(), value == current).on_click(cx.listener(move |this, _, _, cx| {
                this.set_config(section, key, to_json(value));
                cx.notify();
            })));
        }
        track.into_any_element()
    }

    pub fn render_video_section(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let text = |this: &Self, key: &str, default: &str| this.config_value("recording", key).and_then(Value::as_str).unwrap_or(default).to_owned();
        let number = |this: &Self, key: &str, default: u64| this.config_value("recording", key).and_then(Value::as_u64).unwrap_or(default);
        let codec = text(self, "codec", "h264");
        let fps = number(self, "fps", 60);
        let bitrate = number(self, "bitrate_mbps", 25);
        let effort = text(self, "quality", "balanced");
        let pending = self.snap.session.as_ref().is_some_and(|s| s.pending_restart);
        // The engine falls back to H.264 when the GPU cannot encode the chosen format.
        let running = self.snap.engine.as_ref().map(|e| e.codec.clone()).filter(|c| !c.is_empty() && self.snap.replay_ready());
        let fallback = running.as_deref().is_some_and(|r| r != codec);

        let codec_index = CODECS.iter().position(|(v, _)| *v == codec).unwrap_or(0);
        let codecs: Vec<(usize, String)> = CODECS.iter().enumerate().map(|(i, (_, l))| (i, (*l).to_owned())).collect();
        let rates: Vec<(u64, String)> = RATES.iter().map(|r| (*r, format!("{r}"))).collect();
        let bitrates: Vec<(u64, String)> = BITRATES.iter().map(|b| (*b, format!("{b} Mbps"))).collect();
        let effort_index = EFFORT.iter().position(|(v, _)| *v == effort).unwrap_or(1);
        let efforts: Vec<(usize, String)> = EFFORT.iter().enumerate().map(|(i, (_, l))| (i, (*l).to_owned())).collect();

        let format_detail = if fallback {
            format!("This GPU can't record {}; recording {} instead.", CODECS[codec_index].1, running.as_deref().unwrap_or("H.264").to_uppercase().replace("H264", "H.264"))
        } else {
            "H.264 plays everywhere. HEVC and AV1 make smaller files.".to_owned()
        };
        let mut items = vec![
            section_title("Video"),
            setting_row("Format", &format_detail)
                .child(self.segments("codec", 240., &codecs, codec_index, "recording", "codec", |i| json!(CODECS[i].0), cx))
                .into_any_element(),
            setting_row("Frame rate", "Frames per second in your clips.")
                .child(self.segments("fps", 240., &rates, fps, "recording", "fps", |v| json!(v), cx))
                .into_any_element(),
            setting_row("Bitrate", "Higher looks sharper and makes bigger files.")
                .child(self.segments("bitrate", 320., &bitrates, bitrate, "recording", "bitrate_mbps", |v| json!(v), cx))
                .into_any_element(),
            setting_row("Encoder effort", "Quality uses a little more GPU time.")
                .child(self.segments("effort", 300., &efforts, effort_index, "recording", "quality", |i| json!(EFFORT[i].0), cx))
                .into_any_element(),
        ];
        if pending {
            items.push(div().text_size(px(12.)).text_color(c(theme::WARNING)).child("Changes apply when the current save or recording finishes.").into_any_element());
        }
        items.push(section_title("Audio"));
        let count = self.audio_sources().iter().filter(|s| s.enabled).count();
        let detail = format!("{count} recorded now. Microphones, desktop audio, apps and the game are set on the Audio page, each as its own track.");
        items.push(
            setting_row("Audio sources", &detail)
                .child(ui::button("open-audio-page", crate::ui::Variant::Secondary).child("Open Audio page").on_click(cx.listener(|this, _, _, cx| {
                    this.go(crate::root::Route::Audio);
                    cx.notify();
                })))
                .into_any_element(),
        );
        items
    }

    pub fn render_storage_limit(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let cap = self.config_value("storage", "cap_gb").and_then(Value::as_u64).unwrap_or(0);
        let recordings = self.recordings_folder();
        let used: u64 = self.lib.records.iter().filter(|r| crate::views::folders_settings::inside(&r.path, &recordings)).map(|r| r.bytes).sum();
        let share = if cap > 0 { format!(" ({}% of {})", used.saturating_mul(100) / (cap << 30), storage_limit::cap_label(cap << 30)) } else { String::new() };
        let detail = format!(
            "Recordings use {}{share}. Past the limit, the oldest go to the Recycle Bin. You're told at 80% and 90%. Favourites and edited clips stay.",
            format_bytes(used)
        );
        let mut track = ui::segment_track().w(px(400.));
        for (i, (value, label)) in CAPS.iter().enumerate() {
            let value = *value;
            let picked = self.storage_confirm.as_ref().map(|p| u64::from(p.cap_gb)).or(self.storage_asking.map(u64::from)).unwrap_or(cap);
            track = track.child(ui::segment((SharedString::from(format!("cap-{i}")), i), (*label).to_owned(), value == picked).on_click(cx.listener(move |this, _, _, cx| {
                this.pick_storage_cap(value, cap);
                cx.notify();
            })));
        }
        let mut items = vec![section_title("Storage limit"), setting_row("Keep recordings under", &detail).child(track).into_any_element()];
        if let Some(asking) = self.storage_asking {
            let text = format!("Checking what {} would move…", storage_limit::cap_label(u64::from(asking) << 30));
            items.push(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(text).into_any_element());
        }
        if let Some(preview) = self.storage_confirm.clone() {
            let (question, detail, action) = storage_limit::confirm(&preview, crate::services::library::today());
            let cap_gb = preview.cap_gb;
            items.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .p(px(12.))
                    .rounded(px(8.))
                    .bg(c(theme::OVERLAY))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(question))
                            .child(div().text_size(px(12.)).line_height(px(16.)).text_color(c(theme::TERTIARY)).child(detail)),
                    )
                    .child(ui::button("cap-keep", ui::Variant::Ghost).child("Keep current").on_click(cx.listener(|this, _, _, cx| {
                        this.storage_confirm = None;
                        cx.notify();
                    })))
                    .child(ui::button("cap-confirm", ui::Variant::Primary).child(action).on_click(cx.listener(move |this, _, _, cx| {
                        this.storage_confirm = None;
                        this.set_config("storage", "cap_gb", json!(cap_gb));
                        cx.notify();
                    })))
                    .into_any_element(),
            );
        }
        items
    }

    /// A limit that would move clips now asks first (the recorder's dry run names how many and
    /// from which date); no limit, or one already met, applies at once.
    fn pick_storage_cap(&mut self, value: u64, current: u64) {
        self.storage_confirm = None;
        self.storage_asking = None;
        if value == current {
            return;
        }
        if value == 0 {
            self.set_config("storage", "cap_gb", json!(0));
            return;
        }
        let cap_gb = value.min(u64::from(u32::MAX)) as u32;
        self.storage_asking = Some(cap_gb);
        if !self.daemon.send(crate::services::daemon::DaemonCommand::StoragePreview(cap_gb)) {
            self.storage_asking = None;
            self.show_toast(crate::root::ToastKind::Bad, "Recorder is off", Some("Start it from the status pill".into()));
        }
    }
}
