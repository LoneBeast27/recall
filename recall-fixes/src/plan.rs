//! A fix as data: which problem, which changes, and the exact lines the "i"
//! view shows for each change.

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

/// The program's driver profile before the change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Profile {
    /// No profile holds the program; the fix makes `new_profile` for it alone.
    None { new_profile: String },
    /// The program already belongs to this profile; only the one setting changes.
    Existing(String),
}

/// One per-program driver setting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverAppSetting {
    pub vendor: Vendor,
    /// Full path of the program, as Recall found it running.
    pub exe: String,
    pub setting_id: u32,
    pub setting_name: String,
    pub profile: Profile,
    /// The value now, with its Control Panel name; `None` = the profile does
    /// not set it, so the driver default applies.
    pub from: Option<(u32, String)>,
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
            Change::DriverAppSetting(c) => format!("Set the {} driver setting \"{}\" to \"{}\" for this one program", c.vendor.name(), c.setting_name, c.to.1),
        }
    }

    /// The exact lines the "i" view shows: where, what, before and after,
    /// the calls made, and how Undo reverses it.
    pub fn exact(&self) -> Vec<String> {
        match self {
            Change::DriverAppSetting(c) => driver_lines(c),
        }
    }
}

fn driver_lines(c: &DriverAppSetting) -> Vec<String> {
    let v = c.vendor.name();
    let before = match &c.from {
        Some((n, name)) => format!("{name} ({n})"),
        None => "not set, driver default".to_owned(),
    };
    let mut lines = vec![
        format!("Where: the {v} driver's profile database (the same one the {v} Control Panel edits)"),
        format!("Program: {}", c.exe),
    ];
    let (profile_line, calls, undo) = match &c.profile {
        Profile::None { new_profile } => (
            format!("Profile: none today; a new profile \"{new_profile}\" is made holding only this program"),
            "CreateProfile, CreateApplication, SetSetting, SaveSettings",
            format!("Undo: delete the profile \"{new_profile}\""),
        ),
        Profile::Existing(name) => (
            format!("Profile: \"{name}\" (already holds this program; nothing else in it changes)"),
            "SetSetting, SaveSettings",
            match &c.from {
                Some(_) => format!("Undo: put the setting back to {before}"),
                None => "Undo: remove this setting from the profile again".to_owned(),
            },
        ),
    };
    lines.push(profile_line);
    lines.push(format!("Setting 0x{:08X} \"{}\": {before} → {} ({})", c.setting_id, c.setting_name, c.to.1, c.to.0));
    lines.push(format!("Driver calls (NvAPI DRS): {calls}"));
    lines.push("No administrator prompt: the driver lets your own account save its profiles".to_owned());
    lines.push(undo);
    lines
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
    pub title: String,
    /// What is wrong, in plain words.
    pub problem: String,
    pub steps: Vec<Step>,
    /// What the user does afterwards for the fix to take effect, if anything.
    pub after: Option<String>,
}

impl Plan {
    pub fn needs_admin(&self) -> bool {
        self.steps.iter().any(|s| s.change.needs_admin())
    }
}

/// The driver profile a fix makes for `target` when the program has none:
/// one per program path, so Undo of one never touches another.
pub fn new_profile_name(fix: FixId, target: &str) -> String {
    let base = match fix {
        FixId::MinecraftOverlayNvidia => "Recall: Minecraft",
    };
    format!("{base} {:08x}", crate::receipt::fnv1a(&target.to_lowercase()) as u32)
}

/// The Minecraft overlay fix for the javaw.exe Recall found running.
pub fn minecraft_overlay_nvidia(exe: &str, setting: DriverAppSetting) -> Plan {
    debug_assert_eq!(setting.exe, exe);
    Plan {
        fix: FixId::MinecraftOverlayNvidia,
        title: "Show the overlay in Minecraft".into(),
        problem: "In borderless and fullscreen, the NVIDIA driver draws Minecraft past Windows' desktop compositor, so Recall's overlay is hidden behind the game.".into(),
        steps: vec![Step { change: Change::DriverAppSetting(setting) }],
        after: Some("Restart Minecraft".into()),
    }
}
