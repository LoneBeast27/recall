//! The (i) beside Settings → Clip → Bitrate and Encoder effort. It opens a comparison: a
//! zoomed crop of one frame of a panning 1440p60 scene, as the original and as recorded with
//! the engine's NVENC settings. There is one recording per format × bitrate × effort, so each
//! tab shows exactly the setting it names, combined with the other two as they are set now.
//! The crops come from `tools/quality-previews/make-previews.ps1` and are embedded, so nothing
//! is read at start-up; each is decoded the first time it is shown.
use std::sync::{Arc, OnceLock};

use gpui::{AnyElement, Context, Corner, FontWeight, Image, ImageFormat, MouseButton, MouseDownEvent, ObjectFit, SharedString, anchored, deferred, div, img, prelude::*, px};

use crate::{
    root::Root,
    theme::{self, c, ca},
    ui::{self, Icon, icon},
    views::settings_capture::{BITRATES, CODECS, EFFORT},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preview {
    Bitrate,
    Effort,
}

/// A recording's settings, as indices into `settings_capture::CODECS`, `BITRATES` and `EFFORT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recipe {
    pub codec: usize,
    pub bitrate: usize,
    pub effort: usize,
}

impl Recipe {
    /// The settings as Settings holds them. A bitrate that is not one of the choices shows as
    /// the nearest one; an unknown format or effort as H.264 / Balanced.
    pub(crate) fn current(codec: &str, bitrate_mbps: u64, effort: &str) -> Recipe {
        Recipe {
            codec: CODECS.iter().position(|(v, _)| *v == codec).unwrap_or(0),
            bitrate: nearest_bitrate(bitrate_mbps),
            effort: EFFORT.iter().position(|(v, _)| *v == effort).unwrap_or(1),
        }
    }

    /// This recipe with `kind`'s setting changed to option `i`.
    fn with(self, kind: Preview, i: usize) -> Recipe {
        match kind {
            Preview::Bitrate => Recipe { bitrate: i, ..self },
            Preview::Effort => Recipe { effort: i, ..self },
        }
    }

    fn option(self, kind: Preview) -> usize {
        match kind {
            Preview::Bitrate => self.bitrate,
            Preview::Effort => self.effort,
        }
    }

    fn label(self) -> String {
        format!("{} · {} Mbps · {}", CODECS[self.codec].1, BITRATES[self.bitrate], EFFORT[self.effort].1)
    }
}

pub(crate) fn nearest_bitrate(mbps: u64) -> usize {
    (0..BITRATES.len()).min_by_key(|&i| BITRATES[i].abs_diff(mbps)).unwrap_or(1)
}

/// The open comparison: which (i), the settings it opened on, and the option tab it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviewState {
    pub kind: Preview,
    pub recipe: Recipe,
    pub shown: usize,
}

/// Logical px of each square tile. The crops are 136 px of the frame, pixel-doubled to 272 px in
/// the PNGs, so at 100% scaling they show 1:1 at 2x zoom with no smoothing.
const TILE: f32 = 272.;

macro_rules! png {
    ($name:literal) => {
        include_bytes!(concat!("../../assets/quality/", $name, ".png"))
    };
    ($codec:literal, $mbps:literal, $effort:literal) => {
        include_bytes!(concat!("../../assets/quality/", $codec, "-", $mbps, "-", $effort, ".png"))
    };
}
macro_rules! efforts {
    ($codec:literal, $mbps:literal) => {
        [png!($codec, $mbps, "performance"), png!($codec, $mbps, "balanced"), png!($codec, $mbps, "quality")]
    };
}
macro_rules! bitrates {
    ($codec:literal) => {
        [efforts!($codec, "15"), efforts!($codec, "25"), efforts!($codec, "40"), efforts!($codec, "60")]
    };
}

/// Every recorded crop, `[codec][bitrate][effort]` in the Settings orders. The asset checks
/// (`quality_preview_tests.rs`) hold that the names match those orders.
static SAMPLES: [[[&[u8]; 3]; 4]; 3] = [bitrates!("h264"), bitrates!("hevc"), bitrates!("av1")];
static ORIGINAL: &[u8] = png!("original");
/// The setup page's full frames: the original and H.264 at Balanced, per bitrate.
static FRAMES: [&[u8]; 4] = [png!("h264-15-balanced-frame"), png!("h264-25-balanced-frame"), png!("h264-40-balanced-frame"), png!("h264-60-balanced-frame")];
static ORIGINAL_FRAME: &[u8] = png!("original-frame");

