//! Where the capture engine is (`recall.exe` next to us, or a dev `target/` dir) and the
//! command that runs it (moved out of `supervisor.rs`).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{mute, CREATE_NO_WINDOW};
use crate::engine_policy::EngineSpec;

/// Locate the capture-engine exe.
///
/// Search order:
/// 1. **Same directory as our own exe** -- the shipping layout.
/// 2. **Dev fallback**: walk our exe's ancestors for a Cargo `target/` dir and
///    probe `target/debug/recall.exe` then `target/release/recall.exe`.
pub(super) fn locate_engine() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    locate_sibling_from(&exe, "recall.exe")
}

/// Core of the sibling lookup (engine + overlay), unit-testable against a
/// synthetic exe path under a temp dir.
pub(crate) fn locate_sibling_from(current_exe: &Path, file: &str) -> Option<PathBuf> {
    if let Some(dir) = current_exe.parent() {
        let sibling = dir.join(file);
        if sibling.is_file() {
            return Some(sibling);
        }
    }
    for ancestor in current_exe.ancestors() {
        if ancestor.file_name().map(|n| n == "target").unwrap_or(false) {
            for profile in ["debug", "release"] {
                let candidate = ancestor.join(profile).join(file);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

#[cfg(test)]
pub(super) fn locate_engine_from(current_exe: &Path) -> Option<PathBuf> {
    locate_sibling_from(current_exe, "recall.exe")
}

/// Build `recall.exe --json` with piped stdio, no console, the spec's environment edits, and
/// the controller's latest mic mute over the spec's (`supervisor_mute.rs`).
pub(super) fn engine_command(path: &Path, spec: &EngineSpec, mute: Option<bool>) -> Command {
    use std::os::windows::process::CommandExt;
    let mut cmd = Command::new(path);
    cmd.arg("--json")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);
    for (key, value) in &spec.env {
        match value {
            Some(v) => cmd.env(key, v),
            None => cmd.env_remove(key),
        };
    }
    if let Some(muted) = mute {
        cmd.env(mute::MIC_MUTED_ENV, if muted { "1" } else { "0" });
    }
    cmd
}
