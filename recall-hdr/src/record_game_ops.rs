//! One app's registration change (Settings -> Games, the tray's "Add <exe> as a
//! game"), applied by the controller to its current file config. The native app
//! sends `{"cmd":"register_game","exe":..,"how":..}` instead of whole arrays built
//! from its last snapshot, so two quick edits, or a tray add in between, both land
//! (R8-detect-7).
//!
//! - `record`: a game override. An app already recorded is left as it is (its
//!   class, `hdr_game` included, and its clamp); a Never entry becomes the default
//!   game class and keeps its clamp (R8-detect-4).
//! - `never`: a `none` override; an existing entry keeps its clamp.
//! - `fullscreen` / `unlist`: add / drop a `detect.fullscreen_games` entry.
//! - `remove`: drop every override of the exe.
//!
//! An exe matches like the rules do (G9: `Game.exe` = `game`). `None` = nothing to
//! change (no write, no config churn).

use serde_json::{json, Value};

use crate::config::{Config, Override};
use crate::record_app::Controller;
use crate::record_status::config_event;
use crate::record_target::is_fullscreen_game;
use crate::rules::{override_kind, same_exe};
use crate::types::RuleKind;

/// A `register_game` op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GameOp {
    Record,
    Never,
    Fullscreen,
    Remove,
    Unlist,
}

impl GameOp {
    pub(crate) fn parse(s: &str) -> Option<GameOp> {
        Some(match s {
            "record" => GameOp::Record,
            "never" => GameOp::Never,
            "fullscreen" => GameOp::Fullscreen,
            "remove" => GameOp::Remove,
            "unlist" => GameOp::Unlist,
            _ => return None,
        })
    }
}

/// The config's default game class for a new "Record" entry (`sdr_game` unless HDR).
fn game_kind(cfg: &Config) -> RuleKind {
    match cfg.behavior.default_game_class {
        RuleKind::HdrGame => RuleKind::HdrGame,
        _ => RuleKind::SdrGame,
    }
}

/// The `set_config` patch for `op` on `exe`, against `cfg`; `None` = no change.
pub(crate) fn op_patch(cfg: &Config, exe: &str, op: GameOp) -> Option<Value> {
    let exe = exe.trim();
    if exe.is_empty() {
        return None;
    }
    match op {
        GameOp::Record => set_kind(cfg, exe, game_kind(cfg)),
        GameOp::Never => set_kind(cfg, exe, RuleKind::None),
        GameOp::Remove => {
            let list: Vec<Override> = cfg.overrides.iter().filter(|o| !same_exe(&o.exe, exe)).cloned().collect();
            (list.len() != cfg.overrides.len()).then(|| json!({ "overrides": list }))
        }
        GameOp::Fullscreen => {
            if is_fullscreen_game(cfg, exe) {
                return None;
            }
            let mut list = cfg.detect.fullscreen_games.clone();
            list.push(exe.to_string());
            Some(json!({ "detect": { "fullscreen_games": list } }))
        }
        GameOp::Unlist => {
            let list: Vec<String> = cfg.detect.fullscreen_games.iter().filter(|g| !same_exe(g, exe)).cloned().collect();
            (list.len() != cfg.detect.fullscreen_games.len()).then(|| json!({ "detect": { "fullscreen_games": list } }))
        }
    }
}

/// Record (`kind` a game class) or Never (`kind` = none) for `exe`: the first
/// override of the exe is changed in place (exe spelling and clamp kept), any
/// duplicate of it is dropped; an exe with none gets a new entry.
fn set_kind(cfg: &Config, exe: &str, kind: RuleKind) -> Option<Value> {
    let is_game = |k: RuleKind| matches!(k, RuleKind::HdrGame | RuleKind::SdrGame);
    match override_kind(cfg, exe) {
        Some(k) if k == kind || (is_game(k) && is_game(kind)) => return None,
        _ => {}
    }
    let mut list: Vec<Override> = Vec::with_capacity(cfg.overrides.len() + 1);
    let mut done = false;
    for o in &cfg.overrides {
        if !same_exe(&o.exe, exe) {
            list.push(o.clone());
        } else if !done {
            list.push(Override { kind, ..o.clone() });
            done = true;
        }
    }
    if !done {
        list.push(Override { exe: exe.to_string(), kind, clamp: None });
    }
    Some(json!({ "overrides": list }))
}

/// The tray's "Add <exe> as a game": Record, but never over a Never record entry
/// (R8-detect-8); `None` = leave the config as it is.
pub(crate) fn tray_add_patch(cfg: &Config, exe: &str) -> Option<Value> {
    if override_kind(cfg, exe) == Some(RuleKind::None) {
        return None;
    }
    op_patch(cfg, exe, GameOp::Record)
}

/// The tray's offer for `offered` goes: Settings registered the exe (any
/// override, Never included, or a `fullscreen_games` entry).
pub(crate) fn offer_retired(cfg: &Config, offered: &str) -> bool {
    override_kind(cfg, offered).is_some() || is_fullscreen_game(cfg, offered)
}

impl Controller {
    /// `register_game` from the pipe: one app's op against the current file config.
    pub(crate) fn register_game(&mut self, val: &Value, client: Option<u64>) {
        let exe = val.get("exe").and_then(Value::as_str).map(str::trim).filter(|e| !e.is_empty() && !e.contains(['\\', '/']));
        let op = val.get("how").and_then(Value::as_str).and_then(GameOp::parse);
        let (Some(exe), Some(op)) = (exe, op) else {
            return self.reply_error(client, "register_game needs \"exe\" (an exe name, no folders) and \"how\": record|never|fullscreen|remove|unlist");
        };
        match op_patch(&self.file_cfg, exe, op) {
            Some(patch) => {
                self.apply_config_patch(&patch, client);
            }
            None => {
                if let Some(c) = client {
                    self.hub.event_to(c, "session", &config_event(&self.cfg));
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "record_game_ops_tests.rs"]
mod tests;
