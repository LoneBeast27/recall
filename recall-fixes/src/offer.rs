//! Offers: a problem the recorder found, with the plan that would fix it. One
//! file per fix and program, written by the recorder and read by the app.
//! An offer changes nothing; it waits for the user's click or Not now.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::plan::{FixId, Plan};
use crate::receipt::fnv1a;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub plan: Plan,
    /// The program (or item) the fix is for.
    pub target: String,
    /// Seconds since 1970 (UTC) when the problem was seen.
    pub seen_unix: u64,
}

impl Offer {
    pub fn new(plan: Plan, target: &str) -> Offer {
        let seen_unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        Offer { plan, target: target.to_owned(), seen_unix }
    }

    /// `%LOCALAPPDATA%\recall-hdr\fixes\offers`.
    pub fn default_dir() -> Option<PathBuf> {
        std::env::var_os("LOCALAPPDATA").map(|d| Path::new(&d).join("recall-hdr").join("fixes").join("offers"))
    }

    fn path(dir: &Path, fix: FixId, target: &str) -> PathBuf {
        dir.join(format!("{}-{:016x}.json", fix.as_str(), fnv1a(&target.to_lowercase())))
    }

    pub fn save(&self, dir: &Path) -> Result<PathBuf, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("make {}: {e}", dir.display()))?;
        let path = Self::path(dir, self.plan.fix, &self.target);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// Every readable offer, newest first; unknown or damaged files are skipped.
    pub fn list(dir: &Path) -> Vec<Offer> {
        let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
        let mut out: Vec<Offer> = entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
            .collect();
        out.sort_by(|a, b| b.seen_unix.cmp(&a.seen_unix));
        out
    }

    /// Not now, or after the fix was applied.
    pub fn remove(dir: &Path, fix: FixId, target: &str) -> Result<(), String> {
        match std::fs::remove_file(Self::path(dir, fix, target)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}
