//! The Rocket League Borderless fix. Its "Fullscreen" display mode is true exclusive fullscreen:
//! Windows composites no other window over the game, so the island is hidden. Its own Borderless
//! mode looks the same and keeps the island visible. Recall sets only the display-mode lines of
//! the game's own settings file (`TASystemSettings.ini`, section `[SystemSettings]`), only while
//! the game is closed; every other line is kept byte for byte.

use std::path::{Path, PathBuf};

use recall_fixes::plan::{rocket_league_borderless, IniKey, Plan};
use recall_fixes::Undo;

use crate::fix_ini;

pub const EXE: &str = "RocketLeague.exe";
pub const SECTION: &str = "SystemSettings";
/// The lines Borderless needs, in the order they are set.
pub const WANT: [(&str, &str); 2] = [("Fullscreen", "False"), ("Borderless", "True")];

/// What the read-only check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    Offer(Plan),
    AlreadyFixed,
    /// No settings file (the game never ran) or it is unreadable: no offer.
    NotApplicable(String),
}

/// `Documents\My Games\Rocket League\TAGame\Config\TASystemSettings.ini`.
pub fn config_path() -> Option<PathBuf> {
    let docs = crate::user_dirs::known(crate::user_dirs::Known::Documents)?;
    Some(crate::user_dirs::checked(docs.join("My Games").join("Rocket League").join("TAGame").join("Config").join("TASystemSettings.ini")))
}

/// The game's own settings file under this user's Documents, and nothing else.
pub fn is_target(target: &str) -> bool {
    config_path().is_some_and(|want| want.to_string_lossy().eq_ignore_ascii_case(target))
}

/// Whether `exe` (a full path or a file name) is the game.
pub fn is_game(exe: &str) -> bool {
    Path::new(exe).file_name().is_some_and(|n| n.eq_ignore_ascii_case(EXE))
}

/// Read-only: the plan for the settings file `target`.
pub fn plan(target: &str) -> Result<Check, String> {
    if !is_target(target) {
        return Err("not Rocket League's settings file".into());
    }
    let text = match fix_ini::read(Path::new(target)) {
        Ok(Some(t)) => t,
        Ok(None) => return Ok(Check::NotApplicable("Rocket League's settings file is not there yet".into())),
        Err(e) => return Ok(Check::NotApplicable(e)),
    };
    if !fix_ini::has_section(&text, SECTION) {
        return Ok(Check::NotApplicable("the settings file has no [SystemSettings] section".into()));
    }
    let keys: Vec<IniKey> = WANT
        .iter()
        .filter_map(|(key, to)| {
            let from = fix_ini::get(&text, SECTION, key);
            (!from.as_deref().is_some_and(|v| v.eq_ignore_ascii_case(to))).then(|| IniKey { file: target.to_owned(), section: SECTION.into(), key: (*key).into(), from, to: (*to).into() })
        })
        .collect();
    if keys.is_empty() {
        return Ok(Check::AlreadyFixed);
    }
    Ok(Check::Offer(rocket_league_borderless(keys)))
}

fn refuse_while_running() -> Result<(), String> {
    if crate::fix_procs::running(EXE) {
        return Err("Rocket League is running. Close it and try again; the game writes its settings file when it exits.".into());
    }
    Ok(())
}

/// Helper: set the lines. The game must be closed. One undo per line set, in apply order.
pub fn apply(target: &str) -> Result<Vec<Undo>, String> {
    if !is_target(target) {
        return Err("not Rocket League's settings file".into());
    }
    refuse_while_running()?;
    let path = Path::new(target);
    let mut text = fix_ini::read(path)?.ok_or("Rocket League's settings file is not there")?;
    // The file as it was before Recall's first change, kept once beside it.
    fix_ini::backup_once(path)?;
    let mut undo = Vec::new();
    for (key, to) in WANT {
        let prior = fix_ini::get(&text, SECTION, key);
        if prior.as_deref().is_some_and(|v| v.eq_ignore_ascii_case(to)) {
            continue;
        }
        text = fix_ini::set(&text, SECTION, key, Some(to)).ok_or("the settings file has no [SystemSettings] section")?;
        undo.push(Undo::IniKey { file: target.to_owned(), section: SECTION.into(), key: key.into(), value: to.into(), prior });
    }
    fix_ini::write(path, &text)?;
    Ok(undo)
}

/// Helper: put one line back, only while it still holds Recall's value. A line already back as
/// it was (a retry after a partial Undo) counts as done; a line the user changed since is left
/// as they set it, with a note. Either way the next line is still undone.
pub fn undo(target: &str, undo: &Undo) -> Result<Option<String>, String> {
    let Undo::IniKey { file, section, key, value, prior } = undo else { return Err("the undo receipt does not match this fix".into()) };
    let known = WANT.iter().any(|(k, v)| k == key && v == value);
    if !file.eq_ignore_ascii_case(target) || !is_target(target) || section != SECTION || !known {
        return Err("the undo receipt does not match this fix".into());
    }
    refuse_while_running()?;
    let path = Path::new(target);
    let Some(text) = fix_ini::read(path)? else { return Ok(None) };
    let now = fix_ini::get(&text, SECTION, key);
    let already_back = match (prior, &now) {
        (Some(p), Some(n)) => p.eq_ignore_ascii_case(n),
        (None, None) => true,
        _ => false,
    };
    if already_back {
        return Ok(None);
    }
    if !now.is_some_and(|v| v.eq_ignore_ascii_case(value)) {
        return Ok(Some(format!("{key} was changed after the fix, so it was left as you set it.")));
    }
    let back = fix_ini::set(&text, SECTION, key, prior.as_deref()).ok_or("the settings file has no [SystemSettings] section")?;
    fix_ini::write(path, &back).map(|()| None)
}

#[cfg(test)]
#[path = "fix_rl_tests.rs"]
mod tests;
