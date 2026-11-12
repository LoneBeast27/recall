//! Drawing the editor's panel (`panel::PanelScene`) with the guide's
//! software Direct2D renderer: a DIB of exactly the panel's size.

use windows::core::Result;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ROUNDED_RECT};
use windows_numerics::{Matrix3x2, Vector2};

use crate::fonts::Fonts;
use crate::guide_render::{color, GuideRenderer};
use crate::panel::{Action, PanelScene, Text, Tone, HOVER, OUTLINE, PANEL, PRESSED};

fn rounded([l, t, r, b]: [f32; 4], radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: l, top: t, right: r, bottom: b }, radiusX: radius, radiusY: radius }
}

impl GuideRenderer {
    /// Draws the whole panel (it is small: no partial redraws).
    /// `hot`: the button under the pointer, lighter (darker while pressed).
    pub fn draw_panel(&mut self, fonts: &Fonts, scene: &PanelScene, hot: Option<(Action, bool)>) -> Result<()> {
        self.ensure(scene.w, scene.h)?;
        let t = &self.target;
        let (w, h) = (scene.w as f32, scene.h as f32);
        let text = |label: &Text| -> Result<()> {
            let layout = fonts.layout(label.role, scene.scale, &label.text)?;
            unsafe {
                self.brush.SetColor(&color(label.color));
                t.DrawTextLayout(Vector2 { X: label.x, Y: label.y }, &layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
            Ok(())
        };
        unsafe {
            t.BindDC(self.dc, &RECT { left: 0, top: 0, right: scene.w, bottom: scene.h })?;
            t.BeginDraw();
            t.SetTransform(&Matrix3x2::identity());
            t.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            t.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            self.brush.SetColor(&color(PANEL));
            t.FillRoundedRectangle(&rounded([0.0, 0.0, w, h], scene.radius), &self.brush);
            self.brush.SetColor(&color(OUTLINE));
            t.DrawRoundedRectangle(&rounded([0.5, 0.5, w - 0.5, h - 0.5], scene.radius), &self.brush, 1.0, None);
        }
        for label in &scene.texts {
            text(label)?;
        }
        for button in &scene.buttons {
            unsafe {
                self.brush.SetColor(&color(button.tone.colors().0));
                t.FillRoundedRectangle(&rounded(button.rect, scene.button_radius), &self.brush);
                if let Some(pressed) = hot.filter(|(a, _)| *a == button.action && button.tone != Tone::Disabled).map(|(_, p)| p) {
                    self.brush.SetColor(&color(if pressed { PRESSED } else { HOVER }));
                    t.FillRoundedRectangle(&rounded(button.rect, scene.button_radius), &self.brush);
                }
            }
            text(&button.label)?;
        }
        unsafe { t.EndDraw(None, None) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel::{scene, PanelState};
    use crate::spot_edit::Edge;

    /// The real fonts and Direct2D draw the panel: its plate fills the
    /// corners' inside, the outside stays clear, and the text shows. With
    /// `RECALL_PANEL_PNG` set to a folder, each scale is written there as a
    /// PNG over a mid grey (a look check; nothing is shown on screen).
    #[test]
    fn the_panel_draws_its_plate_and_text_at_every_scale() {
        let fonts = Fonts::new().expect("DirectWrite in-memory fonts");
        let mut renderer = GuideRenderer::new().expect("Direct2D");
        for k in [0.75f32, 1.0, 1.5] {
            let state = PanelState { game: "valorant", size: 1.25, edge: Some(Edge::Top) };
            let scene = scene(&state, k, &fonts);
            renderer.draw_panel(&fonts, &scene, None).expect("draw");
            let px = renderer.pixels();
            let at = |x: i32, y: i32| px[(y * scene.w + x) as usize];
            assert_eq!(at(0, 0)[3], 0, "k {k}: the rounded corner is clear");
            assert!(at(scene.w / 2, 2)[3] > 200, "k {k}: the plate is drawn");
            let bright = px.iter().filter(|p| p[0] > 180 && p[3] > 200).count();
            assert!(bright > 200, "k {k}: text and the Done button show ({bright} bright px)");
            // The button under the pointer is lighter, and darker while pressed.
            let done = scene.buttons.iter().find(|b| b.action == Action::Done).unwrap();
            let (cx, cy) = (done.rect[0] as i32 + 3, ((done.rect[1] + done.rect[3]) / 2.0) as i32);
            let luma = |r: &mut GuideRenderer, hot| {
                r.draw_panel(&fonts, &scene, hot).unwrap();
                let p = r.pixels()[(cy * scene.w + cx) as usize];
                p[0] as u32 + p[1] as u32 + p[2] as u32
            };
            let (plain, hover, pressed) = (luma(&mut renderer, None), luma(&mut renderer, Some((Action::Done, false))), luma(&mut renderer, Some((Action::Done, true))));
            assert!(hover > plain && pressed < plain, "k {k}: hover {hover} > plain {plain} > pressed {pressed}");
            renderer.draw_panel(&fonts, &scene, Some((Action::Place(Edge::Bottom), false))).unwrap();
            let px = renderer.pixels();
            if let Some(dir) = std::env::var_os("RECALL_PANEL_PNG") {
                let rgba: Vec<u8> = px
                    .iter()
                    .flat_map(|&[b, g, r, a]| {
                        let under = 0x55 as f32 * (1.0 - a as f32 / 255.0);
                        [r as f32 + under, g as f32 + under, b as f32 + under].map(|c| c.min(255.0) as u8).into_iter().chain([255])
                    })
                    .collect();
                let path = std::path::Path::new(&dir).join(format!("island-panel-{}.png", (k * 100.0) as i32));
                std::fs::write(path, crate::png::encode(scene.w as u32, scene.h as u32, &rgba)).unwrap();
            }
        }
    }
}
