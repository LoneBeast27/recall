//! Which part of a WGC frame holds this frame's picture.
//!
//! After a window resize, WGC keeps delivering frames from the old frame pool until the pool
//! is recreated at the new `ContentSize` (`windows-capture` does that on the frame that shows
//! the change). In those frames the texture and the picture differ in size: a shrunk window
//! leaves the previous frames' pixels to the right of / below its picture (the "frame leak"),
//! a grown one is cut off. Only `ContentSize`, top-left, is this frame's picture.
//!
//! Such a frame is dropped (the encoder repeats the last good one, VFR) for up to
//! [`MAX_DROPS`] in a row; a mismatch that lasts longer (a window resized continuously) records
//! the picture box (texture and content overlap) instead of freezing. Nothing outside that box
//! is ever read. OBS clamps to the same box (`libobs-winrt/winrt-capture.cpp` `get_client_box`).

/// Consecutive mismatched frames dropped before the picture box is recorded instead.
pub const MAX_DROPS: u32 = 8;

/// What to do with one frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Lay the frame out at this size (the texture's top-left box).
    Record((u32, u32)),
    /// Skip it: its texture is a stale pool buffer.
    Drop,
}

/// Mismatch run tracker for one capture session.
#[derive(Default)]
pub struct ContentGuard {
    mismatched: u32,
}

impl ContentGuard {
    /// A frame whose texture is `texture` sized, holding a `content` picture (None = the
    /// source has no separate content size: Desktop Duplication).
    pub fn check(&mut self, texture: (u32, u32), content: Option<(u32, u32)>) -> Verdict {
        let Some(content) = content.filter(|c| *c != texture && c.0 > 0 && c.1 > 0) else {
            self.mismatched = 0;
            return Verdict::Record(texture);
        };
        self.mismatched = self.mismatched.saturating_add(1);
        if self.mismatched <= MAX_DROPS {
            return Verdict::Drop;
        }
        Verdict::Record((texture.0.min(content.0), texture.1.min(content.1)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_frames_record_whole() {
        let mut g = ContentGuard::default();
        assert_eq!(g.check((2560, 1440), Some((2560, 1440))), Verdict::Record((2560, 1440)));
        assert_eq!(g.check((1920, 1080), None), Verdict::Record((1920, 1080)), "Desktop Duplication");
    }

    #[test]
    fn the_stale_buffer_after_a_shrink_is_never_recorded() {
        // 2560x1440 fullscreen -> 1280x720 windowed: the old pool's texture holds the last
        // fullscreen picture around the new one.
        let mut g = ContentGuard::default();
        assert_eq!(g.check((2560, 1440), Some((1280, 720))), Verdict::Drop);
        assert_eq!(g.check((2560, 1440), Some((1280, 720))), Verdict::Drop);
        assert_eq!(g.check((1280, 720), Some((1280, 720))), Verdict::Record((1280, 720)), "pool recreated");
    }

    #[test]
    fn a_long_mismatch_records_only_the_picture_box() {
        let mut g = ContentGuard::default();
        for _ in 0..MAX_DROPS {
            assert_eq!(g.check((2560, 1440), Some((1300, 700))), Verdict::Drop);
        }
        assert_eq!(g.check((2560, 1440), Some((1300, 700))), Verdict::Record((1300, 700)), "shrunk: content only");
        assert_eq!(g.check((1280, 720), Some((1600, 900))), Verdict::Record((1280, 720)), "grown: what the texture holds");
        assert_eq!(g.check((1600, 900), Some((1600, 900))), Verdict::Record((1600, 900)));
        assert_eq!(g.check((1600, 900), Some((1280, 720))), Verdict::Drop, "a new run starts dropping again");
    }

    #[test]
    fn an_empty_content_size_is_ignored() {
        let mut g = ContentGuard::default();
        assert_eq!(g.check((1280, 720), Some((0, 0))), Verdict::Record((1280, 720)));
    }
}
