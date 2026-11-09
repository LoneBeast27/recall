//! The (i) beside Settings → Clip → Bitrate and Encoder effort. It opens a comparison: a
//! zoomed crop of one frame of a panning 1440p60 scene, as the original and as recorded at
//! each option with the engine's NVENC settings (H.264). The crops come from
//! `tools/quality-previews/make-previews.ps1` and are embedded, so nothing is read at
//! start-up; they are decoded the first time a comparison opens.
use std::sync::{Arc, OnceLock};

use gpui::{AnyElement, Context, Corner, FontWeight, Image, ImageFormat, MouseButton, MouseDownEvent, ObjectFit, SharedString, anchored, deferred, div, img, prelude::*, px};

use crate::{
    root::Root,
    theme::{self, c, ca},
    ui::{self, Icon, icon},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preview {
    Bitrate,
    Effort,
}

/// The open comparison and the option tab it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviewState {
    pub kind: Preview,
    pub shown: usize,
}

/// Logical px of each square tile. The crops are 136 px of the frame, pixel-doubled to 272 px in
/// the PNGs, so at 100% scaling they show 1:1 at 2x zoom with no smoothing.
const TILE: f32 = 272.;

macro_rules! crop {
    ($name:literal) => {
        include_bytes!(concat!("../../assets/quality/", $name, ".png"))
    };
}

/// Every crop, decoded once on first use. 0 is the original.
fn images() -> &'static [Arc<Image>; 7] {
    static IMAGES: OnceLock<[Arc<Image>; 7]> = OnceLock::new();
    IMAGES.get_or_init(|| {
        let bytes: [&'static [u8]; 7] = [
            crop!("original"),
            crop!("bitrate-15"),
            crop!("bitrate-25"),
            crop!("bitrate-40"),
            crop!("bitrate-60"),
            crop!("effort-performance"),
            crop!("effort-quality"),
        ];
        bytes.map(|b| Arc::new(Image::from_bytes(ImageFormat::Png, b.to_vec())))
    })
}

/// One tab: its label, its crop in [`images`], and a line on what it costs.
struct Choice {
    label: &'static str,
    image: usize,
    note: &'static str,
}

/// In the order of the Settings segment controls (`settings_capture::BITRATES` / `EFFORT`).
const BITRATE: [Choice; 4] = [
    Choice { label: "15 Mbps", image: 1, note: "About 110 MB per minute of clip." },
    Choice { label: "25 Mbps", image: 2, note: "About 190 MB per minute of clip." },
    Choice { label: "40 Mbps", image: 3, note: "About 300 MB per minute of clip." },
    Choice { label: "60 Mbps", image: 4, note: "About 450 MB per minute of clip." },
];
/// Balanced at 15 Mbps is the same recording as the 15 Mbps bitrate crop.
const EFFORT: [Choice; 3] = [
    Choice { label: "Performance", image: 5, note: "NVENC preset P4. The lightest on the GPU." },
    Choice { label: "Balanced", image: 1, note: "NVENC preset P5." },
    Choice { label: "Quality", image: 6, note: "NVENC preset P7 with a second pass and adaptive quantization. Uses a little more GPU time." },
];

impl Preview {
    fn key(self) -> &'static str {
        match self {
            Preview::Bitrate => "bitrate",
            Preview::Effort => "effort",
        }
    }

    fn choices(self) -> &'static [Choice] {
        match self {
            Preview::Bitrate => &BITRATE,
            Preview::Effort => &EFFORT,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Preview::Bitrate => "How bitrate looks",
            Preview::Effort => "How encoder effort looks",
        }
    }

    fn caption(self) -> &'static str {
        match self {
            Preview::Bitrate => "One frame of a fast camera turn at 1440p and 60 fps, recorded in H.264 at Balanced effort and zoomed in 2x. Fast motion and fine detail like leaves and branches show the difference most. HEVC and AV1 look sharper at the same bitrate.",
            Preview::Effort => "One frame of a fast camera turn at 1440p and 60 fps, recorded in H.264 at 15 Mbps, where effort shows the most, and zoomed in 2x. GTX 10 and 16 series GPUs record every effort as Performance.",
        }
    }
}

