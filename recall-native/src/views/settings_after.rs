//! Settings → Recording → After recording: the editor's export defaults (`[export]`), shown
//! under the capture settings that cap them (`export_defaults`). A choice above the recording
//! frame rate is greyed out; a saved default the recording now holds down says so and is kept.
use gpui::{AnyElement, Context, SharedString, div, prelude::*, px};
use serde_json::json;

use crate::{
    export_defaults::ExportDefaults,
    root::Root,
    theme::{self, c},
    ui,
    views::pages::{section_title, setting_row},
};

/// Export rates offered as defaults (0 = the clip's recorded rate).
const FPS: [(u32, &str); 6] = [(0, "Match"), (30, "30"), (60, "60"), (120, "120"), (144, "144"), (240, "240")];
const HEIGHTS: [(u32, &str); 3] = [(0, "Same as clip"), (1080, "1080p"), (720, "720p")];
const SIZES: [(u32, &str); 4] = [(0, "No limit"), (10, "10 MB"), (50, "50 MB"), (500, "500 MB")];

impl Root {
    pub(crate) fn render_after_recording_section(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let d = ExportDefaults::from_config(self.snap.config.as_ref());
        let rec = d.recording_fps;
        // The rate in effect is marked: a saved 120 under 60 fps recording shows 60.
        let fps_track = self.export_segments("export-fps", 330., &FPS, d.capped_fps(), "fps", |v| rec != 0 && v > rec, cx);
        let modes = [(0u32, "Re-encode"), (1, "Lossless trim")];
        let mode_track = self.export_segments("export-mode", 240., &modes, u32::from(d.lossless), "mode", |_| false, cx);
        let lossless = d.lossless;
        // A height above the recording's own (Settings, Recording, Resolution) has nothing to export.
        let recorded_height = self.config_value("recording", "height").and_then(serde_json::Value::as_u64).unwrap_or(0) as u32;
        // A saved height at or above the recording's is kept, but each clip then exports at its own size.
        let height_in_effect = if recorded_height != 0 && d.height >= recorded_height { 0 } else { d.height };
        let height_track = self.export_segments("export-height", 240., &HEIGHTS, height_in_effect, "height", move |v| lossless || (recorded_height != 0 && v > recorded_height), cx);
        let size_track = self.export_segments("export-size", 320., &SIZES, d.max_mb, "max_mb", move |_| lossless, cx);

        let fps_detail = if rec == 0 {
            "Match exports each clip at the rate it was recorded at.".to_owned()
        } else {
            format!("Never above what you record ({rec} fps) or what a clip was recorded at. Match uses the clip's own rate.")
        };
        // Same order and names as the editor's export page: the everyday three, then Advanced.
        let mut items = vec![
            // Title and its description sit together, like a row's title and detail.
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(section_title("After recording"))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(c(theme::TERTIARY))
                        .child("Where the editor starts each clip's export. A recording holds nothing above these, so they stay at or below it."),
                )
                .into_any_element(),
            setting_row("Export frame rate", &fps_detail).child(fps_track).into_any_element(),
        ];
        // The 60 fps default under a lower recording rate is not a choice the person made: the
        // highlighted rate in effect says enough. A rate they picked gets the note.
        if d.fps_capped_by_recording() && d.fps != crate::export_defaults::FPS_DEFAULT {
            let line = format!("Saved as {saved} fps. Exports use {rec} fps until you record at {saved} fps or more.", saved = d.fps);
            items.push(div().text_size(px(12.)).text_color(c(theme::WARNING)).child(line).into_any_element());
        }
        let size_note = if lossless { "Re-encode only." } else { "Never above a clip's own size; a smaller clip keeps its size." };
        items.push(setting_row("Export resolution", size_note).child(height_track).into_any_element());
        let cap_note = if lossless { "Re-encode only." } else { "The editor fits the export under this size." };
        items.push(setting_row("Export size limit", cap_note).child(size_track).into_any_element());
        // Set once and left: the export method. The closed header names it when it is not the default.
        let open = self.after_advanced;
        items.push(
            ui::disclosure("after-advanced", "Advanced", if lossless { "Lossless trim" } else { "" }, open)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.after_advanced = !this.after_advanced;
                    cx.notify();
                }))
                .into_any_element(),
        );
        if open {
            items.push(setting_row("Export method", "Lossless trim only cuts: it keeps the recording's frame rate, size and quality.").child(mode_track).into_any_element());
        }
        items
    }
    /// A segment track writing `export.<key>`; `off(value)` greys a choice out (not clickable).
    fn export_segments(
        &self,
        id: &'static str,
        width: f32,
        options: &[(u32, &'static str)],
        current: u32,
        key: &'static str,
        off: impl Fn(u32) -> bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut track = ui::segment_track().w(px(width));
        for (i, (value, label)) in options.iter().copied().enumerate() {
            let disabled = off(value);
            let segment = ui::segment((SharedString::from(format!("{id}-{i}")), i), label, value == current && !disabled);
            track = track.child(if disabled {
                segment.opacity(0.35)
            } else {
                segment.on_click(cx.listener(move |this, _, _, cx| {
                    let json = if key == "mode" { json!(if value == 1 { "lossless" } else { "reencode" }) } else { json!(value) };
                    this.set_config("export", key, json);
                    cx.notify();
                }))
            });
        }
        track.into_any_element()
    }
}
