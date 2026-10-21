//! Recall design tokens: Agentic IDE monochrome surfaces plus one signal colour.
//! Footage supplies colour; red only marks "now" (live buffer, REC dot, playhead).
use gpui::{Hsla, Rgba, rgb};

// Surfaces, darkest to lightest.
pub const WINDOW: u32 = 0x030303;
pub const BASE: u32 = 0x0a0a0a;
pub const RAISED: u32 = 0x121212;
pub const OVERLAY: u32 = 0x191919;
pub const FLOATING: u32 = 0x202020;
pub const TOP: u32 = 0x282828;
pub const BORDER: u32 = 0x282828;
pub const BORDER_SUBTLE: u32 = 0x1d1d1d;
pub const BORDER_STRONG: u32 = 0x424242;

// Text. TERTIARY replaces the IDE placeholder grey for readable meta text (5.9:1 on BASE).
pub const TEXT: u32 = 0xededed;
pub const MUTED: u32 = 0xa3a3a3;
pub const TERTIARY: u32 = 0x8f8f8f;
pub const PLACEHOLDER: u32 = 0x757575;
pub const DISABLED: u32 = 0x565656;
pub const FOCUS: u32 = 0xe5e5e5;

// Signal and safety.
pub const SIGNAL: u32 = 0xff453a;
pub const ON_SIGNAL: u32 = 0x0a0a0a;
pub const ACTIVE: u32 = 0x81c995;
pub const WARNING: u32 = 0xfdd663;
pub const ERROR: u32 = 0xf28b82;

/// UI typeface. `RECALL_FONT=outfit|geist|manrope|instrument` picks one for comparison.
pub fn sans() -> &'static str {
    static FONT: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    FONT.get_or_init(|| match std::env::var("RECALL_FONT").unwrap_or_default().to_lowercase().as_str() {
        "geist" => "Geist",
        "manrope" => "Manrope",
        "instrument" => "Instrument Sans",
        _ => "Outfit",
    })
}

/// Numbers use the UI face with tabular figures (set on the root), not a coding font.
pub fn mono() -> &'static str {
    sans()
}

pub fn c(value: u32) -> Hsla {
    rgb(value).into()
}

/// Colour with alpha (0.0..=1.0).
pub fn ca(value: u32, alpha: f32) -> Hsla {
    let mut color: Rgba = rgb(value);
    color.a = alpha;
    color.into()
}
