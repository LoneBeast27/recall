//! How a captured frame lands in the recorded one.
//!
//! A windowed game (a window capture whose client area is smaller than the captured frame:
//! it has a title bar or borders) is cropped to its client area and placed on a fixed
//! [`CANVAS`] (2560x1440, black): centred, never upscaled, scaled down (aspect kept) only
//! when the client area is larger than the canvas. The canvas keeps its size for the whole
//! capture session, so resizing the window never reopens NVENC. Everything else (monitor
//! captures, borderless and fullscreen windows) records at its own size as before, with the
//! border-jitter crop / pad of `gpu_frames::target_size`.

/// The recording canvas for windowed games ("1440p standard").
pub const CANVAS: (u32, u32) = (2560, 1440);

/// A pixel rectangle: origin and size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    /// The whole of a `size` frame.
    pub const fn full(size: (u32, u32)) -> Self {
        Self { x: 0, y: 0, w: size.0, h: size.1 }
    }

    pub const fn size(&self) -> (u32, u32) {
        (self.w, self.h)
    }

    /// The part of `self` inside a `size` frame; None when nothing is left.
    pub fn clamp_to(self, size: (u32, u32)) -> Option<Self> {
        let (x1, y1) = (self.x.saturating_add(self.w).min(size.0), self.y.saturating_add(self.h).min(size.1));
        (self.x < x1 && self.y < y1).then(|| Self { x: self.x, y: self.y, w: x1 - self.x, h: y1 - self.y })
    }
}

/// What one frame renders: the `src` texels of the capture into `dst` of an `out`-sized
/// frame; the rest of `out` is black. `src` and `dst` of one size is a pixel-exact copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub src: Rect,
    pub dst: Rect,
    pub out: (u32, u32),
    /// On the windowed-game canvas (stills, thumbnails and screenshots take `dst` only).
    pub canvas: bool,
}

impl Layout {
    /// A `frame`-sized capture at its own size in an `out` frame: the top-left `out` of it,
    /// padded black where it is smaller (window-border jitter).
    pub fn native(frame: (u32, u32), out: (u32, u32)) -> Self {
        let r = Rect::full((frame.0.min(out.0), frame.1.min(out.1)));
        Self { src: r, dst: r, out, canvas: false }
    }

    /// `src` of the capture fitted onto `canvas` (see [`fit`]).
    pub fn on_canvas(src: Rect, canvas: (u32, u32)) -> Self {
        Self { src, dst: fit(src.size(), canvas), out: canvas, canvas: true }
    }

    /// The region of the output holding the picture, when bars surround it.
    pub fn content(&self) -> Option<Rect> {
        self.canvas.then_some(self.dst)
    }
}

/// Place `content` on `canvas`: unscaled when it fits, else scaled down to fit with its
/// aspect kept; centred, with even offsets so 4:2:0 chroma never straddles a bar's edge.
pub fn fit(content: (u32, u32), canvas: (u32, u32)) -> Rect {
    let (cw, ch) = (u64::from(content.0.max(1)), u64::from(content.1.max(1)));
    let (vw, vh) = (u64::from(canvas.0.max(1)), u64::from(canvas.1.max(1)));
    let (w, h) = if cw <= vw && ch <= vh {
        (cw, ch)
    } else if cw * vh >= ch * vw {
        // Relatively wider than the canvas: width-bound.
        (vw, ((ch * vw + cw / 2) / cw).clamp(1, vh))
    } else {
        (((cw * vh + ch / 2) / ch).clamp(1, vw), vh)
    };
    let (w, h) = (w as u32, h as u32);
    Rect { x: ((canvas.0.saturating_sub(w)) / 2) & !1, y: ((canvas.1.saturating_sub(h)) / 2) & !1, w, h }
}

/// Copy `rect` out of a row-major image with `stride` bytes per row and `bpp` bytes per
/// pixel, tightly packed. The caller keeps `rect` inside the image.
pub fn copy_rect(data: &[u8], stride: usize, bpp: usize, rect: Rect) -> Vec<u8> {
    let row = rect.w as usize * bpp;
    let mut out = Vec::with_capacity(row * rect.h as usize);
    for y in rect.y as usize..(rect.y + rect.h) as usize {
        let start = y * stride + rect.x as usize * bpp;
        out.extend_from_slice(&data[start..start + row]);
    }
    out
}

/// `rect` of a tightly packed RGBA8 frame (`width` px wide), as (w, h, rgba).
pub fn crop_rgba(width: u32, rgba: &[u8], rect: Rect) -> (u32, u32, Vec<u8>) {
    (rect.w, rect.h, copy_rect(rgba, width as usize * 4, 4, rect))
}

#[cfg(test)]
#[path = "framing_tests.rs"]
mod tests;
