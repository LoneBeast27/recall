//! Which foreground windows are capture targets (split out of
//! `record_target`, file size; behaviour unchanged): the deny lists, the
//! private-app and game-evidence rules, and [`classify_event`].

use crate::config::Config;
use crate::record_target::Target;
use crate::rules;
use crate::types::RuleKind;
use crate::watcher::WatchEvent;

/// Stems that are never a capture target even when fullscreen (on top of
/// [`rules::is_denied`]): Recall's own processes, browsers, media players,
/// full-screen overlays (which would otherwise steal the target from the game
/// underneath and restart the engine), launchers, remote-desktop clients, and
/// shell surfaces. Steam's `streaming_client` (Remote Play) is deliberately
/// absent: a streamed game is recorded as a game.
const TARGET_DENY_STEMS: &[&str] = &[
    // DWM (the "Not Responding" ghost of a hung game, third audit U3), the shell host and
    // the lock screen: `never_recorded::SYSTEM_STEMS`, which no Settings entry overrides.
    "recall-native",
    "recall-hub",
    "recall-overlay",
    "nvidia overlay",
    "nvidia share",
    "gamebar",
    "gamebarftserver",
    "overwolf",
    "steam",
    "steamwebhelper",
    "epicgameslauncher",
    "riotclientux",
    "riotclientservices",
    "battle.net",
    "eadesktop",
    "galaxyclient",
    "upc",
    "uplay",
    "ubisoftconnect",
    "origin",
    "xboxapp",
    "gamingservicesui",
    "amazongames",
    "amazon games ui",
    "playnite.fullscreenapp",
    "playnite.desktopapp",
    "brave",
    "chrome",
    "chromium",
    "msedge",
    "msedgewebview2",
    "firefox",
    "librewolf",
    "waterfox",
    "opera",
    "opera_gx",
    "vivaldi",
    "arc",
    "zen",
    "iexplore",
    "vlc",
    "mpv",
    "mpc-hc",
    "mpc-hc64",
    "potplayermini64",
    "wmplayer",
    "screenclippinghost",
    "snippingtool",
    // Remote desktop / streaming clients: [`REMOTE_CLIENT_STEMS`].
    // Steam-library apps that are not games (the Steam-library rule would take them).
    "wallpaper32",
    "wallpaper64",
    "ui32",
    // Crash reporters: they ship in the game's folder (a Steam library) and
    // would end the closed game's drain, its last clip with it (T2).
    "crashreportclient",
    "unitycrashhandler32",
    "unitycrashhandler64",
    "crashpad_handler",
    "werfault",
    "werfaultsecure",
    "crashreporter",
    // Store apps (media, photos, the Store, settings, camera, mail, the Xbox
    // app): in `rules::STORE_APP_STEMS`, which `rules::is_denied` checks.
    // Call, chat, mail and office apps: `private_apps` (opt-in only).
];

/// Remote desktop, remote viewer and game-streaming clients: a fullscreen
/// session is a desktop (or another PC's game), not a game here (msrdc.exe
/// was recorded as "Remote Desktop", 2026-11-04). Never a target, and never
/// named by the G4 "add it as a game" notice (`record_composed`).
pub(crate) const REMOTE_CLIENT_STEMS: &[&str] = &[
    "mstsc",
    "msrdc",
    "msrdcw",
    "rdclient",
    "rdclient.windows",
    "windows365",
    "windowsapp",
    "rdcman",
    "vmconnect",
    "vmware-remotemks",
    "virtualboxvm",
    "mremoteng",
    "royalts",
    "remotedesktopmanager",
    "remotedesktopmanager64",
    "anydesk",
    "teamviewer",
    "rustdesk",
    "parsecd",
    "moonlight",
    "remoting_desktop",
    "remoting_host",
    "vncviewer",
    "tvnviewer",
    "nomachine",
    "nxplayer",
    "splashtop",
    "strwinclt",
    // Citrix Workspace, VMware Horizon, Amazon WorkSpaces, Shadow PC (R7-detect-3).
    "wfica32",
    "cdviewer",
    "vmware-view",
    "workspaces",
    "shadow",
    "shadowpc",
    "shadow pc",
];

/// Remote clients whose bare exe name a game may share: Steam evidence or
/// `known_games` ([`game_evidence`]) makes them a game; the G4 notice never
/// names them.
const GAME_NAMED_REMOTE_STEMS: &[&str] = &["shadow"];

/// `stem` (`.exe` optional) is a remote desktop / streaming client.
pub(crate) fn is_remote_client(stem: &str) -> bool {
    let stem = strip_exe(stem.trim());
    REMOTE_CLIENT_STEMS.iter().any(|d| d.eq_ignore_ascii_case(stem))
}

/// Recall's own processes and Windows itself: never recorded, whatever the config lists
/// (`.exe` optional). One list with the native app's "Add app…" (`never_recorded.rs`).
pub(crate) use crate::never_recorded::never_recordable;

