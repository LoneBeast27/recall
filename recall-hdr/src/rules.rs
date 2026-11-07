//! Rule engine: classify a foreground (exe stem, title, fullscreen?) into an
//! [`AppKind`] using config overrides + defaults, with the vendored DENY_STEMS.
//!
//! Decision order (design-frozen, first match wins):
//!
//! | # | Condition                                   | Result                         |
//! |---|---------------------------------------------|--------------------------------|
//! | 1 | empty / whitespace stem                     | `NonGame`                      |
//! | 2 | stem in [`DENY_STEMS`] (shells, us, tools)  | `NonGame` (overrides all)      |
//! | 3 | a `[[overrides]]` entry matches the stem    | map `RuleKind` -> `AppKind`    |
//! | 4 | no override, `fullscreen_on_target`         | `default_game_class` mapped    |
//! | 5 | no override, windowed                        | `NonGame` (conservative)       |
//!
//! The deny-list (#2) intentionally beats a config override (#3): a shell/capture
//! tool must never be driven into a game rule even if a user mis-lists it. An
//! override (#3) beats the fullscreen default (#4/#5) so an explicit rule fires
//! even for a windowed app. A *windowed unknown* is never auto-classed as a game
//! (#5): only a fullscreen-on-target window earns the default game class.

use crate::config::Config;
use crate::types::{AppKind, RuleKind};

/// Exe stems we never treat as a game: shells, capture tools, terminals, IDEs,
/// and recall itself. Matching is case-insensitive.
///
/// vendored from recall/src/source.rs (DENY_STEMS) -- kept in sync by hand; the
/// display-manager adds "recall-hdr" (this binary) so it can never classify its
/// own tray/window as a game.
const DENY_STEMS: &[&str] = &[
    "recall",
    "recall-hdr",
    "explorer",
    "obs64",
    "obs32",
    "obs",
    "discord",
    "applicationframehost",
    "shellexperiencehost",
    "searchhost",
    "startmenuexperiencehost",
    "textinputhost",
    "windowsterminal",
    "cmd",
    "conhost",
    "powershell",
    "pwsh",
    "code",
    "devenv",
];

/// Store and media apps that go fullscreen without being games: with G6 a UWP
/// frame is named by its hosted exe, no longer the denied
/// `applicationframehost` (R2-5, merge gate P2). Photos is a Win32 app.
/// Checked here (HDR rules) and in `record_target::is_target` (recording).
pub(crate) const STORE_APP_STEMS: &[&str] = &[
    "video.ui",
    "music.ui",
    "microsoft.media.player",
    "microsoft.photos",
    "photos",
    "winstore.app",
    "store",
    "systemsettings",
    "netflix",
    "wwahost",
    "hxoutlook",
    "hxcalendarappimm",
    "windowscamera",
    "xboxpcapp",
    "xboxpcappce",
];

/// Everything the watcher knows about a foreground change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Foreground {
    /// Lowercase file stem, e.g. `"hdr-testcard"`.
    pub exe_stem: String,
    /// Window title (informational; not used for classification today).
    pub title: String,
    /// Borderless/fullscreen on the *managed* monitor.
    pub fullscreen_on_target: bool,
    /// Owning process id.
    pub pid: u32,
}

/// Map a matched/defaulted [`RuleKind`] to the [`AppKind`] the reconciler acts on.
///
/// `None` means "no rule / leave the user's baseline", which is exactly the
/// `NonGame` case for the reconciler.
fn rule_to_app(kind: RuleKind) -> AppKind {
    match kind {
        RuleKind::HdrGame => AppKind::HdrGame,
        RuleKind::SdrGame => AppKind::SdrGame,
        RuleKind::None => AppKind::NonGame,
    }
}

/// True if `stem` is on the deny-list (case-insensitive).
pub(crate) fn is_denied(stem: &str) -> bool {
    DENY_STEMS
        .iter()
        .chain(STORE_APP_STEMS)
        .any(|d| d.eq_ignore_ascii_case(stem))
}

/// Return the explicit override [`RuleKind`] for `stem`, if a `[[overrides]]`
/// entry matches (case-insensitive). `None` means "no entry" -- distinct from an
/// entry whose kind is `RuleKind::None`, which is `Some(RuleKind::None)`.
///
/// We look at `cfg.overrides` directly rather than [`Config::rule_for`] because
/// `rule_for` folds a miss into `default_game_class`, which would make a windowed
/// unknown indistinguishable from an explicit-`none` override -- and the decision
/// table needs to tell those apart (#3 vs #5).
pub(crate) fn override_kind(cfg: &Config, stem: &str) -> Option<RuleKind> {
    cfg.overrides
        .iter()
        .find(|o| same_exe(&o.exe, stem))
        .map(|o| o.kind)
}

/// `a` and `b` name the same exe: case-insensitive, `.exe` optional on
/// either side (`"Game.exe"` = `"game"`, G9).
pub(crate) fn same_exe(a: &str, b: &str) -> bool {
    strip_exe_suffix(a.trim()).eq_ignore_ascii_case(strip_exe_suffix(b.trim()))
}

