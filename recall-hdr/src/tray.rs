//! Tray icon + context menu (tray-icon 0.24 / muda 0.19): a status header, manual
//! HDR toggle, auto on/off, focused-app rule actions, a clamp default radio, and
//! an autostart toggle -- exposed as an event-producing component with NO display
//! logic inside. The app layer owns the message pump, constructs a [`Tray`], drains
//! [`Tray::poll`] each loop iteration, and pushes state back via [`Tray::set_status`]
//! / [`Tray::set_checks`].
//!
//! # Contract with the app layer
//! - [`Tray::build`] MUST be called on the Win32 message-pump thread. tray-icon /
//!   muda create a hidden window and popup `HMENU` bound to that thread; the context
//!   menu is only tracked while that loop pumps messages. `build` returns
//!   `Err(String)` (never panics) if the OS refuses the tray/menu.
//! - Menu clicks arrive on muda's **process-global** [`MenuEvent::receiver`] and
//!   tray-icon's [`TrayIconEvent::receiver`]. [`Tray::poll`] drains both and maps
//!   menu-item ids -> [`TrayCmd`]. Left/right clicks on the icon are handled natively
//!   (the menu shows on click) and are not surfaced as commands.
//!
//! # 0.24 / 0.19 API notes (differs from the 0.19/0.6-era design assumptions)
//! - The context menu is `muda` re-exported as `tray_icon::menu`. Items are built
//!   with explicit string ids via `*::with_id(...)` (`MenuItem`, `CheckMenuItem`,
//!   `Submenu`), which is what makes the id->command mapping deterministic and
//!   unit-testable. `MenuId` is `pub struct MenuId(pub String)` and compares against
//!   `&str`, so [`poll`](Tray::poll) matches on the stored id strings.
//! - There is no built-in radio-group primitive; the "Clamp" radio is three
//!   `CheckMenuItem`s and we enforce single-selection ourselves in [`set_checks`].
//! - The icon comes from `tray_icon::Icon::from_rgba(rgba, w, h)` -- a solid-color
//!   32bpp buffer generated in code (no asset files).
//! - `TrayIconBuilder::with_menu` takes `Box<dyn ContextMenu>`; the built `TrayIcon`
//!   and the `Menu` must be kept alive for the tray to persist (both are `Rc`-backed,
//!   so `Tray` owns them).

use tray_icon::menu::{CheckMenuItem, ContextMenu, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::types::{ClampMode, RuleKind};

/// A command produced by a tray menu interaction. The app layer maps these onto
/// reconciler / config / process actions; the tray itself performs none of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCmd {
    /// "Toggle HDR now" -- flip Windows HDR on the target display immediately.
    ToggleHdrNow,
    /// "Auto HDR/SDR by game" checkbox toggled to this value.
    SetAutoEnabled(bool),
    /// One of the "Focused app rule" submenu picks (HDR game / SDR game / Ignore).
    FocusedRule(RuleKind),
    /// "Forget" the focused app's per-exe override.
    ForgetFocusedRule,
    /// A "Clamp (SDR games)" radio pick.
    SetDefaultClamp(ClampMode),
    /// "Start with Windows" checkbox toggled to this value.
    SetAutostart(bool),
    /// Open the config TOML in the user's editor.
    OpenConfig,
    /// Open the log file.
    OpenLog,
    /// "Restore display now" -- reconcile the display back to baseline.
    RestoreNow,
    /// Quit the app.
    Quit,
}

// ---------------------------------------------------------------------------
// Menu-item ids (the single source of truth for the id <-> command mapping)
// ---------------------------------------------------------------------------
//
// Stored as explicit strings so `build` and `poll` agree and the mapping is
// unit-testable without a live tray. Keep these in sync with `map_id` and
// `all_command_ids` (the test asserts completeness against `all_command_ids`).