fn decode(cell: &'static OnceLock<Arc<Image>>, bytes: &'static [u8]) -> Arc<Image> {
    cell.get_or_init(|| Arc::new(Image::from_bytes(ImageFormat::Png, bytes.to_vec()))).clone()
}

fn sample(r: Recipe) -> Arc<Image> {
    static DECODED: [[[OnceLock<Arc<Image>>; 3]; 4]; 3] = [const { [const { [const { OnceLock::new() }; 3] }; 4] }; 3];
    decode(&DECODED[r.codec][r.bitrate][r.effort], SAMPLES[r.codec][r.bitrate][r.effort])
}

fn original() -> Arc<Image> {
    static DECODED: OnceLock<Arc<Image>> = OnceLock::new();
    decode(&DECODED, ORIGINAL)
}

fn full_frame(bitrate: Option<usize>) -> Arc<Image> {
    static DECODED: [OnceLock<Arc<Image>>; 5] = [const { OnceLock::new() }; 5];
    match bitrate {
        Some(i) => decode(&DECODED[i], FRAMES[i]),
        None => decode(&DECODED[4], ORIGINAL_FRAME),
    }
}

/// What each option costs, in the Settings orders.
const BITRATE_NOTES: [&str; 4] = [
    "About 110 MB per minute of clip.",
    "About 190 MB per minute of clip.",
    "About 300 MB per minute of clip.",
    "About 450 MB per minute of clip.",
];
const EFFORT_NOTES: [&str; 3] = [
    "NVENC preset P4. The lightest on the GPU.",
    "NVENC preset P5.",
    "NVENC preset P7 with a second pass and adaptive quantization. Uses a little more GPU time.",
];

impl Preview {
    fn key(self) -> &'static str {
        match self {
            Preview::Bitrate => "bitrate",
            Preview::Effort => "effort",
        }
    }

    fn labels(self) -> Vec<String> {
        match self {
            Preview::Bitrate => BITRATES.iter().map(|b| format!("{b} Mbps")).collect(),
            Preview::Effort => EFFORT.iter().map(|(_, l)| (*l).to_owned()).collect(),
        }
    }

    fn note(self, i: usize) -> &'static str {
        match self {
            Preview::Bitrate => BITRATE_NOTES[i],
            Preview::Effort => EFFORT_NOTES[i],
        }
    }

    fn title(self) -> &'static str {
        match self {
            Preview::Bitrate => "How bitrate looks",
            Preview::Effort => "How encoder effort looks",
        }
    }

    /// Names the settings held fixed: the ones Settings has now.
    fn caption(self, r: Recipe) -> String {
        let codec = CODECS[r.codec].1;
        match self {
            Preview::Bitrate => format!(
                "One frame of a fast camera turn at 1440p and 60 fps, zoomed in 2x. Each tab is its own recording in {codec} at {} effort, as you have them set now. Fast motion and fine detail like leaves and branches show the difference most.",
                EFFORT[r.effort].1
            ),
            Preview::Effort => format!(
                "One frame of a fast camera turn at 1440p and 60 fps, zoomed in 2x. Each tab is its own recording in {codec} at {} Mbps, as you have them set now. Effort shows most at low bitrates. GTX 10 and 16 series GPUs record every effort as Performance.",
                BITRATES[r.bitrate]
            ),
        }
    }
}

/// A real recorded frame comparison at the recommended settings (H.264, Balanced). It is an
/// example, independent of the current game.
pub(crate) fn setup_comparison(bitrate: u64, detail: bool) -> AnyElement {
    let shown = nearest_bitrate(bitrate);
    let (source, recorded) = if detail {
        (original(), sample(Recipe { codec: 0, bitrate: shown, effort: 1 }))
    } else {
        (full_frame(None), full_frame(Some(shown)))
    };
    let tile = |label: String, image: Arc<Image>| {
        div().flex().flex_col().flex_1().min_w_0().gap(px(4.))
            .child(img(image).w_full().h(px(150.)).rounded(px(6.)).object_fit(ObjectFit::Contain))
            .child(div().text_size(px(12.)).text_color(c(theme::MUTED)).child(label))
    };
    let scale = if detail { "Matching detail crops" } else { "Full frame, scaled to fit" };
    div().flex().flex_col().gap(px(8.))
        .child(div().flex().gap(px(12.)).max_w(px(560.))
            .child(tile("Source frame".into(), source))
            .child(tile(format!("{} Mbps video target", BITRATES[shown]), recorded)))
        .child(div().text_size(px(12.)).text_color(c(theme::MUTED)).child(
            format!("{scale} · fast camera pan · 1440p60 H.264 · Balanced effort. {}", BITRATE_NOTES[shown])
        ))
        .into_any_element()
}