/// The same frame as the detail crops, downscaled uniformly from 1440p.
fn full_frames() -> &'static [Arc<Image>; 5] {
    static FRAMES: OnceLock<[Arc<Image>; 5]> = OnceLock::new();
    FRAMES.get_or_init(|| {
        let bytes: [&'static [u8]; 5] = [
            crop!("original-frame"),
            crop!("bitrate-15-frame"),
            crop!("bitrate-25-frame"),
            crop!("bitrate-40-frame"),
            crop!("bitrate-60-frame"),
        ];
        bytes.map(|b| Arc::new(Image::from_bytes(ImageFormat::Png, b.to_vec())))
    })
}

/// A real recorded frame comparison. It is an example, independent of the current game.
pub(crate) fn setup_comparison(bitrate: u64, detail: bool) -> AnyElement {
    let shown = crate::views::settings_capture::BITRATES.iter().position(|&b| b == bitrate).unwrap_or(1);
    let choice = &BITRATE[shown];
    let frames = if detail { images().as_slice() } else { full_frames().as_slice() };
    let tile = |label: String, image: &Arc<Image>| {
        div().flex().flex_col().flex_1().min_w_0().gap(px(4.))
            .child(img(image.clone()).w_full().h(px(150.)).rounded(px(6.)).object_fit(ObjectFit::Contain))
            .child(div().text_size(px(12.)).text_color(c(theme::MUTED)).child(label))
    };
    let scale = if detail { "Matching detail crops" } else { "Full frame, scaled to fit" };
    div().flex().flex_col().gap(px(8.))
        .child(div().flex().gap(px(12.)).max_w(px(560.))
            .child(tile("Source frame".into(), &frames[0]))
            .child(tile(format!("{} video target", choice.label), &frames[choice.image])))
        .child(div().text_size(px(12.)).text_color(c(theme::MUTED)).child(
            format!("{scale} · fast camera pan · 1440p60 H.264 · Balanced effort. {}", choice.note)
        ))
        .into_any_element()
}

impl Root {
    /// The (i) after a setting's title. `current` is the option the setting holds now; the
    /// comparison opens on it.
    pub(crate) fn quality_info(&self, kind: Preview, current: usize, cx: &mut Context<Self>) -> AnyElement {
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
                                _ => Some(PreviewState { kind, shown: current }),
                            };
                            cx.notify();
                        }),
                    ),
            )
            .when_some(open, |d, state| d.child(self.quality_popover(state, cx)))
            .into_any_element()
    }

    fn quality_popover(&self, state: PreviewState, cx: &mut Context<Self>) -> AnyElement {
        let choices = state.kind.choices();
        let shown = state.shown.min(choices.len() - 1);
        let images = images();
        let mut tabs = ui::segment_track().w_full();
        for (i, choice) in choices.iter().enumerate() {
            tabs = tabs.child(ui::segment((SharedString::from(format!("quality-tab-{}", state.kind.key())), i), choice.label, i == shown).on_click(cx.listener(
                move |this, _, _, cx| {
                    if let Some(p) = this.settings_ui.quality_preview.as_mut() {
                        p.shown = i;
                    }
                    cx.notify();
                },
            )));
        }
        let tile = |label: &'static str, image: &Arc<Image>| {
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(img(image.clone()).size(px(TILE)).rounded(px(6.)).object_fit(ObjectFit::Fill))
                .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(c(theme::MUTED)).child(label))
        };
        let choice = &choices[shown];
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
                    .child(div().text_size(px(12.)).line_height(px(16.)).text_color(c(theme::TERTIARY)).child(state.kind.caption())),
            )
            .child(tabs)
            .child(div().flex().gap(px(12.)).child(tile("Original", &images[0])).child(tile(choice.label, &images[choice.image])))
            .child(div().text_size(px(12.)).line_height(px(16.)).text_color(c(theme::MUTED)).child(choice.note));
        // Opens upward from the (i) so the row and its (i) stay visible (the rows sit low on the page).
        // No margin on the panel: anchored() measures its child without margins. Its anchor point is the
        // bottom-left of the 20 px (i) (measured in the offscreen check), so lift it 20 + 8 px.
        deferred(anchored().anchor(Corner::BottomLeft).offset(gpui::point(px(-8.), px(-28.))).snap_to_window_with_margin(px(8.)).child(panel))
            .with_priority(1)
            .into_any_element()
    }
}
