//! Smart-cache planner: keep the cache as full as the budget allows (~95%) with what the
//! user is most likely to look at next (`score`). Clips are ranked by score; every clip gets
//! its thumbnail and duration (cheap, kept), then in rank order each clip climbs the
//! cascade hover frames → editor media (top [`EDITOR_TOP`]) → SSD video copy (top
//! [`VIDEO_TOP`], within the half-budget video cap). Work items that would overfill the
//! budget are skipped, so smaller items further down still fit.
//!
//! The plan runs as each disk lane's idle source: one item at a time, only after the lane
//! has been free of on-demand work for the idle grace period, and not at all while the
//! recorder is saving or recording ([`set_recorder_busy`]).
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use super::budget;
use super::evict::Tier;
use super::history;
use super::lane::{self, IdleSource, LaneKey, Task};
use super::score::{ClipFacts, Weights, score_all};

pub const EDITOR_TOP: usize = 24;
pub const VIDEO_TOP: usize = 3;
const FILL_PERCENT: u64 = 95;
const THUMB_ESTIMATE: u64 = 30 << 10;
const SCRUB_ESTIMATE: u64 = 160 << 10;
/// Editor audio stems: 48 kHz 16-bit, stereo game + mono mic.
const AUDIO_BYTES_PER_SEC: f64 = 288_000.0;
const VISUALS_ESTIMATE: u64 = 200 << 10;

/// One listed clip as the planner sees it.
#[derive(Debug, Clone)]
pub struct PrefetchClip {
    pub path: String,
    pub name: String,
    pub bytes: u64,
    pub mtime_secs: u64,
    /// The list already carries a cached thumbnail.
    pub has_thumb: bool,
    /// Cached duration (metadata store), if any.
    pub duration: Option<f64>,
    pub game_key: String,
    pub favourite: bool,
    pub edited: bool,
}

/// Results the UI wants, delivered exactly like on-demand results.
#[derive(Debug, Clone)]
pub enum PrefetchEvent {
    Thumb(String, Result<String, String>),
    Duration(String, Option<f64>),
    Scrub(String, Result<Vec<String>, String>),
}

pub type Sink = Arc<dyn Fn(PrefetchEvent) + Send + Sync>;

/// Prepares a clip's editor media (audio stems, poster, filmstrip) without opening it;
/// returns the bytes now cached for it. Called on the clip's disk lane.
pub type PrepareEditor = fn(&str) -> Result<u64, String>;

static RECORDER_BUSY: AtomicBool = AtomicBool::new(false);

/// The recorder is saving or recording: background filling pauses (on-demand work does not).
pub fn set_recorder_busy(busy: bool) {
    RECORDER_BUSY.store(busy, Ordering::Relaxed);
}

pub fn recorder_busy() -> bool {
    RECORDER_BUSY.load(Ordering::Relaxed)
}

/// The open clip's copy from `path` slows down for the recorder (`ClipFill::pace`): only a
/// slow (hard) drive competes with its writes. A clip read from the SSD is never slowed.
pub fn recorder_holds(path: &Path) -> bool {
    holds(recorder_busy(), path)
}

