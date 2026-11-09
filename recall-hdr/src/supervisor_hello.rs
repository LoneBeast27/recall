//! What the running engine's `hello` says it can do (split out of `supervisor.rs`).

use super::Supervisor;

impl Supervisor {
    /// The running engine said it applies `set_source` live (`hello.live_sources`); older
    /// engines ignore that command, so their gain and name changes need a restart.
    pub fn engine_takes_live_sources(&self) -> bool {
        self.hello_says("live_sources")
    }

    /// The running engine's `hello` sets the capability `key` to true (absent: an older engine).
    pub fn hello_says(&self, key: &str) -> bool {
        self.shared.hello_says(key)
    }
}

impl super::Shared {
    /// [`Supervisor::hello_says`], for the manager thread.
    pub(super) fn hello_says(&self, key: &str) -> bool {
        let cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        cache.hello.as_deref().and_then(|h| serde_json::from_str::<serde_json::Value>(h).ok()).and_then(|v| v.get(key)?.as_bool()).unwrap_or(false)
    }
}