/// Whether an exe stem is a capture target under `cfg`. `looks_like_game` =
/// fullscreen on its monitor, or the exe lives in a Steam or launcher library;
/// `game_evidence` = [`game_evidence`] (Steam or `known_games`, not a launcher
/// folder, R7-detect-1).
///
/// Order: Recall's own processes and Windows itself ([`never_recordable`]), then a
/// `none` override, then a game override (the user's own Settings entry beats the
/// browser, media-player, launcher, remote-client and private lists: the maintainer
/// 2026-11-07 "support for manual application registering"), then
/// `detect.fullscreen_games` (beats the deny lists), then the deny lists, then
/// `looks_like_game` unless the stem is a call, chat or office app
/// (`private_apps`: `fullscreen_games` or `game_evidence` = [`game_evidence`] of
/// the event opts it in).
///
/// `true` makes a window a *candidate*. A candidate with no official proof
/// (a plain fullscreen window) is recorded only on a "game" render verdict;
/// with no verdict it is not recorded (`record_hosted::check_proof`).
pub fn is_target_with(stem: &str, looks_like_game: bool, game_evidence: bool, cfg: &Config) -> bool {
    let stem = strip_exe(stem.trim());
    if never_recordable(stem) {
        return false;
    }
    match rules::override_kind(cfg, stem) {
        Some(RuleKind::None) => return false,
        Some(_) => return true,
        None => {}
    }
    if looks_like_game && is_fullscreen_game(cfg, stem) {
        return true;
    }
    let remote = is_remote_client(stem) && !(game_evidence && GAME_NAMED_REMOTE_STEMS.iter().any(|d| d.eq_ignore_ascii_case(stem)));
    if rules::is_denied(stem) || TARGET_DENY_STEMS.iter().any(|d| d.eq_ignore_ascii_case(stem)) || remote {
        return false;
    }
    looks_like_game && !crate::private_apps::blocks_recording(stem, game_evidence)
}

/// [`is_target_with`] with no game evidence (tests).
#[cfg(test)]
pub fn is_target(stem: &str, looks_like_game: bool, cfg: &Config) -> bool {
    is_target_with(stem, looks_like_game, false, cfg)
}

/// `stem` is in `detect.fullscreen_games` (case-insensitive, `.exe` optional):
/// its fullscreen windows are games with no render check.
pub fn is_fullscreen_game(cfg: &Config, stem: &str) -> bool {
    let stem = strip_exe(stem.trim());
    !stem.is_empty() && cfg.detect.fullscreen_games.iter().any(|g| strip_exe(g.trim()).eq_ignore_ascii_case(stem))
}

/// `"msrdc.exe"` -> `"msrdc"` (case-insensitive); a bare stem is unchanged.
fn strip_exe(s: &str) -> &str {
    match s.len().checked_sub(4) {
        Some(i) if s.is_char_boundary(i) && s[i..].eq_ignore_ascii_case(".exe") => &s[..i],
        _ => s,
    }
}

/// A candidate that commits on the 300 ms rule with no render check: a
/// Steam- or launcher-library exe, an Xbox / Store game package (fullscreen:
/// only then is it a candidate), an override, a `detect.fullscreen_games`
/// entry, or a game Recall knows (`known_games`). Asked only after
/// [`classify_event`] made it a candidate, so the deny lists, the private
/// list and `none` overrides already ran.
pub fn skips_render_check(ev: &WatchEvent, cfg: &Config) -> bool {
    official_install(ev)
        || rules::override_kind(cfg, &ev.exe_stem).is_some()
        || is_fullscreen_game(cfg, &ev.exe_stem)
        || crate::known_games::is_known_window(ev)
}

/// `Some(target)` if this foreground event is a capture target. A known game
/// is one windowed too (G10; an override already is); a screensaver
/// (`.scr`) never is (G11).
pub fn classify_event(ev: &WatchEvent, cfg: &Config) -> Option<Target> {
    if is_screensaver(&ev.exe_name) || crate::known_games::is_minecraft_server(&ev.exe_stem, &ev.title) {
        return None;
    }
    let evidence = game_evidence(ev);
    is_target_with(&ev.exe_stem, ev.fullscreen || evidence || ev.launcher_game, evidence, cfg).then(|| Target::from_event(ev))
}

/// Evidence that beats the private list: the exe lives in a Steam library
/// (`WatchEvent::steam_game`) or Recall knows the game (R5-detect-2). A
/// launcher folder is not (R7-detect-1): it makes a windowed exe a candidate
/// ([`in_library`]) but a chat app there stays private without an opt-in.
pub fn game_evidence(ev: &WatchEvent) -> bool {
    ev.steam_game || crate::known_games::is_known_window(ev)
}

/// The exe lives in a Steam or another launcher's game library.
pub fn in_library(ev: &WatchEvent) -> bool {
    ev.steam_game || ev.launcher_game
}

/// [`in_library`], or an Xbox / Store game package: an install that proves a
/// fullscreen window is a game (official-proof ruling, 2026-11-07).
pub fn official_install(ev: &WatchEvent) -> bool {
    in_library(ev) || ev.store_game
}

/// The foreground could become a target by going fullscreen (the follow
/// route, `record_detect`): not denied, no `none` override, and not held
/// back by the private list unless [`game_evidence`] says it is a game.
pub fn followable(ev: &WatchEvent, cfg: &Config) -> bool {
    is_target_with(&ev.exe_stem, true, game_evidence(ev), cfg)
}

/// The image is a screensaver (`*.scr`, any case).
fn is_screensaver(exe_name: &str) -> bool {
    let name = exe_name.trim();
    match name.len().checked_sub(4) {
        Some(i) => name.is_char_boundary(i) && name[i..].eq_ignore_ascii_case(".scr"),
        None => false,
    }
}