pub(super) fn holds(busy: bool, path: &Path) -> bool {
    busy && super::is_slow(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    Thumb,
    Duration,
    Scrub,
    Editor,
    Video,
}

/// Work order. `ranked` holds clip indices, best score first.
pub(super) fn plan(clips: &[PrefetchClip], ranked: &[usize], cached: &dyn Fn(Step, &PrefetchClip) -> bool) -> Vec<(Step, usize)> {
    let mut order = Vec::new();
    for &index in ranked {
        let clip = &clips[index];
        if !clip.has_thumb && !cached(Step::Thumb, clip) {
            order.push((Step::Thumb, index));
        }
        if clip.duration.is_none() {
            order.push((Step::Duration, index));
        }
    }
    for (rank, &index) in ranked.iter().enumerate() {
        let clip = &clips[index];
        let mut push = |step: Step| {
            if !cached(step, clip) {
                order.push((step, index));
            }
        };
        push(Step::Scrub);
        if rank < EDITOR_TOP {
            push(Step::Editor);
        }
        if rank < VIDEO_TOP {
            push(Step::Video);
        }
    }
    order
}

/// Fill limit shared by every lane's feed: ~95% of the budget, video within half of it.
pub(super) struct Gate {
    target: u64,
    cap: u64,
    used: AtomicU64,
    video: AtomicU64,
}

impl Gate {
    pub fn new(budget: u64, used: u64, video_used: u64) -> Self {
        Self { target: budget / 100 * FILL_PERCENT, cap: budget / 2, used: AtomicU64::new(used), video: AtomicU64::new(video_used) }
    }
    /// Thumbnails and durations always run (thumbnails are kept by the eviction rules).
    pub fn admit(&self, step: Step, cost: u64) -> bool {
        let fits = self.used.load(Ordering::Relaxed).saturating_add(cost) <= self.target;
        match step {
            Step::Thumb | Step::Duration => true,
            Step::Video => fits && self.video.load(Ordering::Relaxed).saturating_add(cost) <= self.cap,
            Step::Scrub | Step::Editor => fits,
        }
    }
    pub fn add(&self, step: Step, bytes: u64) {
        self.used.fetch_add(bytes, Ordering::Relaxed);
        if step == Step::Video {
            self.video.fetch_add(bytes, Ordering::Relaxed);
        }
    }
    pub fn used(&self) -> u64 {
        self.used.load(Ordering::Relaxed)
    }
}

pub(super) fn estimate(step: Step, clip: &PrefetchClip) -> u64 {
    match step {
        Step::Thumb => THUMB_ESTIMATE,
        Step::Duration => 0,
        Step::Scrub => SCRUB_ESTIMATE,
        Step::Editor => clip.duration.map_or(clip.bytes / 12, |d| (d * AUDIO_BYTES_PER_SEC) as u64) + VISUALS_ESTIMATE,
        Step::Video => clip.bytes,
    }
}

struct Library {
    clips: Vec<PrefetchClip>,
    sink: Sink,
    prepare: PrepareEditor,
}

fn library() -> &'static Mutex<Option<Arc<Library>>> {
    static LIBRARY: OnceLock<Mutex<Option<Arc<Library>>>> = OnceLock::new();
    LIBRARY.get_or_init(|| Mutex::new(None))
}

fn scores() -> &'static Mutex<HashMap<String, f64>> {
    static SCORES: OnceLock<Mutex<HashMap<String, f64>>> = OnceLock::new();
    SCORES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn active() -> &'static Mutex<HashSet<LaneKey>> {
    static ACTIVE: OnceLock<Mutex<HashSet<LaneKey>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Last computed score of a clip identity (0 for clips not in the current list).
pub(super) fn score_of(id: &str) -> f64 {
    scores().lock().ok().and_then(|s| s.get(id).copied()).unwrap_or(0.0)
}

fn clip_id(clip: &PrefetchClip) -> String {
    super::identity(&clip.name, clip.bytes, clip.mtime_secs)
}

fn cached(root: &Path, owners: &HashSet<String>, step: Step, clip: &PrefetchClip) -> bool {
    let id = clip_id(clip);
    match step {
        Step::Thumb => super::valid_file(&super::thumbs::thumb_path(root, &id)),
        Step::Duration => clip.duration.is_some(),
        Step::Scrub => super::scrub_frame_paths(root, &id).iter().all(|p| super::valid_file(p)),
        Step::Editor => owners.contains(&id) && super::valid_file(&super::visuals_dir(root, &id).join("poster.jpg")),
        // A clip already on an SSD gains nothing from a copy.
        Step::Video => {
            !super::drive_kind(Path::new(&clip.path)).is_hdd() || root.join("video").join(format!("{id}.mp4")).metadata().is_ok_and(|m| m.len() == clip.bytes)
        }
    }
}

struct Feed {
    root: PathBuf,
    budget: u64,
    library: Arc<Library>,
    owners: HashSet<String>,
    order: Mutex<VecDeque<(Step, usize)>>,
    gate: Arc<Gate>,
}

