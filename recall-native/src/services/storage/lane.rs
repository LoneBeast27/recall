//! I/O lanes: one worker thread per physical HDD runs every heavy read of that disk, one at
//! a time, in background mode (very low I/O priority). Queued work runs most-urgent first
//! (`Interactive` > `Visible` > `Background`; newest first within a level, so the latest
//! hover or scroll position wins). When the queue is empty and no on-demand work arrived
//! for [`IDLE_GRACE`], the lane pulls one item at a time from its idle (prefetch) source.
//! SSD paths skip lanes entirely (they run on the caller's thread).
use std::cell::{Cell, RefCell};
use std::cmp::Ordering as CmpOrdering;
use std::collections::{BinaryHeap, HashMap};
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use super::drive;
use super::io_priority;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    /// The user is waiting: hover-scrub, opening / caching a clip, export, sidecar flush.
    Interactive,
    /// Rows on screen: thumbnails and durations.
    Visible,
    /// Queued low-value work (superseded hover requests). Still ahead of idle prefetch.
    Background,
}

pub(super) type Task = Box<dyn FnOnce() + Send + 'static>;

/// Prefetch feed: asked for one task whenever the lane has been idle long enough.
pub(super) trait IdleSource: Send + Sync {
    fn next_task(&self) -> Option<Task>;
    /// True while the feed must wait (e.g. the recorder is writing); polled every grace period.
    fn paused(&self) -> bool {
        false
    }
}

/// Quiet period after the last on-demand request before prefetch resumes.
pub(super) const IDLE_GRACE: Duration = Duration::from_millis(400);

struct Slot<T> {
    priority: Priority,
    seq: u64,
    item: T,
}

impl<T> PartialEq for Slot<T> {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.seq == other.seq
    }
}
impl<T> Eq for Slot<T> {}
impl<T> PartialOrd for Slot<T> {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}
impl<T> Ord for Slot<T> {
    /// Max-heap order: more urgent priority first, then the newest submission.
    fn cmp(&self, other: &Self) -> CmpOrdering {
        other.priority.cmp(&self.priority).then(self.seq.cmp(&other.seq))
    }
}

/// Pure priority queue behind each lane (unit-tested on its own).
pub(super) struct LaneQueue<T> {
    heap: BinaryHeap<Slot<T>>,
    next: u64,
}

impl<T> LaneQueue<T> {
    pub fn new() -> Self {
        Self { heap: BinaryHeap::new(), next: 0 }
    }
    pub fn push(&mut self, priority: Priority, item: T) {
        self.next += 1;
        self.heap.push(Slot { priority, seq: self.next, item });
    }
    pub fn pop(&mut self) -> Option<(Priority, T)> {
        self.heap.pop().map(|slot| (slot.priority, slot.item))
    }
    pub fn len(&self) -> usize {
        self.heap.len()
    }
    /// Someone is waiting: an `Interactive` or `Visible` item is queued.
    pub fn has_demand(&self) -> bool {
        self.heap.peek().is_some_and(|slot| slot.priority < Priority::Background)
    }
}

struct State {
    queue: LaneQueue<Task>,
    idle: Option<Arc<dyn IdleSource>>,
    last_demand: Option<Instant>,
    closed: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

fn lock(shared: &Shared) -> MutexGuard<'_, State> {
    shared.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(super) struct Lane {
    id: u64,
    shared: Arc<Shared>,
}

static NEXT_LANE: AtomicU64 = AtomicU64::new(1);
static HDD_TASKS: AtomicU64 = AtomicU64::new(0);
static IDLE_TASKS: AtomicU64 = AtomicU64::new(0);
static HDD_BYTES: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// Lane id of the current thread (0 = not a lane thread).
    static CURRENT: Cell<u64> = const { Cell::new(0) };
    /// The current lane thread's queue, for [`demand_waiting`].
    static CURRENT_LANE: RefCell<Option<Arc<Shared>>> = const { RefCell::new(None) };
}