const ID_STATUS: &str = "status"; // disabled header, never a command
const ID_TOGGLE_HDR: &str = "toggle_hdr_now";
const ID_AUTO: &str = "auto_enabled";
const ID_RULE_HDR: &str = "focused_rule_hdr";
const ID_RULE_SDR: &str = "focused_rule_sdr";
const ID_RULE_IGNORE: &str = "focused_rule_ignore";
const ID_RULE_FORGET: &str = "focused_rule_forget";
const ID_CLAMP_PUNCHY: &str = "clamp_punchy";
const ID_CLAMP_ACCURATE: &str = "clamp_accurate";
const ID_CLAMP_OFF: &str = "clamp_off";
const ID_AUTOSTART: &str = "autostart";
const ID_OPEN_CONFIG: &str = "open_config";
const ID_OPEN_LOG: &str = "open_log";
const ID_RESTORE: &str = "restore_now";
const ID_QUIT: &str = "quit";

/// Map a clicked menu id string to its [`TrayCmd`], if any.
///
/// The checkbox commands (`auto`, `autostart`) carry the *new* desired value. muda
/// flips a `CheckMenuItem`'s checked state on click *before* the event fires, so the
/// live `is_checked()` is the post-click value -- but `poll` reads the current check
/// state to build these, keeping the source of truth in the menu item itself.
fn map_id(id: &str, auto_now: bool, autostart_now: bool) -> Option<TrayCmd> {
    Some(match id {
        ID_TOGGLE_HDR => TrayCmd::ToggleHdrNow,
        ID_AUTO => TrayCmd::SetAutoEnabled(auto_now),
        ID_RULE_HDR => TrayCmd::FocusedRule(RuleKind::HdrGame),
        ID_RULE_SDR => TrayCmd::FocusedRule(RuleKind::SdrGame),
        ID_RULE_IGNORE => TrayCmd::FocusedRule(RuleKind::None),
        ID_RULE_FORGET => TrayCmd::ForgetFocusedRule,
        ID_CLAMP_PUNCHY => TrayCmd::SetDefaultClamp(ClampMode::Punchy),
        ID_CLAMP_ACCURATE => TrayCmd::SetDefaultClamp(ClampMode::Accurate),
        ID_CLAMP_OFF => TrayCmd::SetDefaultClamp(ClampMode::Off),
        ID_AUTOSTART => TrayCmd::SetAutostart(autostart_now),
        ID_OPEN_CONFIG => TrayCmd::OpenConfig,
        ID_OPEN_LOG => TrayCmd::OpenLog,
        ID_RESTORE => TrayCmd::RestoreNow,
        ID_QUIT => TrayCmd::Quit,
        // ID_STATUS is a disabled header; anything else is not ours.
        _ => return None,
    })
}

/// Every menu id that maps to a command (drives the completeness test).
#[cfg(test)]
const ALL_COMMAND_IDS: &[&str] = &[
    ID_TOGGLE_HDR,
    ID_AUTO,
    ID_RULE_HDR,
    ID_RULE_SDR,
    ID_RULE_IGNORE,
    ID_RULE_FORGET,
    ID_CLAMP_PUNCHY,
    ID_CLAMP_ACCURATE,
    ID_CLAMP_OFF,
    ID_AUTOSTART,
    ID_OPEN_CONFIG,
    ID_OPEN_LOG,
    ID_RESTORE,
    ID_QUIT,
];

// ---------------------------------------------------------------------------
// Tray
// ---------------------------------------------------------------------------

/// The tray icon + menu. Owns the `TrayIcon` and `Menu` (keeping them alive) plus
/// handles to the mutable items so the app can reflect state in ([`set_status`],
/// [`set_checks`]).
pub struct Tray {
    /// Kept alive so the tray icon persists; also the drop point that removes it.
    _tray: TrayIcon,
    /// Kept alive so the context menu (and its `HMENU`) persists.
    _menu: Menu,

    // Mutable item handles for state reflection.
    status: MenuItem,
    auto: CheckMenuItem,
    autostart: CheckMenuItem,
    clamp_punchy: CheckMenuItem,
    clamp_accurate: CheckMenuItem,
    clamp_off: CheckMenuItem,
}