impl IdleSource for Arc<Feed> {
    fn paused(&self) -> bool {
        recorder_busy()
    }

    fn next_task(&self) -> Option<Task> {
        loop {
            let (step, index) = self.order.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).pop_front()?;
            let clip = &self.library.clips[index];
            // Re-check: on-demand work may have filled it since the plan was made.
            if cached(&self.root, &self.owners, step, clip) || !self.gate.admit(step, estimate(step, clip)) {
                continue;
            }
            let feed = Arc::clone(self);
            return Some(Box::new(move || feed.run(step, index)));
        }
    }
}

impl Feed {
    fn run(&self, step: Step, index: usize) {
        let clip = &self.library.clips[index];
        let (path, sink) = (clip.path.clone(), &self.library.sink);
        let size = |p: &str| Path::new(p).metadata().map_or(0, |m| m.len());
        match step {
            Step::Thumb => {
                let result = super::thumbs::ensure_in(&self.root, Path::new(&path));
                if let Ok(thumb) = &result {
                    self.gate.add(step, size(thumb));
                }
                sink(PrefetchEvent::Thumb(path, result));
            }
            Step::Duration => sink(PrefetchEvent::Duration(path.clone(), crate::services::library::mp4_duration(Path::new(&path)))),
            Step::Scrub => {
                let result = crate::services::library::scrub_frames(&path);
                if let Ok(frames) = &result {
                    self.gate.add(step, frames.iter().map(|f| size(f)).sum());
                }
                sink(PrefetchEvent::Scrub(path, result));
            }
            Step::Editor => {
                if let Ok(bytes) = (self.library.prepare)(&path) {
                    self.gate.add(step, bytes);
                }
            }
            Step::Video => {
                if super::video::cache_in(&self.root, Path::new(&path), self.budget).is_ok() {
                    self.gate.add(step, clip.bytes);
                }
            }
        }
        invalidate_usage();
    }
}

/// Replaces the plan with one for `clips` (the full list, any order).
pub fn start_prefetch(clips: Vec<PrefetchClip>, sink: Sink, prepare: PrepareEditor) {
    *library().lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::new(Library { clips, sink, prepare }));
    replan();
}

/// Re-scores the current list, evicts to the budget and restarts the fill.
pub(super) fn replan() {
    let Some(library) = library().lock().ok().and_then(|l| l.clone()) else { return };
    let root = super::cache_root();
    let budget = super::video::budget_in(&root);
    let (now, weights, memory) = (budget::now_ms(), Weights::default(), history::snapshot());
    let facts: Vec<ClipFacts> = library
        .clips
        .iter()
        .map(|c| ClipFacts { name: c.name.to_lowercase(), game: c.game_key.clone(), mtime_ms: c.mtime_secs * 1000, favourite: c.favourite, edited: c.edited })
        .collect();
    let clip_scores = score_all(&facts, &memory, now, &weights);
    *scores().lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = library.clips.iter().map(clip_id).zip(clip_scores.iter().copied()).collect();
    budget::enforce(&root, budget, 0);
    let inventory = budget::inventory(&root, &score_of);
    let gate = Arc::new(Gate::new(budget, inventory.total(), inventory.tier_bytes(Tier::Video)));
    let owners: HashSet<String> = budget::read_media(&root).owners.into_values().collect();
    let mut ranked: Vec<usize> = (0..library.clips.len()).collect();
    ranked.sort_by(|a, b| clip_scores[*b].total_cmp(&clip_scores[*a]).then(library.clips[*b].mtime_secs.cmp(&library.clips[*a].mtime_secs)));
    let order = plan(&library.clips, &ranked, &|step, clip| cached(&root, &owners, step, clip));
    let mut by_lane: HashMap<LaneKey, VecDeque<(Step, usize)>> = HashMap::new();
    let mut keys: HashMap<usize, LaneKey> = HashMap::new();
    for (step, index) in order {
        let key = keys.entry(index).or_insert_with(|| lane::key_for(Path::new(&library.clips[index].path))).clone();
        by_lane.entry(key).or_default().push_back((step, index));
    }
    let mut lanes = active().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    for key in lanes.drain() {
        if !by_lane.contains_key(&key) {
            lane::lane(&key).set_idle(None);
        }
    }
    for (key, order) in by_lane {
        let feed = Feed { root: root.clone(), budget, library: library.clone(), owners: owners.clone(), order: Mutex::new(order), gate: gate.clone() };
        lane::lane(&key).set_idle(Some(Arc::new(Arc::new(feed))));
        lanes.insert(key);
    }
    drop(lanes);
    *favoured().lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = memory.favoured_games(now, &weights, 2);
    invalidate_usage();
}

