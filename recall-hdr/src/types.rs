//! Frozen Wave-0 contracts shared across the whole crate.
//!
//! Everything downstream (the rule engine, the tray UI, the clamp driver, the
//! HDR toggle) is written against these types. They are intentionally small,
//! `Copy` where possible, and serde-derivable so rules/config round-trip through
//! TOML/JSON unchanged. Changing a variant here is a breaking change for every
//! other module -- treat additions as append-only.

use serde::{Deserialize, Serialize};

/// How the foreground app was classified. Drives which rule (if any) fires.
///
/// - `HdrGame`  -- a game we want Windows HDR *on* for.
/// - `SdrGame`  -- a game we want HDR *off* + the sRGB gamut clamp engaged for
///   (wide-gamut panel, SDR title, so we clamp to avoid oversaturation).
/// - `NonGame`  -- desktop / browser / anything we leave in the user's baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppKind {
    HdrGame,
    SdrGame,
    NonGame,
}

/// Gamut-clamp intensity requested of the `GamutClamp` driver.
///
/// - `Punchy`   -- no clamp / lightest touch: let the wide-gamut panel run wide
///   (vivid but oversaturated for sRGB content). In the NvAPI driver this is a
///   *reset* (CSC disabled) -- the panel's native gamut.
/// - `Accurate` -- clamp the panel to sRGB primaries (color-correct SDR).
/// - `Off`      -- clamp management disabled entirely; do not touch CSC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClampMode {
    Punchy,
    Accurate,
    Off,
}

/// Which rule matched the current foreground app. Mirrors `AppKind` minus the
/// desktop case, plus an explicit `None` for "no rule fired / leave baseline".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleKind {
    HdrGame,
    SdrGame,
    None,
}

/// A single reconciler step: the smallest unit the executor applies to move the
/// live display toward the desired state. Emitted by diffing `DesiredDisplay`
/// against the observed live state; `Noop` when they already agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    /// Turn Windows HDR on for the primary display.
    EnableHdr,
    /// Turn Windows HDR off for the primary display.
    DisableHdr,
    /// Apply a gamut clamp of the given intensity via the `GamutClamp` driver.
    SetClamp(ClampMode),
    /// Disable any active gamut clamp (reset CSC to the panel's native gamut).
    ClearClamp,
    /// Set the SDR-content white level (nits) used while HDR is on.
    SetSdrWhite(u32),
    /// Nothing to do -- live state already matches desired.
    Noop,
}

/// The reconciler's target for the primary display. The executor drives the
/// live display toward this and never past it (idempotent: re-applying an
/// already-satisfied `DesiredDisplay` yields only `Noop`s).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesiredDisplay {
    /// Whether Windows HDR should be ON.
    pub hdr: bool,
    /// Gamut-clamp intensity to request.
    pub clamp: ClampMode,
    /// SDR-content white level in nits (meaningful only while `hdr == true`).
    pub sdr_white: u32,
}
