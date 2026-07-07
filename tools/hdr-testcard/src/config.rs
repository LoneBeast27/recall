//! CLI parsing + the known-value colour patches that make this a colourimeter
//! reference.
//!
//! Every patch carries a *linear scRGB* value (not gamma-encoded). The Recall
//! capture engine (`RECALL_DUMP=out.png`) reads the screen back as scRGB-linear
//! Rgba16F and reports per-channel min/max/mean + %pixels above 1.0, so these
//! commanded values are directly comparable to the measured output — a
//! closed-loop check of HDR flips and gamut clamps with no human eyes.

use serde::Serialize;

/// One labelled colour patch with its commanded linear scRGB value.
#[derive(Clone, Copy, Serialize)]
pub struct Patch {
    pub name: &'static str,
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl Patch {
    const fn new(name: &'static str, r: f32, g: f32, b: f32) -> Self {
        Self { name, r, g, b }
    }
}

/// The default 4x2 grid, row-major (top-left first).
///
/// Values chosen so an orchestrator can verify distinct behaviours:
///   - Primaries R/G/B at 1.0 exercise per-channel gamut boundaries.
///   - gray 0.18 (mid-grey card) and 0.5 sit safely inside SDR.
///   - white 1.0 is the SDR ceiling reference.
///   - white 2.0 / 4.0 are HDR patches: they only survive to the display as
///     >1.0 when an FP16 / scRGB surface is obtained AND Windows HDR is on.
///     On an sRGB8 surface they clamp to 1.0 (reported via the startup JSON).
pub const GRID_PATCHES: [Patch; 8] = [
    Patch::new("R", 1.0, 0.0, 0.0),
    Patch::new("G", 0.0, 1.0, 0.0),
    Patch::new("B", 0.0, 0.0, 1.0),
    Patch::new("gray 0.18", 0.18, 0.18, 0.18),
    Patch::new("gray 0.5", 0.5, 0.5, 0.5),
    Patch::new("white 1.0", 1.0, 1.0, 1.0),
    Patch::new("white 2.0", 2.0, 2.0, 2.0),
    Patch::new("white 4.0", 4.0, 4.0, 4.0),
];

/// Grid layout: 4 columns x 2 rows over the 8 patches above.
pub const GRID_COLS: u32 = 4;
pub const GRID_ROWS: u32 = 2;

/// Which test pattern to render.
#[derive(Clone)]
pub enum Mode {
    /// The default labelled 4x2 grid of known linear values.
    Grid,
    /// A single known linear value filling the whole window. This is the
    /// primary colourimeter mode: the capture engine's MEAN statistic becomes
    /// directly comparable to the commanded (r, g, b).
    Solid { r: f32, g: f32, b: f32 },
}

impl Mode {
    /// `"grid"` or `"solid"` for the startup JSON.
    pub fn label(&self) -> &'static str {
        match self {
            Mode::Grid => "grid",
            Mode::Solid { .. } => "solid",
        }
    }

    /// The patch list this mode presents, so the JSON always reflects what is
    /// actually on screen (one solid patch, or the eight grid patches).
    pub fn patches(&self) -> Vec<Patch> {
        match self {
            Mode::Grid => GRID_PATCHES.to_vec(),
            Mode::Solid { r, g, b } => vec![Patch::new("solid", *r, *g, *b)],
        }
    }
}

/// Parsed command-line configuration.
pub struct Config {
    pub mode: Mode,
    pub fullscreen: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: Mode::Grid,
            fullscreen: false,
        }
    }
}

/// Parse `--solid R,G,B` (floats) and `--fullscreen`. Unknown args are ignored,
/// matching Recall's lenient parser. A malformed `--solid` value is a hard error
/// (better to fail loudly than silently mis-command the colourimeter).
pub fn parse_args() -> Config {
    let mut cfg = Config::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--fullscreen" => cfg.fullscreen = true,
            "--solid" => {
                let spec = args.next().unwrap_or_else(|| {
                    eprintln!("--solid requires an R,G,B argument (e.g. --solid 0.5,0.5,0.5)");
                    std::process::exit(2);
                });
                cfg.mode = parse_solid(&spec);
            }
            _ => {}
        }
    }
    cfg
}

/// Parse `"R,G,B"` of linear floats into a `Solid` mode. Values may exceed 1.0
/// (HDR); negatives are rejected.
fn parse_solid(spec: &str) -> Mode {
    let parts: Vec<&str> = spec.split(',').collect();
    if parts.len() != 3 {
        eprintln!("--solid expects exactly three comma-separated floats, got: {spec:?}");
        std::process::exit(2);
    }
    let mut vals = [0.0f32; 3];
    for (i, p) in parts.iter().enumerate() {
        match p.trim().parse::<f32>() {
            Ok(v) if v >= 0.0 && v.is_finite() => vals[i] = v,
            _ => {
                eprintln!("--solid component {:?} is not a valid non-negative float", p.trim());
                std::process::exit(2);
            }
        }
    }
    Mode::Solid {
        r: vals[0],
        g: vals[1],
        b: vals[2],
    }
}