/// On a lane thread: on-demand work (`Interactive` / `Visible`) is queued behind the task
/// running now, which should yield if it is long (an idle warm-up copy). False elsewhere.
pub(super) fn demand_waiting() -> bool {
    CURRENT_LANE.with(|lane| lane.borrow().as_ref().is_some_and(|shared| lock(shared).queue.has_demand()))
}

impl Lane {
    /// `hdd` lanes count their tasks in [`stats`]; `grace` is the prefetch quiet period.
    pub(super) fn spawn(name: &str, hdd: bool, grace: Duration) -> Arc<Lane> {
        let id = NEXT_LANE.fetch_add(1, Ordering::Relaxed);
        let shared = Arc::new(Shared {
            state: Mutex::new(State { queue: LaneQueue::new(), idle: None, last_demand: None, closed: false }),
            wake: Condvar::new(),
        });
        let thread_shared = shared.clone();
        let spawned = std::thread::Builder::new().name(name.into()).spawn(move || worker(thread_shared, id, hdd, grace));
        if let Err(error) = spawned {
            eprintln!("start {name}: {error}");
        }
        Arc::new(Lane { id, shared })
    }

    pub(super) fn submit(&self, priority: Priority, task: Task) {
        let mut state = lock(&self.shared);
        state.queue.push(priority, task);
        state.last_demand = Some(Instant::now());
        drop(state);
        self.shared.wake.notify_one();
    }

    /// Replaces the prefetch feed (None stops prefetch).
    pub(super) fn set_idle(&self, source: Option<Arc<dyn IdleSource>>) {
        lock(&self.shared).idle = source;
        self.shared.wake.notify_one();
    }

    pub(super) fn is_current(&self) -> bool {
        CURRENT.with(|current| current.get() == self.id)
    }

    /// Runs `work` on the lane and waits for it (inline when already on this lane).
    pub(super) fn run<T: Send + 'static>(&self, priority: Priority, work: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
        if self.is_current() {
            return Ok(work());
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.submit(priority, Box::new(move || {
            let _ = tx.send(work());
        }));
        rx.recv().map_err(|_| "disk lane task failed".to_string())
    }
}

impl Drop for Lane {
    fn drop(&mut self) {
        lock(&self.shared).closed = true;
        self.shared.wake.notify_all();
    }
}

fn execute(task: Task, hdd: bool) {
    let _ = std::panic::catch_unwind(AssertUnwindSafe(task));
    if hdd {
        HDD_TASKS.fetch_add(1, Ordering::Relaxed);
    }
}

