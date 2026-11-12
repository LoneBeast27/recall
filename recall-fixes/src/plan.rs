//! A fix as data: which problem, which changes, and the exact lines the "i"
//! view shows for each change. Titles and problem text come from a fixed
//! table keyed by [`FixId`], never from a file.

use serde::{Deserialize, Serialize};

/// Every fix Recall knows. The helper refuses any other ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FixId {
    /// Minecraft Java in borderless or fullscreen on an NVIDIA card: the
    /// driver presents OpenGL frames past the desktop compositor, so the
    /// overlay island is drawn but never seen.
    MinecraftOverlayNvidia,
}

impl FixId {
    pub const ALL: [FixId; 1] = [FixId::MinecraftOverlayNvidia];

    /// The stable ID on the helper's command line and in receipts.
    pub fn as_str(self) -> &'static str {
        match self {
            FixId::MinecraftOverlayNvidia => "minecraft-overlay-nvidia",
        }
    }

    pub fn parse(s: &str) -> Option<FixId> {
        FixId::ALL.into_iter().find(|f| f.as_str() == s)
    }

    pub fn title(self) -> &'static str {
        match self {
            FixId::MinecraftOverlayNvidia => "Show the overlay in Minecraft",
        }
    }

    /// What is wrong, in plain words.
    pub fn problem(self) -> &'static str {
        match self {
            FixId::MinecraftOverlayNvidia => "In borderless and fullscreen, the NVIDIA driver draws Minecraft past Windows' desktop compositor, so Recall's overlay is hidden behind the game.",
        }
    }

    /// What the user does afterwards for the fix to take effect.
    pub fn after(self) -> Option<&'static str> {
        match self {
            FixId::MinecraftOverlayNvidia => Some("Restart Minecraft"),
        }
    }

    /// The driver profile Recall makes for `target`: one per program path,
    /// so Undo of one never touches another.
    pub fn own_profile(self, target: &str) -> String {
        let base = match self {
            FixId::MinecraftOverlayNvidia => "Recall: Minecraft",
        };
        format!("{base} {:08x}", crate::receipt::fnv1a(&target.to_lowercase()) as u32)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Vendor {
    Nvidia,
}

impl Vendor {
    pub fn name(self) -> &'static str {
        match self {
            Vendor::Nvidia => "NVIDIA",
        }
    }
}

/// One per-program driver setting, in a profile Recall makes for the
/// program alone. Existing profiles are never edited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverAppSetting {
    pub vendor: Vendor,
    /// Full path of the program, as Recall found it running.
    pub exe: String,
    pub setting_id: u32,
    pub setting_name: String,
    /// The profile Recall makes, holding only this program.
    pub new_profile: String,
    /// The value and its Control Panel name.
    pub to: (u32, String),
}

/// One change a fix makes. Closed set: the helper executes only these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Change {
    DriverAppSetting(DriverAppSetting),
}

impl Change {
    pub fn needs_admin(&self) -> bool {
        match self {
            // The NVIDIA profile database (ProgramData, "NVIDIA Corporation/Drs")
            // lets every account write it, as the NVIDIA app does.
            Change::DriverAppSetting(_) => false,
        }
    }

    /// One plain line for the card.
    pub fn summary(&self) -> String {
        match self {
            Change::DriverAppSetting(c) => format!("Set the {} driver setting \"{}\" to \"{}\" for programs that use this Java install", c.vendor.name(), c.setting_name, c.to.1),
        }
    }

    /// The exact lines the "i" view shows: where, what, before and after,
    /// the calls made, the safety check, and how Undo reverses it.
    pub fn exact(&self) -> Vec<String> {
        match self {
            Change::DriverAppSetting(c) => driver_lines(c),
        }
    }
}

fn driver_lines(c: &DriverAppSetting) -> Vec<String> {
    let v = c.vendor.name();
    vec![
        format!("Where: the {v} driver's profile database (the same one the {v} Control Panel edits)"),
        format!("Program: {} (every program that runs on this Java install uses this file, so all of them get the setting; other Java installs do not)", c.exe),
        format!("Profile: a new profile \"{}\" holding only this program. No existing profile is changed.", c.new_profile),
        format!("Setting 0x{:08X} \"{}\": not set, driver default (Auto) → {} ({})", c.setting_id, c.setting_name, c.to.1, c.to.0),
        "Driver calls (NvAPI DRS): CreateProfile, CreateApplication, SetSetting, SaveSettings".to_owned(),
        "Check before saving: another program with the same file name must not match the new profile. If the driver would apply it to every one of them, nothing is saved.".to_owned(),
        "No administrator prompt: the driver lets your own account save its profiles".to_owned(),
        format!("Undo: delete \"{}\". If you added to it since, only Recall's program entry and its setting (while it still has Recall's value) are removed.", c.new_profile),
    ]
}

/// One step of a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub change: Change,
}

/// Everything one fix does, shown before the user agrees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub fix: FixId,
    pub steps: Vec<Step>,
}

impl Plan {
    pub fn needs_admin(&self) -> bool {
        self.steps.iter().any(|s| s.change.needs_admin())
    }

    /// Ties a helper result to the plan it ran (stable across runs).
    pub fn hash(&self) -> String {
        format!("{:016x}", crate::receipt::fnv1a(&serde_json::to_string(self).unwrap_or_default()))
    }
}

/// The Minecraft overlay fix for the javaw.exe Recall found running.
pub fn minecraft_overlay_nvidia(setting: DriverAppSetting) -> Plan {
    Plan { fix: FixId::MinecraftOverlayNvidia, steps: vec![Step { change: Change::DriverAppSetting(setting) }] }
}
