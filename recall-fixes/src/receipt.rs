//! Receipts: one file per applied fix, holding what Undo needs. The helper
//! writes it (atomically, before it reports success) and removes it after a
//! successful Undo, so a slow helper or a closed app never leaves a change
//! without its receipt.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::plan::{FixId, Plan, Vendor};

/// How to reverse one change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Undo {
    /// Remove what is still Recall's in its own profile `profile`.
    DriverOwnProfile { vendor: Vendor, exe: String, setting_id: u32, value: u32, profile: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub fix: FixId,
    /// The program or item the fix was applied to.
    pub target: String,
    /// Seconds since 1970 (UTC).
    pub applied_unix: u64,
    /// The plan as the user saw it, for the applied list.
    pub plan: Plan,
    /// In apply order; Undo runs them last to first.
    pub undo: Vec<Undo>,
}

/// The receipts folder.
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Store { dir: dir.into() }
    }

    /// `%LOCALAPPDATA%\recall-hdr\fixes\applied`.
    pub fn default_dir() -> Option<PathBuf> {
        std::env::var_os("LOCALAPPDATA").map(|d| Path::new(&d).join("recall-hdr").join("fixes").join("applied"))
    }

    fn path(&self, fix: FixId, target: &str) -> PathBuf {
        self.dir.join(format!("{}-{}.json", fix.as_str(), target_hash(target)))
    }

    pub fn save(&self, receipt: &Receipt) -> Result<PathBuf, String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("make {}: {e}", self.dir.display()))?;
        let path = self.path(receipt.fix, &receipt.target);
        let tmp = path.with_extension("json.tmp");
        let body = serde_json::to_vec_pretty(receipt).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, body).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("rename {}: {e}", path.display()))?;
        Ok(path)
    }

    pub fn find(&self, fix: FixId, target: &str) -> Option<Receipt> {
        let body = std::fs::read(self.path(fix, target)).ok()?;
        serde_json::from_slice(&body).ok()
    }

    /// Every readable receipt, oldest first. A damaged file is skipped, never deleted.
    pub fn list(&self) -> Vec<Receipt> {
        let Ok(dir) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        let mut out: Vec<Receipt> = dir
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
            .collect();
        out.sort_by_key(|r| r.applied_unix);
        out
    }

    /// After a successful Undo.
    pub fn remove(&self, fix: FixId, target: &str) -> Result<(), String> {
        match std::fs::remove_file(self.path(fix, target)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Stable across runs and Rust versions (unlike `DefaultHasher`).
pub(crate) fn fnv1a(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

/// A program path as 16 hex digits: for file names and log lines (no user name in logs).
pub fn target_hash(target: &str) -> String {
    format!("{:016x}", fnv1a(&target.to_lowercase()))
}