fn strip_exe_suffix(s: &str) -> &str {
    match s.len().checked_sub(4) {
        Some(i) if s.is_char_boundary(i) && s[i..].eq_ignore_ascii_case(".exe") => &s[..i],
        _ => s,
    }
}

/// Classify a foreground into what the reconciler should treat it as.
///
/// See the module-level decision table. Stems arrive already-lowercased from the
/// watcher, but every comparison here is case-insensitive so a stray-case stem
/// (or a hand-built `Foreground` in a test) still classifies correctly.
pub fn classify(fg: &Foreground, cfg: &Config) -> AppKind {
    let stem = fg.exe_stem.trim();

    // 1. Empty / unknown stem -> baseline.
    if stem.is_empty() {
        return AppKind::NonGame;
    }

    // 2. Deny-list beats everything, including a config override.
    if is_denied(stem) {
        return AppKind::NonGame;
    }

    // 3. Explicit per-exe override wins over the fullscreen default.
    if let Some(kind) = override_kind(cfg, stem) {
        return rule_to_app(kind);
    }

    // 4/5. No override: only a fullscreen-on-target window can be a game.
    if fg.fullscreen_on_target {
        rule_to_app(cfg.behavior.default_game_class)
    } else {
        AppKind::NonGame
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BehaviorCfg, Config, Override};
    use crate::types::{ClampMode, RuleKind};

    /// Build a `Foreground` with the given stem + fullscreen flag; title/pid are
    /// fixed since classification ignores them today.
    fn fg(stem: &str, fullscreen: bool) -> Foreground {
        Foreground {
            exe_stem: stem.to_string(),
            title: "some window".to_string(),
            fullscreen_on_target: fullscreen,
            pid: 1234,
        }
    }

    /// A config whose default game class is `default_kind`, no extra overrides
    /// beyond whatever the caller pushes (starts from `Config::default`, which
    /// ships the `hdr-testcard` override).
    fn cfg_with_default(default_kind: RuleKind) -> Config {
        Config {
            behavior: BehaviorCfg {
                default_game_class: default_kind,
                default_clamp: ClampMode::Punchy,
                sdr_white_nits: 220,
                debounce_ms: 400,
                hdr_auto: false,
            },
            ..Config::default()
        }
    }

    // --- #1 empty / whitespace stem ---------------------------------------

    #[test]
    fn empty_stem_is_nongame() {
        let cfg = cfg_with_default(RuleKind::HdrGame);
        assert_eq!(classify(&fg("", true), &cfg), AppKind::NonGame);
    }

    #[test]
    fn whitespace_stem_is_nongame() {
        let cfg = cfg_with_default(RuleKind::HdrGame);
        assert_eq!(classify(&fg("   ", true), &cfg), AppKind::NonGame);
    }

    // --- #2 deny-list beats everything ------------------------------------

    #[test]
    fn denied_stem_is_nongame_even_when_fullscreen() {
        let cfg = cfg_with_default(RuleKind::HdrGame);
        assert_eq!(classify(&fg("explorer", true), &cfg), AppKind::NonGame);
    }

    #[test]
    fn recall_hdr_itself_is_denied() {
        // The display manager must never classify its own window as a game.
        let cfg = cfg_with_default(RuleKind::HdrGame);
        assert_eq!(classify(&fg("recall-hdr", true), &cfg), AppKind::NonGame);
        assert_eq!(classify(&fg("recall", true), &cfg), AppKind::NonGame);
    }

    #[test]
    fn deny_list_beats_a_config_override() {
        // Even if a user mis-lists a shell as an hdr_game override, deny wins.
        let mut cfg = cfg_with_default(RuleKind::SdrGame);
        cfg.overrides.push(Override {
            exe: "obs64".into(),
            kind: RuleKind::HdrGame,
            clamp: None,
        });
        assert_eq!(classify(&fg("obs64", true), &cfg), AppKind::NonGame);
    }

    #[test]
    fn fullscreen_store_media_apps_are_not_hdr_games() {
        let cfg = cfg_with_default(RuleKind::HdrGame);
        for stem in ["Netflix", "Video.UI", "Microsoft.Media.Player", "Photos", "WindowsCamera", "HxCalendarAppImm"] {
            assert_eq!(classify(&fg(stem, true), &cfg), AppKind::NonGame, "{stem}");
        }
        assert_eq!(classify(&fg("minecraft.windows", true), &cfg), AppKind::HdrGame, "a Store game still is");
    }

    #[test]
    fn deny_match_is_case_insensitive() {
        // Stems arrive lowercase, but be defensive against stray case.
        let cfg = cfg_with_default(RuleKind::HdrGame);
        assert_eq!(classify(&fg("EXPLORER", true), &cfg), AppKind::NonGame);
        assert_eq!(classify(&fg("Recall-HDR", true), &cfg), AppKind::NonGame);
    }

    // --- #3 override beats fullscreen default -----------------------------

    #[test]
    fn override_beats_fullscreen_default_hdr() {
        // Default class is SdrGame, but the override says HdrGame -> HdrGame.
        let mut cfg = cfg_with_default(RuleKind::SdrGame);
        cfg.overrides.push(Override {
            exe: "witcher3".into(),
            kind: RuleKind::HdrGame,
            clamp: None,
        });
        assert_eq!(classify(&fg("witcher3", true), &cfg), AppKind::HdrGame);
    }

    #[test]
    fn override_applies_even_when_windowed() {
        // An explicit override fires regardless of the fullscreen flag (#3 sits
        // above #4/#5): a windowed app with an override is still classified.
        let mut cfg = cfg_with_default(RuleKind::HdrGame);
        cfg.overrides.push(Override {
            exe: "doom".into(),
            kind: RuleKind::SdrGame,
            clamp: None,
        });
        assert_eq!(classify(&fg("doom", false), &cfg), AppKind::SdrGame);
    }

    #[test]
    fn override_kind_none_is_nongame() {
        // A `kind = "none"` override forces baseline even for a fullscreen app.
        let mut cfg = cfg_with_default(RuleKind::HdrGame);
        cfg.overrides.push(Override {
            exe: "spotify".into(),
            kind: RuleKind::None,
            clamp: None,
        });
        assert_eq!(classify(&fg("spotify", true), &cfg), AppKind::NonGame);
    }

    #[test]
    fn override_match_is_case_insensitive() {
        let cfg = Config::default(); // ships hdr-testcard -> HdrGame
        assert_eq!(classify(&fg("HDR-TestCard", false), &cfg), AppKind::HdrGame);
    }

    // --- #4 fullscreen unknown -> default class ---------------------------

    #[test]
    fn fullscreen_unknown_uses_default_hdr() {
        let cfg = cfg_with_default(RuleKind::HdrGame);
        assert_eq!(classify(&fg("unlisted-game", true), &cfg), AppKind::HdrGame);
    }

    #[test]
    fn fullscreen_unknown_uses_default_sdr() {
        let cfg = cfg_with_default(RuleKind::SdrGame);
        assert_eq!(classify(&fg("unlisted-game", true), &cfg), AppKind::SdrGame);
    }

    #[test]
    fn fullscreen_unknown_with_default_none_is_nongame() {
        // If the user sets default_game_class = none, even a fullscreen unknown
        // stays baseline.
        let cfg = cfg_with_default(RuleKind::None);
        assert_eq!(classify(&fg("unlisted-game", true), &cfg), AppKind::NonGame);
    }

    // --- #5 windowed unknown -> NonGame (conservative) --------------------

    #[test]
    fn windowed_unknown_is_never_a_game() {
        // The core conservative invariant: a windowed unknown app is NonGame
        // regardless of the default game class.
        let cfg = cfg_with_default(RuleKind::HdrGame);
        assert_eq!(classify(&fg("unlisted-app", false), &cfg), AppKind::NonGame);
    }

    // --- default-config end-to-end ---------------------------------------

    #[test]
    fn default_config_hdr_testcard_resolves_to_hdrgame() {
        // The `hdr-testcard` default override in Config::default resolves to
        // HdrGame -- even windowed, because an override fires before the
        // fullscreen gate.
        let cfg = Config::default();
        assert_eq!(classify(&fg("hdr-testcard", false), &cfg), AppKind::HdrGame);
        assert_eq!(classify(&fg("hdr-testcard", true), &cfg), AppKind::HdrGame);
    }

    // --- helper units -----------------------------------------------------

    #[test]
    fn rule_to_app_maps_all_variants() {
        assert_eq!(rule_to_app(RuleKind::HdrGame), AppKind::HdrGame);
        assert_eq!(rule_to_app(RuleKind::SdrGame), AppKind::SdrGame);
        assert_eq!(rule_to_app(RuleKind::None), AppKind::NonGame);
    }

    #[test]
    fn override_kind_distinguishes_miss_from_explicit_none() {
        let mut cfg = cfg_with_default(RuleKind::HdrGame);
        cfg.overrides.push(Override {
            exe: "x".into(),
            kind: RuleKind::None,
            clamp: None,
        });
        assert_eq!(override_kind(&cfg, "x"), Some(RuleKind::None));
        assert_eq!(override_kind(&cfg, "not-listed"), None);
    }

    #[test]
    fn override_with_exe_suffix_matches() {
        // G9: `exe = "Game.exe"` is the form `detect.fullscreen_games` accepts.
        let mut cfg = cfg_with_default(RuleKind::HdrGame);
        cfg.overrides.push(Override { exe: "Game.exe".into(), kind: RuleKind::None, clamp: None });
        assert_eq!(override_kind(&cfg, "game"), Some(RuleKind::None));
        assert_eq!(override_kind(&cfg, "Game.exe"), Some(RuleKind::None), "a stem with the suffix too");
        assert_eq!(cfg.rule_for("game").0, RuleKind::None, "the HDR side reads the same entry");
        assert_eq!(override_kind(&cfg, "gam"), None);
    }
}
