//! Interaction memory for the smart cache: what the user opens, hovers, exports and
//! favourites, which games they play, how far back they browse. Everything decays
//! exponentially (a clip's own interest over `Weights::clip_half_life_days`, games and
//! browse depth over `Weights::memory_half_life_days`), so old habits fade. Stored locally in
//! `<cache>/interactions.json` (atomic, debounced writes); never leaves the machine.
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::score::Weights;

/// One user interaction, reported by the UI (`record_interaction` never blocks).
#[derive(Debug, Clone, PartialEq)]
pub enum Interaction {
    /// A clip was opened in the editor (path).
    Opened(String),
    /// Playback started in the editor (path), once per open.
    Played(String),
    /// A library tile was hovered (path).
    Hovered(String),
    /// A clip was exported (path).
    Exported(String),
    /// A clip was marked favourite (path).
    Favourited(String),
    /// The recorder saved a new clip (path).
    Saved(String),
    /// The library was scrolled to clips this many days old.
    ScrolledTo { days_back: u32 },
}

/// An exponentially decaying amount: `value` as of `at_ms`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct Decayed {
    pub value: f64,
    pub at_ms: u64,
}

impl Decayed {
    pub fn value_at(&self, now_ms: u64, half_life_ms: f64) -> f64 {
        if self.value == 0.0 {
            return 0.0;
        }
        self.value * 0.5f64.powf(now_ms.saturating_sub(self.at_ms) as f64 / half_life_ms)
    }
    pub fn add(&mut self, amount: f64, now_ms: u64, half_life_ms: f64) {
        self.value = self.value_at(now_ms, half_life_ms) + amount;
        self.at_ms = now_ms.max(self.at_ms);
    }
    /// Decayed maximum: a deep scroll raises it, then it relaxes back over time.
    pub fn raise_to(&mut self, amount: f64, now_ms: u64, half_life_ms: f64) {
        self.value = self.value_at(now_ms, half_life_ms).max(amount);
        self.at_ms = now_ms.max(self.at_ms);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct ClipMemory {
    pub interest: Decayed,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub favourite: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct History {
    /// Lowercase clip file name → memory.
    pub clips: BTreeMap<String, ClipMemory>,
    /// Game key → decayed opens / hovers / exports of that game's clips.
    pub games: BTreeMap<String, Decayed>,
    /// Game key → display name ("Rocket League").
    pub game_names: BTreeMap<String, String>,
    /// Decayed maximum of how many days back the user scrolls.
    pub browse_days: Decayed,
}

/// (lowercase file name, game key, game display name) of a clip path.
pub(super) fn clip_key(path: &str) -> Option<(String, String, String)> {
    let name = Path::new(path).file_name()?.to_string_lossy().into_owned();
    let prefix = crate::services::library::parse_file_name(&name)
        .map(|(prefix, _)| prefix)
        .unwrap_or_else(|| name.split("__").next().unwrap_or(&name).trim_end_matches(".mp4").to_owned());
    let (key, display) = crate::services::library::normalize_game(&prefix);
    Some((name.to_lowercase(), key, display))
}

impl History {
    /// Applies one interaction at `now_ms`. Returns true when it should re-plan the cache.
    pub fn apply(&mut self, event: &Interaction, now_ms: u64, weights: &Weights) -> bool {
        let (half_life, clip_half_life) = (weights.memory_half_life_ms(), weights.clip_half_life_ms());
        let (path, amount, replan) = match event {
            Interaction::ScrolledTo { days_back } => {
                self.browse_days.raise_to(f64::from(*days_back), now_ms, half_life);
                return false;
            }
            Interaction::Opened(path) => (path, weights.open_event, true),
            Interaction::Played(path) => (path, weights.play_event, false),
            Interaction::Exported(path) => (path, weights.export_event, true),
            Interaction::Hovered(path) => (path, weights.hover_event, false),
            Interaction::Favourited(path) => (path, weights.favourite_event, true),
            // A fresh save starts as warm as one open (it is usually opened within minutes).
            Interaction::Saved(path) => (path, weights.open_event, true),
        };
        let Some((name, game, display)) = clip_key(path) else { return false };
        let memory = self.clips.entry(name).or_default();
        match event {
            Interaction::Favourited(_) => memory.favourite = true,
            Interaction::Saved(_) => memory.saved_at_ms = Some(now_ms),
            _ => {}
        }
        if amount > 0.0 {
            memory.interest.add(amount, now_ms, clip_half_life);
        }
        // A save says which game is recorded, not which one is watched.
        if amount > 0.0 && !matches!(event, Interaction::Saved(_)) {
            self.games.entry(game.clone()).or_default().add(amount, now_ms, half_life);
            self.game_names.insert(game, display);
        }
        replan
    }

    /// Drops memories that have decayed to nothing (keeps the file small).
    pub fn prune(&mut self, now_ms: u64, weights: &Weights) {
        let (half_life, clip_half_life) = (weights.memory_half_life_ms(), weights.clip_half_life_ms());
        let day = 86_400_000;
        self.clips.retain(|_, m| m.favourite || m.interest.value_at(now_ms, clip_half_life) >= 0.01 || m.saved_at_ms.is_some_and(|t| now_ms.saturating_sub(t) < day));
        self.games.retain(|_, g| g.value_at(now_ms, half_life) >= 0.01);
        let games = &self.games;
        self.game_names.retain(|key, _| games.contains_key(key));
    }

    pub fn load(path: &Path) -> History {
        std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        super::projects::write_atomic(path, &bytes)
    }

    /// The `n` most-played games' display names, by decayed affinity.
    pub fn favoured_games(&self, now_ms: u64, weights: &Weights, n: usize) -> Vec<String> {
        let half_life = weights.memory_half_life_ms();
        let mut games: Vec<(f64, &String)> = self.games.iter().map(|(key, d)| (d.value_at(now_ms, half_life), key)).filter(|(v, _)| *v >= 0.5).collect();
        games.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(b.1)));
        games.into_iter().take(n).map(|(_, key)| self.game_names.get(key).cloned().unwrap_or_else(|| key.clone())).collect()
    }
}

const FLUSH_AFTER: Duration = Duration::from_secs(3);
const REPLAN_AFTER: Duration = Duration::from_secs(5);

struct Recorder {
    tx: Sender<Interaction>,
    history: Arc<Mutex<History>>,
}

fn recorder() -> &'static Recorder {
    static RECORDER: OnceLock<Recorder> = OnceLock::new();
    RECORDER.get_or_init(|| {
        let path = super::cache_root().join("interactions.json");
        let history = Arc::new(Mutex::new(History::load(&path)));
        let (tx, rx) = mpsc::channel();
        let shared = history.clone();
        let spawned = std::thread::Builder::new().name("recall-cache-history".into()).spawn(move || run(rx, shared, &path));
        if let Err(error) = spawned {
            eprintln!("start recall-cache-history: {error}");
        }
        Recorder { tx, history }
    })
}

/// Queues an interaction for the smart cache (never blocks; written within ~3 s).
pub fn record_interaction(interaction: Interaction) {
    let _ = recorder().tx.send(interaction);
}

/// Current memory (a copy) for scoring.
pub(super) fn snapshot() -> History {
    recorder().history.lock().map(|h| h.clone()).unwrap_or_default()
}

fn run(rx: Receiver<Interaction>, history: Arc<Mutex<History>>, path: &Path) {
    let weights = Weights::default();
    let (mut dirty, mut replan) = (false, false);
    let mut last_save = Instant::now();
    let mut last_plan = Instant::now() - REPLAN_AFTER;
    loop {
        match rx.recv_timeout(FLUSH_AFTER) {
            Ok(event) => {
                let mut memory = history.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                replan |= memory.apply(&event, super::budget::now_ms(), &weights);
                dirty = true;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if dirty && last_save.elapsed() >= FLUSH_AFTER {
            let mut memory = history.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            memory.prune(super::budget::now_ms(), &weights);
            let _ = memory.save(path);
            dirty = false;
            last_save = Instant::now();
        }
        if replan && last_plan.elapsed() >= REPLAN_AFTER {
            replan = false;
            last_plan = Instant::now();
            super::prefetch::replan();
        }
    }
}
