//! How much of the library header fits: the window can be as narrow as 1000 px (about 700 px of
//! header after the sidebar), and the header holds a title, the clip count, Resume, search and
//! three dropdowns. Parts give way in this order: Resume's clip title, the dropdowns' long
//! words, the search box's width and its Ctrl K hint, then Resume's word (its icon stays).
//! Widths are logical px, so the rule holds at any UI scale.

/// What the header shows at one width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeaderFit {
    /// Resume shows the clip's title beside it.
    pub resume_title: bool,
    /// Resume shows its word; else only its play icon.
    pub resume_word: bool,
    /// Dropdowns say "Newest first" / "By time"; else "Newest" / "Time".
    pub long_menus: bool,
    /// Search box width, and whether it shows the Ctrl K hint.
    pub search: f32,
    pub search_hint: bool,
}

const FULL: HeaderFit = HeaderFit { resume_title: true, resume_word: true, long_menus: true, search: 260.0, search_hint: true };

/// Room each part takes (logical px). Generous: a long label measured wider than this would
/// still truncate (the title) or clip inside its own button, never push the row wider.
mod room {
    pub const TITLE: f32 = 130.0;
    /// Wide enough for "1,234,567".
    pub const COUNT: f32 = 56.0;
    pub const GAP: f32 = 12.0;
    pub const RESUME_ICON: f32 = 36.0;
    pub const RESUME_WORD: f32 = 64.0;
    pub const RESUME_TITLE: f32 = 208.0;
    pub const MENUS_LONG: f32 = 370.0;
    pub const MENUS_SHORT: f32 = 262.0;
}

/// Header inner width before the grid has been measured (the first frame): the window less
/// the sidebar (232), the panel's right margin (8) and the content padding (2 x 24).
pub fn first_frame_width(window: f32) -> f32 {
    window - 232.0 - 8.0 - 48.0
}

/// The widest form that fits `width` (the header's inner width).
pub fn header_fit(width: f32) -> HeaderFit {
    let steps = [
        FULL,
        HeaderFit { resume_title: false, ..FULL },
        HeaderFit { resume_title: false, long_menus: false, ..FULL },
        HeaderFit { resume_title: false, long_menus: false, search: 180.0, search_hint: false, ..FULL },
        HeaderFit { resume_title: false, resume_word: false, long_menus: false, search: 140.0, search_hint: false },
    ];
    steps.into_iter().find(|fit| needed(fit) <= width).unwrap_or(steps[steps.len() - 1])
}

/// Width the header needs in this form, with Resume shown (the widest case).
pub fn needed(fit: &HeaderFit) -> f32 {
    let resume = room::RESUME_ICON + if fit.resume_word { room::RESUME_WORD } else { 0.0 } + if fit.resume_title { room::RESUME_TITLE } else { 0.0 };
    let menus = if fit.long_menus { room::MENUS_LONG } else { room::MENUS_SHORT };
    // title, count, Resume, search, 3 dropdowns: 7 parts, 6 gaps.
    room::TITLE + room::COUNT + resume + fit.search + menus + room::GAP * 6.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_smallest_window_fits_the_header() {
        // 1000 px window minus the sidebar and padding leaves about 700 px.
        let fit = header_fit(700.0);
        assert!(needed(&fit) <= 700.0, "needs {}", needed(&fit));
        assert!(!fit.resume_title && !fit.long_menus);
        // The first frame, before the grid is measured, uses the window's own width.
        assert_eq!(header_fit(first_frame_width(1000.0)), fit);
        assert_eq!(first_frame_width(1000.0), 712.0);
    }

    #[test]
    fn a_wide_header_shows_everything_and_parts_give_way_in_order() {
        assert_eq!(header_fit(1400.0), FULL);
        let mut last = FULL;
        for width in (600..=1400).rev() {
            let fit = header_fit(width as f32);
            if width >= 700 {
                assert!(needed(&fit) <= width as f32, "{width} px: needs {}", needed(&fit));
            }
            // Narrower never brings a part back.
            assert!(!fit.resume_title || last.resume_title, "{width}");
            assert!(!fit.long_menus || last.long_menus, "{width}");
            assert!(!fit.resume_word || last.resume_word, "{width}");
            assert!(fit.search <= last.search, "{width}");
            // Resume's clip title goes before anything else does.
            if !fit.long_menus || fit.search < 260.0 {
                assert!(!fit.resume_title, "{width}");
            }
            last = fit;
        }
    }
}
