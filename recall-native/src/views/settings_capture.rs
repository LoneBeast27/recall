//! Settings → Clip (video format, frame rate, bitrate, encoder effort, voice-chat track) and
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
/// (exe, label); "" turns the separate voice track off.
const VOICE_APPS: [(&str, &str); 4] = [("", "Off"), ("Discord.exe", "Discord"), ("TeamSpeak.exe", "TeamSpeak"), ("mumble.exe", "Mumble")];
/// GiB; 0 = no limit.
const CAPS: [(u64, &str); 5] = [(0, "No limit"), (250, "250 GB"), (500, "500 GB"), (1000, "1 TB"), (2000, "2 TB")];

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
        let voice = text(self, "voice_app", "");
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
        let voice_index = VOICE_APPS.iter().position(|(exe, _)| exe.eq_ignore_ascii_case(&voice)).unwrap_or(usize::MAX);
        let voices: Vec<(usize, String)> = VOICE_APPS.iter().enumerate().map(|(i, (_, l))| (i, (*l).to_owned())).collect();

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
        items.push(
            setting_row("Voice chat on its own track", "Keeps friends' voices separate, so you can turn them down in the editor.")
                .child(self.segments("voice", 340., &voices, voice_index, "recording", "voice_app", |i| json!(VOICE_APPS[i].0), cx))
                .into_any_element(),
        );
        items
    }

    pub fn render_storage_limit(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let cap = self.config_value("storage", "cap_gb").and_then(Value::as_u64).unwrap_or(0);
        let recordings = self.recordings_folder();
        let used: u64 = self.lib.records.iter().filter(|r| crate::views::folders_settings::inside(&r.path, &recordings)).map(|r| r.bytes).sum();
        let options: Vec<(u64, String)> = CAPS.iter().map(|(gb, l)| (*gb, (*l).to_owned())).collect();
        let detail = format!("Recordings use {}. Past the limit, the oldest go to the Recycle Bin. Favourites and edited clips stay.", format_bytes(used));
        vec![
            section_title("Storage limit"),
            setting_row("Keep recordings under", &detail).child(self.segments("cap", 400., &options, cap, "storage", "cap_gb", |v| json!(v), cx)).into_any_element(),
        ]
    }
}
