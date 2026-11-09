//! What an Audio-page change does to the engine (design-audio-mute P10, B1-B3): the list every
//! later spawn starts with, the lines that go to the running engine at once, and whether a
//! restart follows. The lines go first, whatever the engine's state (a spawn, before its
//! `hello`, or busy with a save or a long recording): every privacy-tightening part (the
//! effective flag, the saved id, `retire`) is in force at once, so a restart that must wait for
//! the save or the long recording (`request_settings_restart`, Defer) waits only for the new
//! shape: a new track, or the newly picked device.

use serde_json::Value;

use crate::record_audio_sources::{spawn_update, AudioSource};

#[derive(Debug, PartialEq)]
pub struct AudioChange {
    /// The new spawn list (`Supervisor::update_audio`), when the list changed.
    pub spawn_value: Option<String>,
    /// The live lines (`audio_intent::change_lines`).
    pub lines: Vec<Value>,
    /// A restart follows (it may be deferred; privacy is already in force).
    pub restart: bool,
}

/// The change from `old` to `new`. `settings_restart`: `engine_settings_changed` (the track
/// shape, or another engine setting). Below the protocol floor (`below_floor`) the engine
/// ignores `set_source`, so a gain or name it `cannot_take` restarts it (it records no input or
/// picked device, so nothing private waits on that).
pub fn plan(old: &[AudioSource], new: &[AudioSource], settings_restart: bool, (running, below_floor): (bool, bool), cannot_take: impl Fn(&[String]) -> bool) -> AudioChange {
    let lines = crate::audio_intent::change_lines(old, new);
    let texts: Vec<String> = lines.iter().map(Value::to_string).collect();
    let restart = settings_restart || (running && below_floor && cannot_take(&texts));
    AudioChange { spawn_value: spawn_update(old, new), lines, restart }
}

#[cfg(test)]
#[path = "record_audio_change_tests.rs"]
mod tests;