fn worker(shared: Arc<Shared>, id: u64, hdd: bool, grace: Duration) {
    io_priority::enter_background_mode();
    CURRENT.with(|current| current.set(id));
    CURRENT_LANE.with(|lane| *lane.borrow_mut() = Some(shared.clone()));
    let mut state = lock(&shared);
    loop {
        if let Some((_, task)) = state.queue.pop() {
            drop(state);
            execute(task, hdd);
            state = lock(&shared);
            continue;
        }
        if state.closed {
            return;
        }
        let Some(source) = state.idle.clone() else {
            state = shared.wake.wait(state).unwrap_or_else(|poisoned| poisoned.into_inner());
            continue;
        };
        let quiet = state.last_demand.map_or(grace, |at| at.elapsed());
        if quiet < grace || source.paused() {
            let wait = if quiet < grace { grace - quiet } else { grace };
            state = shared.wake.wait_timeout(state, wait).unwrap_or_else(|poisoned| poisoned.into_inner()).0;
            continue;
        }
        drop(state);
        let next = source.next_task();
        state = lock(&shared);
        match next {
            // On-demand work arrived while the feed was asked: it goes first.
            Some(task) if state.queue.len() > 0 => state.queue.push(Priority::Background, task),
            Some(task) => {
                drop(state);
                execute(task, hdd);
                IDLE_TASKS.fetch_add(1, Ordering::Relaxed);
                state = lock(&shared);
            }
            None => {
                let same = state.idle.as_ref().is_some_and(|current| std::ptr::addr_eq(Arc::as_ptr(current), Arc::as_ptr(&source)));
                if same {
                    state.idle = None;
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum LaneKey {
    Disk(u32),
    Volume(String),
    /// Background lane for prefetching SSD-resident clips (on-demand SSD work runs inline).
    Ssd,
}

/// Lane key for `path`: SSD volumes map to [`LaneKey::Ssd`].
pub(super) fn key_for(path: &Path) -> LaneKey {
    match drive::disk_of(path) {
        (_, _, kind) if !kind.is_hdd() => LaneKey::Ssd,
        (Some(disk), _, _) => LaneKey::Disk(disk),
        (None, Some(volume), _) => LaneKey::Volume(volume),
        (None, None, _) => LaneKey::Volume(String::new()),
    }
}

pub(super) fn lane(key: &LaneKey) -> Arc<Lane> {
    static LANES: OnceLock<Mutex<HashMap<LaneKey, Arc<Lane>>>> = OnceLock::new();
    let mut lanes = LANES.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    lanes
        .entry(key.clone())
        .or_insert_with(|| {
            let name = match key {
                LaneKey::Disk(disk) => format!("recall-disk{disk}"),
                LaneKey::Volume(volume) => format!("recall-vol-{}", volume.trim_end_matches('\\')),
                LaneKey::Ssd => "recall-ssd-prefetch".into(),
            };
            Lane::spawn(&name, *key != LaneKey::Ssd, IDLE_GRACE)
        })
        .clone()
}

/// The HDD lane for `path`, or None when it is on an SSD.
pub(super) fn hdd_lane(path: &Path) -> Option<Arc<Lane>> {
    match key_for(path) {
        LaneKey::Ssd => None,
        key => Some(lane(&key)),
    }
}

/// Queues `task` on the lane of the drive holding `path`; runs it now on an SSD path or
/// when already on that lane.
pub fn submit_io(path: &Path, priority: Priority, task: impl FnOnce() + Send + 'static) {
    match hdd_lane(path) {
        Some(lane) if !lane.is_current() => lane.submit(priority, Box::new(task)),
        _ => task(),
    }
}

/// Runs `work` on the lane of the drive holding `path` and waits; inline on an SSD path.
pub fn run_io<T: Send + 'static>(path: &Path, priority: Priority, work: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    match hdd_lane(path) {
        Some(lane) => lane.run(priority, work),
        None => Ok(work()),
    }
}

static LANE_PROBE: Mutex<Option<Instant>> = Mutex::new(None);
static LANE_WAITED: AtomicU64 = AtomicU64::new(0);

/// For the player's trace: how long a task has waited, or last waited, for the lane of `path`
/// (None on an SSD). A lane held by a long job shows here as a number that keeps growing.
pub fn lane_wait_ms(path: &Path) -> Option<u64> {
    let lane = hdd_lane(path)?;
    let mut sent = LANE_PROBE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(at) = *sent {
        return Some(at.elapsed().as_millis() as u64);
    }
    *sent = Some(Instant::now());
    lane.submit(Priority::Interactive, Box::new(|| {
        if let Some(at) = LANE_PROBE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take() {
            LANE_WAITED.store(at.elapsed().as_millis() as u64, Ordering::Relaxed);
        }
    }));
    Some(LANE_WAITED.load(Ordering::Relaxed))
}

/// Counters for evidence: tasks run on HDD lanes, idle-prefetch tasks, bytes this process
/// read from HDDs itself (helper processes' reads are counted as tasks only).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LaneStats {
    pub hdd_tasks: u64,
    pub idle_tasks: u64,
    pub hdd_bytes_read: u64,
}

pub fn stats() -> LaneStats {
    LaneStats {
        hdd_tasks: HDD_TASKS.load(Ordering::Relaxed),
        idle_tasks: IDLE_TASKS.load(Ordering::Relaxed),
        hdd_bytes_read: HDD_BYTES.load(Ordering::Relaxed),
    }
}

pub(super) fn count_read(bytes: u64) {
    HDD_BYTES.fetch_add(bytes, Ordering::Relaxed);
}
