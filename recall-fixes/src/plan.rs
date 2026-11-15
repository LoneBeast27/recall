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
    /// Minecraft Java with the Cubes Without Borders mod: its default borderless window is
    /// exactly the screen size, which NVIDIA's OpenGL driver takes exclusively.
    MinecraftBorderlessCwb,
    /// Stats are on but FPS shows "-": the account is not in Performance Log Users.
    FpsPermission,
    /// Rocket League in its "Fullscreen" display mode: true exclusive fullscreen, so Windows
    /// composites no other window over it. Its own Borderless mode keeps the island visible.
    RocketLeagueBorderless,
}

impl FixId {
    pub const ALL: [FixId; 4] = [FixId::MinecraftOverlayNvidia, FixId::MinecraftBorderlessCwb, FixId::FpsPermission, FixId::RocketLeagueBorderless];

    /// The stable ID on the helper's command line and in receipts.
    pub fn as_str(self) -> &'static str {
        match self {
            FixId::MinecraftOverlayNvidia => "minecraft-overlay-nvidia",
            FixId::MinecraftBorderlessCwb => "minecraft-borderless-cwb",
            FixId::FpsPermission => "fps-permission",
            FixId::RocketLeagueBorderless => "rocket-league-borderless",
        }
    }

    /// Whether Recall offers this fix. Off: the app ignores an offer file left from before (no
    /// notice) and the helper refuses to apply it. Undo of a fix already applied still works, so
    /// it can always be taken back.
    pub fn offered(self) -> bool {
        match self {
            // Off for 0.1.6 (2026-11-12): the driver profile applied in 1 of 3 test runs, and the
            // Cubes Without Borders change is untested on a real PC.
            FixId::MinecraftOverlayNvidia | FixId::MinecraftBorderlessCwb => false,
            FixId::FpsPermission => true,
            // Off until a real switch in the game's Video menu confirms which settings-file
            // lines its Borderless mode writes (the community notes disagree).
            FixId::RocketLeagueBorderless => false,
        }
    }

    pub fn parse(s: &str) -> Option<FixId> {
        FixId::ALL.into_iter().find(|f| f.as_str() == s)
    }

    pub fn title(self) -> &'static str {
        match self {
            FixId::MinecraftOverlayNvidia => "Show the overlay in Minecraft",
            FixId::MinecraftBorderlessCwb => "Keep the island visible in Minecraft borderless",
            FixId::FpsPermission => "Show FPS on the stats island",
            FixId::RocketLeagueBorderless => "Show the island in Rocket League",
        }
    }

    /// What is wrong, in plain words.
    pub fn problem(self) -> &'static str {
        match self {
            FixId::MinecraftOverlayNvidia => "In borderless and fullscreen, the NVIDIA driver draws Minecraft past Windows' desktop compositor, so Recall's overlay is hidden behind the game.",
            FixId::MinecraftBorderlessCwb => "Minecraft's borderless mode (from the Cubes Without Borders mod) makes the window exactly your screen size, so the graphics driver takes the whole screen and hides Recall's island.",
            FixId::FpsPermission => "FPS needs a Windows permission.",
            FixId::RocketLeagueBorderless => "Rocket League's Fullscreen mode takes the display for itself, so Windows shows no other window over it and Recall's island is hidden. Its Borderless mode looks the same and keeps the island visible.",
        }
    }

    /// What the user does afterwards for the fix to take effect.
    pub fn after(self) -> Option<&'static str> {
        match self {
            FixId::MinecraftOverlayNvidia | FixId::MinecraftBorderlessCwb => Some("Restart Minecraft"),
            FixId::FpsPermission => Some("Sign out of Windows and back in"),
            FixId::RocketLeagueBorderless => Some("Start Rocket League again"),
        }
    }

    /// The driver profile Recall makes for `target`: one per program path,
    /// so Undo of one never touches another.
    pub fn own_profile(self, target: &str) -> String {
        let base = match self {
            FixId::MinecraftOverlayNvidia | FixId::MinecraftBorderlessCwb => "Recall: Minecraft",
            FixId::FpsPermission => "Recall",
            FixId::RocketLeagueBorderless => "Recall: Rocket League",
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
    /// The program entry written to the driver: the end of the path (the form NVIDIA's own
    /// profiles use). Empty in plans made before it existed.
    #[serde(default)]
    pub entry: String,
    pub setting_id: u32,
    pub setting_name: String,
    /// The profile Recall makes, holding only this program.
    pub new_profile: String,
    /// The value and its Control Panel name.
    pub to: (u32, String),
}

impl DriverAppSetting {
    /// The Java build the entry names: the entry without its `bin/<file>` end
    /// (`zulu25…-win_x64`, or `somelauncher/jre` for a generic folder), or the whole path for
    /// an old full-path entry.
    pub fn build(&self) -> String {
        if self.entry.is_empty() {
            return self.exe.clone();
        }
        let parts: Vec<&str> = self.entry.split('/').filter(|p| !p.is_empty()).collect();
        if parts.len() >= 3 { parts[..parts.len() - 2].join("/") } else { self.entry.clone() }
    }
}

/// One key in a program's own JSON config file (the user's file; no administrator).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonKey {
    pub file: String,
    pub key: String,
    /// The value now; `None` = not set (the program's default applies).
    pub from: Option<serde_json::Value>,
    pub to: serde_json::Value,
}

