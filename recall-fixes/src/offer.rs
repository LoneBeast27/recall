//! Offers: a problem the recorder found, with the plan that would fix it. One
//! file per fix and program, written by the recorder and read by the app.
//! An offer changes nothing; it waits for the user's click or Not now.
//! Not now is remembered on disk ([`Dismissed`]), so the offer stays away
//! after a restart.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::plan::{FixId, Plan};
use crate::receipt::target_hash;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub plan: Plan,
    /// The program (or item) the fix is for.
    pub target: String,
    /// Seconds since 1970 (UTC) when the problem was seen.
    pub seen_unix: u64,
}

/// `%LOCALAPPDATA%\recall-hdr\fixes`.
pub fn fixes_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| Path::new(&d).join("recall-hdr").join("fixes"))
}

impl Offer {
    pub fn new(plan: Plan, target: &str) -> Offer {
        let seen_unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        Offer { plan, target: target.to_owned(), seen_unix }
    }

    /// `%LOCALAPPDATA%\recall-hdr\fixes\offers`.
    pub fn default_dir() -> Option<PathBuf> {
        fixes_dir().map(|d| d.join("offers"))
    }

    fn path(dir: &Path, fix: FixId, target: &str) -> PathBuf {
        dir.join(format!("{}-{}.json", fix.as_str(), target_hash(target)))
    }

    pub fn save(&self, dir: &Path) -> Result<PathBuf, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("make {}: {e}", dir.display()))?;
        let path = Self::path(dir, self.plan.fix, &self.target);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// Every readable offer, newest first. A damaged file, or one whose plan
    /// names another fix than its own steps, is skipped.
    pub fn list(dir: &Path) -> Vec<Offer> {
        let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
        let mut out: Vec<Offer> = entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| serde_json::from_slice::<Offer>(&std::fs::read(e.path()).ok()?).ok())
            .filter(|o| !o.plan.steps.is_empty())
            .collect();
        out.sort_by_key(|o| std::cmp::Reverse(o.seen_unix));
        out
    }

    /// After the fix was applied, or Not now.
    pub fn remove(dir: &Path, fix: FixId, target: &str) -> Result<(), String> {
        match std::fs::remove_file(Self::path(dir, fix, target)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Fixes the user said Not now to, per fix and program (hashed, no paths):
/// `%LOCALAPPDATA%\recall-hdr\fixes\dismissed.json`.
pub struct Dismissed {
    file: PathBuf,
}

impl Dismissed {
    pub fn new(fixes_dir: &Path) -> Dismissed {
        Dismissed { file: fixes_dir.join("dismissed.json") }
    }

    fn key(fix: FixId, target: &str) -> String {
        format!("{}:{}", fix.as_str(), target_hash(target))
    }

    fn read(&self) -> BTreeSet<String> {
        std::fs::read(&self.file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn contains(&self, fix: FixId, target: &str) -> bool {
        self.read().contains(&Self::key(fix, target))
    }

    pub fn add(&self, fix: FixId, target: &str) -> Result<(), String> {
        let mut all = self.read();
        all.insert(Self::key(fix, target));
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let tmp = self.file.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&all).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.file).map_err(|e| e.to_string())
    }
}