/// Stops filling until the next re-plan (after a Settings "clear").
pub(super) fn stop() {
    for key in active().lock().unwrap_or_else(|poisoned| poisoned.into_inner()).drain() {
        lane::lane(&key).set_idle(None);
    }
}

fn favoured() -> &'static Mutex<Vec<String>> {
    static FAVOURED: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    FAVOURED.get_or_init(|| Mutex::new(Vec::new()))
}

/// Cheap per-tier totals for Settings, refreshed in the background when stale.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Summary {
    pub thumbs: u64,
    pub scrub: u64,
    pub editor: u64,
    pub video: u64,
    pub total: u64,
    /// Clips with more than a thumbnail cached.
    pub preloaded: usize,
}

static SUMMARY_STALE: AtomicBool = AtomicBool::new(true);
static SUMMARY_REFRESHING: AtomicBool = AtomicBool::new(false);

fn summary_cell() -> &'static Mutex<Option<Summary>> {
    static SUMMARY: OnceLock<Mutex<Option<Summary>>> = OnceLock::new();
    SUMMARY.get_or_init(|| Mutex::new(None))
}

pub(super) fn invalidate_usage() {
    SUMMARY_STALE.store(true, Ordering::Relaxed);
}

pub(super) fn compute_summary(root: &Path) -> Summary {
    let inventory = budget::inventory(root, &|_| 0.0);
    let media = budget::read_media(root);
    let preloaded: HashSet<&str> =
        inventory.units.iter().filter(|u| u.tier != Tier::Thumb).filter_map(|u| budget::unit_clip(&u.name, &media)).collect();
    Summary {
        thumbs: inventory.tier_bytes(Tier::Thumb),
        scrub: inventory.tier_bytes(Tier::Scrub),
        editor: inventory.tier_bytes(Tier::EditorMedia),
        video: inventory.tier_bytes(Tier::Video),
        total: inventory.total(),
        preloaded: preloaded.len(),
    }
}

/// Latest totals; the first call walks the cache, later ones never block (a stale snapshot
/// is refreshed on a background thread).
pub(super) fn summary() -> Summary {
    let current = *summary_cell().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(current) = current else {
        let fresh = compute_summary(&super::cache_root());
        SUMMARY_STALE.store(false, Ordering::Relaxed);
        *summary_cell().lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(fresh);
        return fresh;
    };
    if SUMMARY_STALE.load(Ordering::Relaxed) && !SUMMARY_REFRESHING.swap(true, Ordering::AcqRel) {
        SUMMARY_STALE.store(false, Ordering::Relaxed);
        let _ = std::thread::Builder::new().name("recall-cache-usage".into()).spawn(|| {
            let fresh = compute_summary(&super::cache_root());
            *summary_cell().lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(fresh);
            SUMMARY_REFRESHING.store(false, Ordering::Release);
        });
    }
    current
}

/// One human line for Settings, e.g. "Preloaded 312 clips · VALORANT, Rocket League favoured".
pub fn smart_status() -> String {
    let summary = summary();
    let games = favoured().lock().map(|g| g.clone()).unwrap_or_default();
    let noun = if summary.preloaded == 1 { "clip" } else { "clips" };
    let mut line = format!("Preloaded {} {noun}", summary.preloaded);
    if games.is_empty() {
        line.push_str(" · learning what you watch");
    } else {
        line.push_str(&format!(" · {} favoured", games.join(", ")));
    }
    if recorder_busy() {
        line.push_str(" · paused while recording");
    }
    line
}
