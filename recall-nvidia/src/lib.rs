//! NVIDIA's game knowledge for Recall, read from the driver's profile database
//! (NvAPI DRS). [`Nvapi`] is read-only: a session is created, loaded, queried
//! and destroyed; nothing is ever saved back. [`DriverWriter`] writes one
//! program's setting, and only the in-app fixer uses it, after the user
//! clicked Fix for that fix.
//!
//! Every common app has a predefined driver profile (Chrome, Edge, Firefox,
//! Discord, Spotify, Teams, Zoom, PowerPoint, VLC, Explorer, Notepad, Steam),
//! so "has a profile" says nothing. The game signal is one setting,
//! [`GAME_MARKER_ID`] (`0x105E2A1D`, unnamed by the driver, Ansel-related: the
//! driver's Ansel test apps carry it too). Measured on driver DB 2026-11-05
//! (7,984 profiles): 1,470 profiles hold it non-zero; a name scan of them found
//! only games and Ansel test apps; Wallpaper Engine holds it as 0. None of 15 non-game apps
//! on the test PC carries it; 40 of 45 installed games do.
//!
//! The ID is private to the driver and may change in a later driver: then
//! nothing is a known game and Recall falls back to its own rules.

mod drs;
mod drs_write;

pub use drs::Nvapi;
pub use drs_write::{DriverWriter, Placement, Reverted};

/// Driver setting "Vulkan/OpenGL present method" (per program).
pub mod present_method {
    /// The setting's ID in the driver profile database.
    pub const ID: u32 = 0x20D6_90F8;
    /// The name the NVIDIA Control Panel shows.
    pub const NAME: &str = "Vulkan/OpenGL present method";
    /// "Prefer native".
    pub const NATIVE: u32 = 0;
    /// "Prefer layered on DXGI Swapchain": OpenGL frames go through a DXGI
    /// swap chain, so the desktop compositor (and windows above the game) see them.
    pub const LAYERED_ON_DXGI: u32 = 1;
    /// "Auto", the driver's default.
    pub const AUTO: u32 = 2;

    /// The Control Panel's name for a value.
    pub fn value_name(v: u32) -> String {
        match v {
            NATIVE => "Prefer native".into(),
            LAYERED_ON_DXGI => "Prefer layered on DXGI Swapchain".into(),
            AUTO => "Auto".into(),
            other => format!("unknown value {other}"),
        }
    }
}

/// The driver setting whose non-zero value marks a game profile.
pub const GAME_MARKER_ID: u32 = 0x105E2A1D;

/// One application's entry in the driver profile database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// The driver has no profile for this exe.
    NoProfile,
    /// A profile matched; `marker` = the value of [`GAME_MARKER_ID`], if set.
    Profile { name: String, marker: Option<u32> },
}

impl Lookup {
    /// NVIDIA treats this exe as a game.
    pub fn is_game(&self) -> bool {
        matches!(self, Lookup::Profile { marker: Some(v), .. } if *v != 0)
    }
}

#[cfg(test)]
mod tests;