impl Root {
    /// The (i) after a setting's title. `recipe` is what Settings holds now; the comparison
    /// opens on its option and keeps the other settings as they are.
    pub(crate) fn quality_info(&self, kind: Preview, recipe: Recipe, cx: &mut Context<Self>) -> AnyElement {
        let open = self.settings_ui.quality_preview.filter(|p| p.kind == kind);
        let id = SharedString::from(format!("quality-info-{}", kind.key()));
        div()
            .relative()
            .flex_none()
            .child(
                div()
                    .child(crate::trace::target(id.to_string()))
                    .id(id)
                    .size(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(10.))
                    .cursor_pointer()
                    .hover(|s| s.bg(c(theme::OVERLAY)))
                    .when(open.is_some(), |d| d.bg(c(theme::OVERLAY)))
                    .child(icon(Icon::Info, 14., if open.is_some() { theme::TEXT } else { theme::TERTIARY }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            // This (i)'s comparison was closed by its outside-press on this same press.
                            if this.settings_ui.quality_closed_at.take() == Some((kind, event.position)) {
                                return;
                            }
                            let ui = &mut this.settings_ui;
                            ui.quality_preview = match ui.quality_preview {
                                Some(p) if p.kind == kind => None,
                                _ => Some(PreviewState { kind, recipe, shown: recipe.option(kind) }),
                            };
                            cx.notify();
                        }),
                    ),
            )
            .when_some(open, |d, state| d.child(self.quality_popover(state, cx)))
            .into_any_element()
    }

    fn quality_popover(&self, state: PreviewState, cx: &mut Context<Self>) -> AnyElement {
        let labels = state.kind.labels();
        let shown = state.shown.min(labels.len() - 1);
        let mut tabs = ui::segment_track().w_full();
        for (i, label) in labels.into_iter().enumerate() {
            tabs = tabs.child(ui::segment((SharedString::from(format!("quality-tab-{}", state.kind.key())), i), label, i == shown).on_click(cx.listener(
                move |this, _, _, cx| {
                    if let Some(p) = this.settings_ui.quality_preview.as_mut() {
                        p.shown = i;
                    }
                    cx.notify();
                },
            )));
        }
        let tile = |label: String, image: Arc<Image>| {
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(img(image).size(px(TILE)).rounded(px(6.)).object_fit(ObjectFit::Fill))
                .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(c(theme::MUTED)).child(label))
        };
        let recorded = state.recipe.with(state.kind, shown);
        let panel = div()
            .id(SharedString::from(format!("quality-preview-{}", state.kind.key())))
            .occlude()
            .w(px(TILE * 2. + 12. + 32.))
            .p(px(16.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .rounded(px(10.))
            .bg(c(theme::FLOATING))
            .border_1()
            .border_color(c(theme::BORDER))
            .shadow(vec![gpui::BoxShadow { color: ca(0x000000, 0.5), offset: gpui::point(px(0.), px(8.)), blur_radius: px(24.), spread_radius: px(0.) }])
            .on_mouse_down_out(cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.settings_ui.quality_preview = None;
                this.settings_ui.quality_closed_at = Some((state.kind, event.position));
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(div().font_weight(FontWeight::MEDIUM).child(state.kind.title()))
                    .child(div().text_size(px(12.)).line_height(px(16.)).text_color(c(theme::TERTIARY)).child(state.kind.caption(state.recipe))),
            )
            .child(tabs)
            .child(div().flex().gap(px(12.)).child(tile("Original".into(), original())).child(tile(recorded.label(), sample(recorded))))
            .child(div().text_size(px(12.)).line_height(px(16.)).text_color(c(theme::MUTED)).child(state.kind.note(shown)));
        // Opens upward from the (i) so the row and its (i) stay visible (the rows sit low on the page).
        // No margin on the panel: anchored() measures its child without margins. Its anchor point is the
        // bottom-left of the 20 px (i) (measured in the offscreen check), so lift it 20 + 8 px.
        deferred(anchored().anchor(Corner::BottomLeft).offset(gpui::point(px(-8.), px(-28.))).snap_to_window_with_margin(px(8.)).child(panel))
            .with_priority(1)
            .into_any_element()
    }
}

#[cfg(test)]
#[path = "quality_preview_tests.rs"]
mod tests;
