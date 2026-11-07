//! `ConfigStore`: where config.toml is loaded from and saved to (split from record_config.rs).
use std::path::{Path, PathBuf};

use crate::config::Config;

/// Where the unified mode loads/saves its config. Tests point it at a temp dir.
#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
    /// Set when config.toml existed but could not be read at load: saves are refused
    /// for the rest of the run, so the defaults never replace the user's file.
    blocked: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl ConfigStore {
    /// `%APPDATA%\recall-hdr\config.toml` -- the user's real config.
    pub fn user() -> Self {
        Self::at(Config::path())
    }

    pub fn at(path: impl Into<PathBuf>) -> Self {
        ConfigStore { path: path.into(), blocked: Default::default() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The config on disk. Unreadable (after a brief retry) -> the defaults, and every
    /// later [`ConfigStore::save`] this run is refused.
    pub fn load(&self) -> Config {
        crate::config::load_checked(&self.path).unwrap_or_else(|e| {
            crate::log!("config: read {} failed: {e}; using defaults, settings are not saved this run", self.path.display());
            let reason = format!("{} could not be read at start ({e}); settings are not saved until Recall restarts", self.path.display());
            *self.blocked.lock().unwrap_or_else(|p| p.into_inner()) = Some(reason);
            Config::default()
        })
    }

    pub fn save(&self, cfg: &Config) -> Result<(), String> {
        if let Some(reason) = self.blocked.lock().unwrap_or_else(|p| p.into_inner()).clone() {
            return Err(reason);
        }
        cfg.save_to(&self.path)
    }
}
