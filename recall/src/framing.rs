//! How a captured frame lands in the recorded one.
//!
//! A windowed game (a window capture whose client area is smaller than the captured frame:
//! it has a title bar or borders) is cropped to its client area and placed on a canvas the
//! size of that client area (`capture/client_area.rs`), so a 1600x900 or 16:10 window records
//! at its own size and shape, with no bars. The canvas keeps its size for the whole capture
//! session, so resizing the window never reopens NVENC: a resized client area is centred on
//! it, never upscaled, scaled down (aspect kept) only when larger. Everything else (monitor
//! captures, borderless and fullscreen windows) records at its own size, with the
//! border-jitter crop / pad of [`target_size`].

/// Shortest recorded height the resolution setting takes (`RECALL_HEIGHT`).
pub const MIN_RECORD_HEIGHT: u32 = 360;

/// `RECALL_HEIGHT`, read once: the recorded picture's height cap (None = native). The
/// controller restarts the engine when it changes.
pub fn record_height() -> Option<u32> {
    static HEIGHT: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *HEIGHT.get_or_init(|| {
        let raw = std::env::var("RECALL_HEIGHT").ok()?.trim().parse::<u32>().ok()?;
        (raw != 0).then(|| raw.clamp(MIN_RECORD_HEIGHT, 4320))
    })
}

/// A `size` picture recorded under a `height` cap: the same shape scaled down so its height
/// is the cap (even sides, 4:2:0), never scaled up. None or a cap at or above it: unchanged.
pub fn capped(size: (u32, u32), height: Option<u32>) -> (u32, u32) {
    match height {
        Some(cap) if cap < size.1 => {
            let h = (cap & !1).max(2);
            let w = (u64::from(size.0) * u64::from(h) + u64::from(size.1) / 2) / u64::from(size.1.max(1));
            (((w as u32) & !1).max(2), h)
        }
        _ => size,
    }
}

/// Largest window-border jitter (px per axis) absorbed by cropping / padding instead of
/// reopening the encoder: both frame paths ([`target_size`], `encoder/frame_fit.rs`) and the
/// client-area check (`capture/client_area.rs`) use it. Games wobble by 2 (2560x1440 <->
/// 2560x1438); the smallest real change in the log is 14 (Helldivers 2, 2546 <-> 2560).
pub const MAX_JITTER_PIXELS: u32 = 8;

/// Smallest even frame every NVENC codec opens: H.264 146x50, HEVC 130x34, AV1 130x66
/// (RTX 5090, swept by `encoder/wobble_tests.rs`). The largest of each axis, so a small
/// frame never pushes an AV1 session onto the H.264 fallback.
pub const MIN_ENCODE: (u32, u32) = (146, 66);

/// Whether NVENC can open on a `size` frame (it encodes the even-cropped size).
pub fn encodable(size: (u32, u32)) -> bool {
    size.0 & !1 >= MIN_ENCODE.0 && size.1 & !1 >= MIN_ENCODE.1
}

/// Encoder frame size for a `src` capture recorded at its own size (not on the canvas): the
/// current size while the source stays within [`MAX_JITTER_PIXELS`] of it, else the source
/// cropped to even dimensions (NVENC).
pub fn target_size(current: Option<(u32, u32)>, src: (u32, u32)) -> (u32, u32) {
    if let Some((w, h)) = current {
        if src.0.abs_diff(w) <= MAX_JITTER_PIXELS && src.1.abs_diff(h) <= MAX_JITTER_PIXELS {
            return (w, h);
        }
    }
    ((src.0 & !1).max(2), (src.1 & !1).max(2))
}

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

    /// What the zero-copy path renders for a `frame`-sized capture: `crop` of it on the
    /// `canvas` when the session records there, else the frame at its own size in the current
    /// encoder frame `target`. None when that size is too small to encode (a window on its
    /// way to the taskbar): the frame is skipped, and the encoder, buffer and pool stay.
    pub fn recorded(frame: (u32, u32), crop: Option<Rect>, canvas: Option<(u32, u32)>, target: Option<(u32, u32)>) -> Option<Self> {
        Self::recorded_at(frame, crop, canvas, target, record_height())
    }

    /// [`Self::recorded`] under a `height` cap (the recording resolution setting): the
    /// canvas, or the frame, scaled down to it in its own shape.
    pub fn recorded_at(frame: (u32, u32), crop: Option<Rect>, canvas: Option<(u32, u32)>, target: Option<(u32, u32)>, height: Option<u32>) -> Option<Self> {
        if let Some(canvas) = canvas {
            return Some(Self::on_canvas(crop.unwrap_or(Rect::full(frame)), capped(canvas, height)));
        }
        let out = target_size(target, capped(frame, height));
        encodable(out).then(|| Self::sized(frame, out, height))
    }

    /// A `frame`-sized capture in an `out` frame: [`Self::native`] at its own size, or the
    /// whole frame scaled into `out` when a `height` cap shrinks it.
    pub fn sized(frame: (u32, u32), out: (u32, u32), height: Option<u32>) -> Self {
        if capped(frame, height) == frame {
            return Self::native(frame, out);
        }
        Self { src: Rect::full(frame), dst: Rect::full(out), out, canvas: false }
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