/// The user's own account in a Windows local group (an administrator change).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalGroup {
    /// The group's well-known SID (its name is translated on non-English Windows).
    pub group_sid: String,
    pub group_name: String,
    /// The account's SID (this user's own account).
    pub account: String,
}

/// One `key=value` line in one section of a game's own INI settings file (the user's file).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IniKey {
    pub file: String,
    pub section: String,
    pub key: String,
    /// The value now; `None` = the line is not there (the game's default applies).
    pub from: Option<String>,
    pub to: String,
}

/// One change a fix makes. Closed set: the helper executes only these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Change {
    DriverAppSetting(DriverAppSetting),
    JsonKey(JsonKey),
    LocalGroup(LocalGroup),
    IniKey(IniKey),
}

impl Change {
    pub fn needs_admin(&self) -> bool {
        match self {
            // The NVIDIA profile database (ProgramData, "NVIDIA Corporation/Drs")
            // lets every account write it, as the NVIDIA app does.
            Change::DriverAppSetting(_) => false,
            Change::JsonKey(_) | Change::IniKey(_) => false,
            // Windows lets only an administrator change local groups.
            Change::LocalGroup(_) => true,
        }
    }

    /// One plain line for the card.
    pub fn summary(&self) -> String {
        match self {
            Change::DriverAppSetting(c) => format!("Set the {} driver setting \"{}\" to \"{}\" for programs that use this Java build ({}), wherever it is installed", c.vendor.name(), c.setting_name, c.to.1, c.build()),
            Change::JsonKey(c) => format!("Set \"{}\" to {} in this Minecraft's mod config", c.key, c.to),
            Change::LocalGroup(c) => format!("Add your Windows account to the group \"{}\"", c.group_name),
            Change::IniKey(c) => format!("Set {}={} in the game's settings file", c.key, c.to),
        }
    }

    /// The exact lines the "i" view shows: where, what, before and after,
    /// the calls made, the safety check, and how Undo reverses it.
    pub fn exact(&self) -> Vec<String> {
        match self {
            Change::DriverAppSetting(c) => driver_lines(c),
            Change::JsonKey(c) => json_lines(c),
            Change::LocalGroup(c) => group_lines(c),
            Change::IniKey(c) => ini_lines(c),
        }
    }
}

fn driver_lines(c: &DriverAppSetting) -> Vec<String> {
    let v = c.vendor.name();
    vec![
        format!("Where: the {v} driver's profile database (the same one the {v} Control Panel edits)"),
        format!("Program: {} (found running)", c.exe),
        format!("Reach: every program that runs on the Java build \"{}\" gets the setting, wherever that build is installed. Other launchers may bundle the same build. Other Java builds do not.", c.build()),
        format!("Driver entry: \"{}\" (the end of the path, the form NVIDIA's own profiles use)", if c.entry.is_empty() { c.exe.as_str() } else { c.entry.as_str() }),
        format!("Profile: a new profile \"{}\" holding only this program. No existing profile is changed.", c.new_profile),
        format!("Setting 0x{:08X} \"{}\": not set, driver default (Auto) → {} ({})", c.setting_id, c.setting_name, c.to.1, c.to.0),
        "Driver calls (NvAPI DRS): CreateProfile, CreateApplication, SetSetting, SaveSettings".to_owned(),
        "Check before saving: the same file in another Java build folder, and the bare file name, must not match the new profile. If either does, nothing is saved.".to_owned(),
        "No administrator prompt: the driver lets your own account save its profiles".to_owned(),
        format!("Undo: delete \"{}\". If you added to it since, only Recall's program entry and its setting (while it still has Recall's value) are removed.", c.new_profile),
    ]
}

