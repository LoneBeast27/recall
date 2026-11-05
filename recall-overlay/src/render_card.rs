//! The shortcut hint's card under the pill: the same body and hairline as
//! the pill (rounded, not round), revealed from the top as it unfolds; each
//! option a key cap in the track tone with its key, then the label.

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{ID2D1StrokeStyle, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ROUNDED_RECT};
use windows_numerics::{Matrix3x2, Vector2};

use super::Renderer;
use crate::fonts::Fonts;
use crate::hint::CardScene;
use crate::layout::{EDGE_LINE, PILL, TRACK};
use crate::presence::Look;

fn rounded(left: f32, top: f32, w: f32, h: f32, r: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left, top, right: left + w, bottom: top + h }, radiusX: r, radiusY: r }
}

impl Renderer {
    /// `card` at `(x, y)` in the window, as far unfolded as `look.card` says.
    pub(crate) unsafe fn card(&self, fonts: &Fonts, card: &CardScene, look: &Look, (x, y): (f32, f32), scale: f32) -> Result<()> {
        let Some(shown) = look.card else { return Ok(()) };
        let t = &self.target;
        // Unfolds from the pill: from its top edge down, or (`up`) from its bottom edge up.
        let reveal = match look.up {
            false => D2D_RECT_F { left: x, top: y, right: x + card.w, bottom: y + card.h * shown.progress },
            true => D2D_RECT_F { left: x, top: y + card.h * (1.0 - shown.progress), right: x + card.w, bottom: y + card.h },
        };
        unsafe {
            t.PushAxisAlignedClip(&reveal, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            self.paint(PILL, 1.0);
            t.FillRoundedRectangle(&rounded(x, y, card.w, card.h, card.radius), &self.brush);
            self.paint(EDGE_LINE, 1.0);
            let edge = rounded(x + 0.5, y + 0.5, card.w - 1.0, card.h - 1.0, (card.radius - 0.5).max(0.0));
            t.DrawRoundedRectangle(&edge, &self.brush, 1.0, None::<&ID2D1StrokeStyle>);
            self.paint(TRACK, 1.0);
            for key in &card.keys {
                t.FillRoundedRectangle(&rounded(x + key.x, y + key.y, key.size, key.size, key.r), &self.brush);
            }
            self.set_transform(Matrix3x2::translation(x, y));
            for text in &card.texts {
                let layout = fonts.layout(text.role, scale, &text.text)?;
                self.paint(text.color, 1.0);
                t.DrawTextLayout(Vector2 { X: text.x, Y: text.y }, &layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
            self.set_transform(Matrix3x2::identity());
            t.PopAxisAlignedClip();
        }
        Ok(())
    }
}
