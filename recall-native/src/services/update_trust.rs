//! What this PC remembers about update manifests (`%LOCALAPPDATA%\Recall\updates\trust.json`):
//! the highest `sequence` it has accepted (an older signed manifest, replayed by a host, is
//! refused) and the key names a backup-signed manifest revoked. A missing, empty or garbled
//! file is a fresh start, never an error. Unknown keys are kept on save, so a newer build's
//! file survives a rollback to this one and back.
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::manifest::{Manifest, Role};

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct TrustState {
    pub highest_sequence: u64,
    pub revoked: Vec<String>,
    /// Everything else in the file (a newer build's keys), written back unchanged.
    rest: Map<String, Value>,
}

pub(crate) fn trust_file() -> Option<PathBuf> {
    crate::user_dirs::local_appdata().map(|p| crate::user_dirs::checked(p.join("Recall").join("updates").join("trust.json")))
}

impl TrustState {
    pub(crate) fn load(file: Option<&Path>) -> Self {
        let Some(Value::Object(mut map)) = file.and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice(&b).ok()) else {
            return Self::default();
        };
        let highest_sequence = map.remove("highest_sequence").and_then(|v| v.as_u64()).unwrap_or(0);
        let revoked = match map.remove("revoked") {
            Some(Value::Array(names)) => names.into_iter().filter_map(|n| n.as_str().map(str::to_owned)).collect(),
            _ => Vec::new(),
        };
        Self { highest_sequence, revoked, rest: map }
    }

    pub(crate) fn save(&self, file: &Path) -> Result<(), String> {
        let mut map = self.rest.clone();
        map.insert("highest_sequence".into(), self.highest_sequence.into());
        map.insert("revoked".into(), self.revoked.clone().into());
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let bytes = serde_json::to_vec_pretty(&Value::Object(map)).map_err(|e| e.to_string())?;
        // Write beside, then replace: a crash mid-write never leaves a half file.
        let temp = file.with_extension("json.tmp");
        std::fs::write(&temp, bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&temp, file).map_err(|e| e.to_string())
    }

    /// A manifest older than one already accepted here (a replay) is refused, unless the backup
    /// key signed it: a leaked release key that signed a far-future sequence must not lock the
    /// backup key out of recovery.
    pub(crate) fn admits(&self, m: &Manifest) -> bool {
        m.signer == Role::Backup || m.sequence >= self.highest_sequence
    }

    /// Remember an accepted manifest: its sequence, and its revocations when the backup key
    /// signed it. Returns whether anything changed (the file is then saved).
    pub(crate) fn accept(&mut self, m: &Manifest) -> bool {
        let mut changed = false;
        let mut revoked_now = false;
        if m.signer == Role::Backup {
            for name in &m.revoke_keys {
                // The backup key never revokes itself: that would leave no way back.
                if *name != m.signer_name && !self.revoked.contains(name) {
                    self.revoked.push(name.clone());
                    revoked_now = true;
                }
            }
        }
        if revoked_now {
            // A revoked key may have pushed the sequence far ahead: start again from the
            // backup's, so the replacement key's manifests are admitted.
            self.highest_sequence = m.sequence;
            changed = true;
        } else if m.sequence > self.highest_sequence {
            self.highest_sequence = m.sequence;
            changed = true;
        }
        changed
    }
}