fn json_lines(c: &JsonKey) -> Vec<String> {
    let before = c.from.as_ref().map_or("not set (the mod's default, \"minecraft:windowed\")".to_owned(), |v| v.to_string());
    vec![
        format!("File: {}", c.file),
        format!("Key \"{}\": {before} → {}", c.key, c.to),
        "Effect: the mod's \"windows:windowed\" borderless keeps Windows' compositor in charge, so other windows (Recall's island) can show over the game. The mod notes it can add slightly more input latency (Windows' own V-Sync).".to_owned(),
        "Only this file changes, only while Minecraft is closed. The mod itself is not changed; other keys in the file are kept.".to_owned(),
        "No administrator prompt: it is your own file.".to_owned(),
        "Undo: put the key back as it was (or remove it), only while it still has Recall's value.".to_owned(),
    ]
}

fn group_lines(c: &LocalGroup) -> Vec<String> {
    vec![
        format!("Group: \"{}\" ({}), the Windows group allowed to read the frame timing Windows logs", c.group_name, c.group_sid),
        format!("Account: {} (yours, nobody else's)", c.account),
        format!("How: Recall Setup runs as administrator with \"/add-perflog-user {}\" (NetLocalGroupAddMembers); Windows asks first", c.account),
        "Takes effect after you sign out of Windows and back in".to_owned(),
        format!("Undo: Recall Setup runs as administrator with \"/remove-perflog-user {}\" (NetLocalGroupDelMembers); Windows asks first, and it also takes effect at the next sign-in", c.account),
    ]
}

fn ini_lines(c: &IniKey) -> Vec<String> {
    let before = c.from.as_deref().map_or("not there (the game's default)".to_owned(), str::to_owned);
    vec![
        format!("File: {}", c.file),
        format!("Section [{}], line \"{}\": {before} → {}", c.section, c.key, c.to),
        "Effect: the game starts in its own Borderless mode, the same picture without taking the display, so Windows can show Recall's island over it. You can switch back in the game's Video settings at any time.".to_owned(),
        "Only these lines change, only while the game is closed. Every other line in the file is kept as it is.".to_owned(),
        "No administrator prompt: it is your own file.".to_owned(),
        "Undo: put the line back as it was (or remove it), only while it still has Recall's value.".to_owned(),
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

/// The FPS permission fix for this user's account.
pub fn fps_permission(account: &str) -> Plan {
    let change = LocalGroup { group_sid: "S-1-5-32-559".into(), group_name: "Performance Log Users".into(), account: account.to_owned() };
    Plan { fix: FixId::FpsPermission, steps: vec![Step { change: Change::LocalGroup(change) }] }
}

/// The Minecraft borderless fix for one instance's Cubes Without Borders config.
pub fn minecraft_borderless_cwb(change: JsonKey) -> Plan {
    Plan { fix: FixId::MinecraftBorderlessCwb, steps: vec![Step { change: Change::JsonKey(change) }] }
}

/// The Rocket League Borderless fix: the settings-file lines that differ from Borderless.
pub fn rocket_league_borderless(keys: Vec<IniKey>) -> Plan {
    Plan { fix: FixId::RocketLeagueBorderless, steps: keys.into_iter().map(|k| Step { change: Change::IniKey(k) }).collect() }
}

/// The Minecraft overlay fix for the javaw.exe Recall found running.
pub fn minecraft_overlay_nvidia(setting: DriverAppSetting) -> Plan {
    Plan { fix: FixId::MinecraftOverlayNvidia, steps: vec![Step { change: Change::DriverAppSetting(setting) }] }
}
