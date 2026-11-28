//! Live window previews for the Record picker, drawn by Windows itself (DWM thumbnails): the
//! compositor scales the source window into a rect of Recall's window, so a preview costs no
//! CPU copy, no texture upload and no gpui image (whose atlas never frees: 11-15 audit). They
//! exist only while the picker is open: [`Thumbs::clear`] on close, and on drop.
//!
//! Each frame the picker's cards place their preview ([`Thumbs::place`], from a canvas paint,
//! in physical px of Recall's window, cut to the scroll area); a preview not placed since the
//! last frame is hidden ([`Thumbs::end_frame`]), so a tab switch or a scroll never leaves one
//! behind.
use std::collections::{HashMap, HashSet};

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_RECTSOURCE, DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE,
    DwmQueryThumbnailSourceSize, DwmRegisterThumbnail, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
};

use crate::services::capture_targets;

/// One registered preview.
struct Live {
    id: isize,
    shown: bool,
}

#[derive(Default)]
pub struct Thumbs {
    /// Recall's own window (the destination), once known.
    dest: Option<isize>,
    live: HashMap<isize, Live>,
    /// Sources DWM refused (shown as their icon instead), not asked again until cleared.
    refused: HashSet<isize>,
    /// Placed since the last [`end_frame`](Self::end_frame).
    placed: HashSet<isize>,
}

fn rect(r: [i32; 4]) -> RECT {
    RECT { left: r[0], top: r[1], right: r[2], bottom: r[3] }
}

impl Thumbs {
    pub fn set_dest(&mut self, hwnd: isize) {
        if self.dest != Some(hwnd) {
            self.clear();
            self.dest = Some(hwnd);
        }
    }

    /// Show `src` fit inside `dest`, cut to `clip` (both physical px in Recall's window).
    /// `false` when DWM refused the source.
    pub fn place(&mut self, src: isize, dest: [i32; 4], clip: [i32; 4]) -> bool {
        let Some(own) = self.dest else { return false };
        if self.refused.contains(&src) {
            return false;
        }
        self.placed.insert(src);
        if !self.live.contains_key(&src) {
            // SAFETY: two window handles; the id is unregistered in `clear` / `end_frame` / drop.
            match unsafe { DwmRegisterThumbnail(HWND(own as *mut _), HWND(src as *mut _)) } {
                Ok(id) => {
                    self.live.insert(src, Live { id, shown: false });
                }
                Err(_) => {
                    self.refused.insert(src);
                    return false;
                }
            }
        }
        let Some(live) = self.live.get_mut(&src) else { return false };
        // SAFETY: a registered id.
        let size = unsafe { DwmQueryThumbnailSourceSize(live.id) }.unwrap_or_default();
        let fitted = capture_targets::fit(size.cx, size.cy, dest);
        let mut props = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_VISIBLE | DWM_TNP_RECTDESTINATION | DWM_TNP_RECTSOURCE | DWM_TNP_OPACITY | DWM_TNP_SOURCECLIENTAREAONLY,
            opacity: 255,
            fSourceClientAreaOnly: false.into(),
            ..Default::default()
        };
        match capture_targets::clip(fitted, size.cx, size.cy, clip) {
            Some((shown, source)) => {
                props.rcDestination = rect(shown);
                props.rcSource = rect(source);
                props.fVisible = true.into();
            }
            None => props.fVisible = false.into(),
        }
        live.shown = props.fVisible.as_bool();
        // SAFETY: a registered id and a filled struct.
        unsafe { DwmUpdateThumbnailProperties(live.id, &props) }.is_ok()
    }

    /// Hide every preview not placed since the last call (a card no longer drawn).
    pub fn end_frame(&mut self) {
        let placed = std::mem::take(&mut self.placed);
        for (src, live) in self.live.iter_mut() {
            if live.shown && !placed.contains(src) {
                let props = DWM_THUMBNAIL_PROPERTIES { dwFlags: DWM_TNP_VISIBLE, fVisible: false.into(), ..Default::default() };
                // SAFETY: a registered id.
                let _ = unsafe { DwmUpdateThumbnailProperties(live.id, &props) };
                live.shown = false;
            }
        }
    }

    /// Every preview gone (the picker closed).
    pub fn clear(&mut self) {
        for (_, live) in self.live.drain() {
            // SAFETY: each id was registered once and is unregistered once.
            let _ = unsafe { DwmUnregisterThumbnail(live.id) };
        }
        self.refused.clear();
        self.placed.clear();
    }
}

impl Drop for Thumbs {
    fn drop(&mut self) {
        self.clear();
    }
}