impl Tray {
    /// Build the tray icon + menu from the current config. MUST be called on the
    /// message-pump thread. Returns `Err(String)` (never panics) if the tray/menu
    /// cannot be created.
    pub fn build(cfg: &crate::config::Config) -> Result<Tray, String> {
        // `auto` is the app-level master switch (auto-HDR/SDR-by-game). There is no
        // dedicated config field for it in the frozen schema, so it defaults ON;
        // the app layer can reflect a persisted value via `set_checks` after build.
        let auto_on = true;
        let autostart_on = cfg.startup.autostart;
        let clamp = cfg.behavior.default_clamp;

        // --- items -------------------------------------------------------------
        let status = MenuItem::with_id(ID_STATUS, "recall-hdr: (starting)", false, None);

        let toggle = MenuItem::with_id(ID_TOGGLE_HDR, "Toggle HDR now", true, None);
        let auto = CheckMenuItem::with_id(ID_AUTO, "Auto HDR/SDR by game", true, auto_on, None);

        let rule_hdr = MenuItem::with_id(ID_RULE_HDR, "Treat as HDR game", true, None);
        let rule_sdr = MenuItem::with_id(ID_RULE_SDR, "Treat as SDR game", true, None);
        let rule_ignore = MenuItem::with_id(ID_RULE_IGNORE, "Ignore this app", true, None);
        let rule_forget = MenuItem::with_id(ID_RULE_FORGET, "Forget rule", true, None);
        let rule_submenu = Submenu::with_items(
            "Focused app rule",
            true,
            &[&rule_hdr, &rule_sdr, &rule_ignore, &PredefinedMenuItem::separator(), &rule_forget],
        )
        .map_err(|e| format!("build focused-rule submenu: {e}"))?;

        let clamp_punchy = CheckMenuItem::with_id(
            ID_CLAMP_PUNCHY,
            "Punchy (native gamut)",
            true,
            clamp == ClampMode::Punchy,
            None,
        );
        let clamp_accurate = CheckMenuItem::with_id(
            ID_CLAMP_ACCURATE,
            "Accurate (sRGB clamp)",
            true,
            clamp == ClampMode::Accurate,
            None,
        );
        let clamp_off = CheckMenuItem::with_id(
            ID_CLAMP_OFF,
            "Off (don't manage clamp)",
            true,
            clamp == ClampMode::Off,
            None,
        );
        let clamp_submenu = Submenu::with_items(
            "Clamp (SDR games)",
            true,
            &[&clamp_punchy, &clamp_accurate, &clamp_off],
        )
        .map_err(|e| format!("build clamp submenu: {e}"))?;

        let autostart = CheckMenuItem::with_id(ID_AUTOSTART, "Start with Windows", true, autostart_on, None);

        let open_config = MenuItem::with_id(ID_OPEN_CONFIG, "Open config file", true, None);
        let open_log = MenuItem::with_id(ID_OPEN_LOG, "Open log", true, None);
        let restore = MenuItem::with_id(ID_RESTORE, "Restore display now", true, None);
        let quit = MenuItem::with_id(ID_QUIT, "Quit", true, None);

        // --- menu (design-frozen order) ---------------------------------------
        let menu = Menu::with_items(&[
            &status,
            &PredefinedMenuItem::separator(),
            &toggle,
            &auto,
            &PredefinedMenuItem::separator(),
            &rule_submenu,
            &clamp_submenu,
            &PredefinedMenuItem::separator(),
            &autostart,
            &PredefinedMenuItem::separator(),
            &open_config,
            &open_log,
            &restore,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .map_err(|e| format!("build tray menu: {e}"))?;

        // --- icon + tray ------------------------------------------------------
        let (rgba, w, h) = tray_icon_rgba();
        let icon = Icon::from_rgba(rgba, w, h).map_err(|e| format!("build tray icon: {e}"))?;

        let tray = TrayIconBuilder::new()
            .with_tooltip("recall-hdr")
            .with_menu(Box::new(menu.clone()) as Box<dyn ContextMenu>)
            .with_icon(icon)
            .build()
            .map_err(|e| format!("build tray icon: {e}"))?;

        Ok(Tray {
            _tray: tray,
            _menu: menu,
            status,
            auto,
            autostart,
            clamp_punchy,
            clamp_accurate,
            clamp_off,
        })
    }

    /// Drain the menu + tray-icon event channels and return the mapped commands in
    /// arrival order. Non-blocking. Tray-icon click events are consumed but not
    /// surfaced (the menu shows natively on click).
    pub fn poll(&self) -> Vec<TrayCmd> {
        // Read the post-click check state so the checkbox commands carry the new
        // value muda already toggled onto the item.
        let auto_now = self.auto.is_checked();
        let autostart_now = self.autostart.is_checked();

        let mut cmds = Vec::new();
        let menu_rx = MenuEvent::receiver();
        while let Ok(event) = menu_rx.try_recv() {
            if let Some(cmd) = map_id(event.id.as_ref(), auto_now, autostart_now) {
                cmds.push(cmd);
            }
        }

        // Drain tray icon click/hover events so the channel never backs up. We don't
        // surface them (the context menu is shown natively on click).
        let tray_rx = TrayIconEvent::receiver();
        while tray_rx.try_recv().is_ok() {}

        cmds
    }

    /// Update the disabled status header (e.g. `"Monitor 27Q: SDR · desktop"`).
    pub fn set_status(&mut self, line: &str) {
        self.status.set_text(line);
    }

    /// Reflect app state into the checkable / radio items: the auto master switch,
    /// the autostart toggle, and single-selection on the clamp radio group.
    pub fn set_checks(&mut self, auto: bool, autostart: bool, clamp: ClampMode) {
        self.auto.set_checked(auto);
        self.autostart.set_checked(autostart);
        // Enforce single-selection on the (hand-rolled) clamp radio group.
        self.clamp_punchy.set_checked(clamp == ClampMode::Punchy);
        self.clamp_accurate.set_checked(clamp == ClampMode::Accurate);
        self.clamp_off.set_checked(clamp == ClampMode::Off);
    }
}

// ---------------------------------------------------------------------------
// Icon generation (no asset files)
// ---------------------------------------------------------------------------

/// Tray icon edge length in pixels. 32x32 is the natural notification-area size on
/// modern Windows (it downscales cleanly to 16x16 on lower-DPI shells).
const ICON_SIZE: u32 = 32;

/// A solid teal RGBA icon buffer, generated in code. Returns `(rgba, width, height)`
/// where `rgba.len() == (width * height * 4)`, opaque (`a = 255`).
///
/// Teal (`#26A69A`) reads as a neutral "display/color" accent and is distinct from
/// the common red/green tray dots so the icon is findable at a glance.
fn tray_icon_rgba() -> (Vec<u8>, u32, u32) {
    let (r, g, b, a) = (0x26u8, 0xA6u8, 0x9Au8, 0xFFu8);
    let px = (ICON_SIZE * ICON_SIZE) as usize;
    let mut rgba = Vec::with_capacity(px * 4);
    for _ in 0..px {
        rgba.extend_from_slice(&[r, g, b, a]);
    }
    (rgba, ICON_SIZE, ICON_SIZE)
}

// ---------------------------------------------------------------------------
// Open helpers (kept here per the file-ownership constraint)
// ---------------------------------------------------------------------------
//
// The app layer calls these when it receives `OpenConfig` / `OpenLog`. They live
// here (not in a shared util) because tray.rs is the only file this agent owns.

/// Open a file with the shell's default handler (`cmd /c start "" <path>`).
///
/// Uses an empty title arg (`""`) so a path with spaces isn't mistaken for the
/// window title by `start`. Best-effort: returns the spawn error as a string.
pub fn open_path(path: &std::path::Path) -> Result<(), String> {
    use std::{os::windows::process::CommandExt, process::Command};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    Command::new("cmd")
        .args(["/C", "start", "", &path.to_string_lossy()])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("open {}: {e}", path.display()))
}

/// Reveal a file in Explorer with it selected (`explorer /select,<path>`).
pub fn reveal_in_explorer(path: &std::path::Path) -> Result<(), String> {
    use std::process::Command;
    Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("reveal {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_rgba_has_correct_byte_length() {
        let (rgba, w, h) = tray_icon_rgba();
        assert_eq!(w, ICON_SIZE);
        assert_eq!(h, ICON_SIZE);
        assert_eq!(
            rgba.len(),
            (w * h * 4) as usize,
            "RGBA buffer must be width*height*4 bytes"
        );
        // Divisible-by-4 is what Icon::from_rgba requires.
        assert_eq!(rgba.len() % 4, 0);
    }

    #[test]
    fn icon_rgba_is_fully_opaque() {
        let (rgba, _, _) = tray_icon_rgba();
        // Every 4th byte is alpha; all must be 0xFF (opaque) so the icon is visible.
        for (i, chunk) in rgba.chunks_exact(4).enumerate() {
            assert_eq!(chunk[3], 0xFF, "pixel {i} alpha must be opaque");
        }
    }

    #[test]
    fn every_command_id_maps() {
        // Completeness: every id in ALL_COMMAND_IDS must produce a command, and the
        // command must be the right variant for that id.
        for id in ALL_COMMAND_IDS {
            let cmd = map_id(id, false, false);
            assert!(cmd.is_some(), "menu id '{id}' has no TrayCmd mapping");
        }
    }

    #[test]
    fn map_id_variants_are_correct() {
        assert_eq!(map_id(ID_TOGGLE_HDR, false, false), Some(TrayCmd::ToggleHdrNow));
        assert_eq!(map_id(ID_RULE_HDR, false, false), Some(TrayCmd::FocusedRule(RuleKind::HdrGame)));
        assert_eq!(map_id(ID_RULE_SDR, false, false), Some(TrayCmd::FocusedRule(RuleKind::SdrGame)));
        assert_eq!(map_id(ID_RULE_IGNORE, false, false), Some(TrayCmd::FocusedRule(RuleKind::None)));
        assert_eq!(map_id(ID_RULE_FORGET, false, false), Some(TrayCmd::ForgetFocusedRule));
        assert_eq!(map_id(ID_CLAMP_PUNCHY, false, false), Some(TrayCmd::SetDefaultClamp(ClampMode::Punchy)));
        assert_eq!(map_id(ID_CLAMP_ACCURATE, false, false), Some(TrayCmd::SetDefaultClamp(ClampMode::Accurate)));
        assert_eq!(map_id(ID_CLAMP_OFF, false, false), Some(TrayCmd::SetDefaultClamp(ClampMode::Off)));
        assert_eq!(map_id(ID_RESTORE, false, false), Some(TrayCmd::RestoreNow));
        assert_eq!(map_id(ID_QUIT, false, false), Some(TrayCmd::Quit));
        assert_eq!(map_id(ID_OPEN_CONFIG, false, false), Some(TrayCmd::OpenConfig));
        assert_eq!(map_id(ID_OPEN_LOG, false, false), Some(TrayCmd::OpenLog));
    }

    #[test]
    fn checkbox_commands_carry_the_current_check_value() {
        // `auto`/`autostart` reflect the post-click check state passed in.
        assert_eq!(map_id(ID_AUTO, true, false), Some(TrayCmd::SetAutoEnabled(true)));
        assert_eq!(map_id(ID_AUTO, false, false), Some(TrayCmd::SetAutoEnabled(false)));
        assert_eq!(map_id(ID_AUTOSTART, false, true), Some(TrayCmd::SetAutostart(true)));
        assert_eq!(map_id(ID_AUTOSTART, false, false), Some(TrayCmd::SetAutostart(false)));
    }

    #[test]
    fn status_header_and_unknown_ids_do_not_map() {
        assert_eq!(map_id(ID_STATUS, false, false), None, "status header is not a command");
        assert_eq!(map_id("bogus-id", false, false), None, "unknown id is not a command");
        assert_eq!(map_id("", false, false), None);
    }

    #[test]
    fn command_ids_are_unique() {
        // A duplicate id would make two menu items indistinguishable in poll().
        let mut seen = std::collections::HashSet::new();
        for id in ALL_COMMAND_IDS {
            assert!(seen.insert(*id), "duplicate command id '{id}'");
        }
    }
}
