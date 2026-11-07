//! Storage cap + auto-clean (`[storage] cap_gb`: 0 = off, else at least
//! [`STORAGE_CAP_MIN_GB`]). After each saved clip (debounced) and hourly,
//! when the top-level Recall clips in `recording.output_dir` total more than
//! the cap, the OLDEST clips go to the Recycle Bin, with their recorder
//! thumbnails and `.recall-edit.json` / `.recall-marks.json` sidecars, through
//! `IFileOperation` with a sink that refuses a permanent delete
//! (`win_fileop.rs`), until the folder is under the cap. A clip
//! goes only when its volume has a Recycle Bin that will take it
//! (`win_recycle.rs`); otherwise the run stops there and nothing is deleted.
//!
//! Only Recall-named clips count and can go ([`is_recall_clip`]:
//! `<GAME>__YYYY-MM-DD__HH-MM-SS[...].mp4`, long recordings included); other
//! videos in the folder are left alone. Never touched: the newest clip and any
//! clip modified in the last [`RECENT_SECS`], favourites
//! (`%APPDATA%\Recall\library.json`, keyed by file name, `favourite: true`;
//! `RECALL_LIBRARY_META` overrides the path), clips with an edit sidecar,
//! anything in a subfolder (`exports\`, `screenshots\`, `thumbs\` except a
//! removed clip's own thumbnails), and `.part` files. An unreadable or
//! corrupt library file -- or a missing one beside the app's quarantined
//! `library.json.bad` -- cancels the run (favourites unknown = delete
//! nothing). `FOF_WANTNUKEWARNING` makes Windows ask before it would delete
//! permanently (a clip bigger than the Recycle Bin), never silently.
//!
//! Runs on its own background-I/O thread (`record_janitor.rs`, which also
//! checks free space: `record_free_space.rs`; the clip folder is an SMR HDD); one
//! directory listing per run, no per-file stats. Results (on `mod:"session"`):
//! a log line and `{"type":"storage_cleaned","removed":N,"freed_bytes":B,
//! "through":T,"used_bytes":U,"cap_bytes":C}` (`through` = last-write Unix
//! seconds of the newest clip that went: "recordings from <date> and
//! earlier"). Before the cap is reached, [`Warner`] sends
//! `{"type":"storage_warning","level":80|90|100,"used_bytes":U,"cap_bytes":C,
//! "next_through":T|null}` once per crossing (100 = over the cap with nothing
//! left that may go; `next_through` = the clip that would go first). A
//! `storage_preview{cap_gb}` job plans without moving anything and answers the
//! asking client with `storage_preview{cap_gb,used_bytes,removed,freed_bytes,through}`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::log;
use crate::record_config::STORAGE_CAP_MIN_GB;

pub const GIB: u64 = 1 << 30;
/// A clip modified this recently (seconds) is never removed: a save, trim or
/// export may still be landing, and the user has not seen it yet.
pub const RECENT_SECS: u64 = 600;
/// Wait after a save (bursts coalesce; thumbnails land first).
pub const AFTER_SAVE: Duration = Duration::from_secs(5);
pub const HOURLY: Duration = Duration::from_secs(3600);

/// One top-level Recall clip from the listing.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipFile {
    pub name: String,
    pub bytes: u64,
    /// Unix seconds (last write).
    pub modified: u64,
}

/// One listing of the clip folder.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Listing {
    pub clips: Vec<ClipFile>,
    /// Every top-level file name (sidecars included).
    pub names: Vec<String>,
    /// File names inside `thumbs\`.
    pub thumbs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// Clip names to recycle, oldest first.
    pub remove: Vec<String>,
    pub freed: u64,
    pub total: u64,
    /// Last-write time of the newest clip in `remove` (the "and earlier" date).
    pub through: Option<u64>,
    /// Last-write time of the oldest clip that may go but stays this run (the
    /// next one out once the folder grows).
    pub next: Option<u64>,
}

impl Plan {
    /// Over the cap with nothing left that may go (favourites, edits, new clips).
    pub fn stuck(&self, cap: u64) -> bool {
        cap > 0 && self.total.saturating_sub(self.freed) > cap && self.next.is_none()
    }
}

/// `<GAME>__YYYY-MM-DD__HH-MM-SS[...].mp4` (case-insensitive): replay clips
/// (`...-275__<hex>.mp4`), trims, renamed takes and long recordings
/// (`...__rec.mp4`, `...__rec-N.mp4`). Never a `.part`.
pub fn is_recall_clip(name: &str) -> bool {
    const SHAPE: &[u8] = b"__dddd-dd-dd__dd-dd-dd";
    let lower = name.to_ascii_lowercase();
    let Some(stem) = lower.strip_suffix(".mp4") else { return false };
    let b = stem.as_bytes();
    // From 1: the game prefix is never empty.
    (1..b.len()).any(|i| {
        b.len() >= i + SHAPE.len()
            && SHAPE.iter().zip(&b[i..]).all(|(&s, &c)| if s == b'd' { c.is_ascii_digit() } else { s == c })
    })
}

/// Indices of the clips to remove: oldest unprotected first, until the total
/// is at or under `cap`. The total counts protected clips too. Never picked:
/// the newest clip, and clips modified less than [`RECENT_SECS`] before
/// `now_unix` (or after it).
pub fn select_oldest(clips: &[ClipFile], cap: u64, now_unix: u64, protected: impl Fn(&ClipFile) -> bool) -> Vec<usize> {
    let mut total: u64 = clips.iter().map(|c| c.bytes).sum();
    if cap == 0 || total <= cap {
        return Vec::new();
    }
    let mut out = Vec::new();
    for i in candidates(clips, now_unix, protected) {
        if total <= cap {
            break;
        }
        total -= clips[i].bytes;
        out.push(i);
    }
    out
}

/// Indices of every clip that may go, oldest first (see [`select_oldest`]).
pub fn candidates(clips: &[ClipFile], now_unix: u64, protected: impl Fn(&ClipFile) -> bool) -> Vec<usize> {
    let by_age = |a: &usize, b: &usize| clips[*a].modified.cmp(&clips[*b].modified).then_with(|| clips[*a].name.cmp(&clips[*b].name));
    let newest = (0..clips.len()).max_by(by_age);
    let settled = |c: &ClipFile| c.modified.saturating_add(RECENT_SECS) <= now_unix;
    let mut order: Vec<usize> =
        (0..clips.len()).filter(|&i| Some(i) != newest && settled(&clips[i]) && !protected(&clips[i])).collect();
    order.sort_by(by_age);
    order
}

/// Lowercase file names marked `favourite: true` in `library.json`.
pub fn favourites(json_bytes: &[u8]) -> Result<HashSet<String>, String> {
    let Value::Object(map) = serde_json::from_slice::<Value>(json_bytes).map_err(|e| format!("library.json: {e}"))? else {
        return Err("library.json is not an object".into());
    };
    Ok(map
        .into_iter()
        .filter(|(_, v)| v.get("favourite").and_then(Value::as_bool) == Some(true))
        .map(|(name, _)| name.to_lowercase())
        .collect())
}

/// Lowercase clip names that have an edit sidecar: `<name>.recall-edit.json`
/// or `<stem>.recall-edit.json` (stem + `.mp4`).
pub fn edited(names: &[String]) -> HashSet<String> {
    names
        .iter()
        .filter_map(|n| n.to_lowercase().strip_suffix(".recall-edit.json").map(str::to_owned))
        .map(|base| if base.ends_with(".mp4") { base } else { format!("{base}.mp4") })
        .collect()
}

/// `<stem>-<epoch>-thumb.jpg` -> lowercase stem (the recorder's thumbnail rule).
fn thumb_stem(name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    let base = lower.strip_suffix(".jpg")?.strip_suffix("-thumb")?;
    let (stem, epoch) = base.rsplit_once('-')?;
    (!stem.is_empty() && epoch.len() >= 9 && epoch.chars().all(|c| c.is_ascii_digit())).then(|| stem.to_string())
}

/// Plan a run over `listing` with `favs` protected, at Unix time `now_unix`.
pub fn plan(listing: &Listing, favs: &HashSet<String>, cap: u64, now_unix: u64) -> Plan {
    let edits = edited(&listing.names);
    let protected = |c: &ClipFile| {
        let key = c.name.to_lowercase();
        favs.contains(&key) || edits.contains(&key)
    };
    let picks = select_oldest(&listing.clips, cap, now_unix, protected);
    let next = candidates(&listing.clips, now_unix, protected).into_iter().find(|i| !picks.contains(i)).map(|i| listing.clips[i].modified);
    Plan {
        remove: picks.iter().map(|&i| listing.clips[i].name.clone()).collect(),
        freed: picks.iter().map(|&i| listing.clips[i].bytes).sum(),
        total: listing.clips.iter().map(|c| c.bytes).sum(),
        through: picks.iter().map(|&i| listing.clips[i].modified).max(),
        next,
    }
}

/// Warning levels, percent of the cap: a toast at each, before anything goes.
pub const WARN_LEVELS: [u8; 2] = [80, 90];
/// Over the cap and stuck (nothing left that may go).
pub const LEVEL_STUCK: u8 = 100;
/// A level warns again only after usage fell this many points below it.
pub const REARM_POINTS: u64 = 5;

/// One warning per threshold crossing (per cap; a new cap starts over).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Warner {
    cap: u64,
    level: u8,
    /// The "couldn't move old clips" notice was shown and no run has worked since.
    refused: bool,
}

/// The notice when a clip cannot go to the Recycle Bin (shown once until a run works again).
pub const REFUSED_NOTICE: &str = "Couldn't move old clips to the Recycle Bin, so your limit wasn't applied.";
/// The notice while an earlier Recycle Bin move is still stuck (shown once per streak, like [`REFUSED_NOTICE`]).
pub const BUSY_NOTICE: &str = "The Recycle Bin is busy, try again: old clips weren't moved yet, so your limit wasn't applied.";

impl Warner {
    /// The level to warn about now, if any. `pruned`: this run moved clips
    /// (their own toast says so), so the level is only recorded.
    pub fn update(&mut self, used: u64, cap: u64, stuck: bool, pruned: bool) -> Option<u8> {
        if cap != self.cap {
            *self = Warner { cap, level: 0, refused: self.refused };
        }
        if cap == 0 {
            return None;
        }
        let pct = (used as u128 * 100 / cap as u128).min(u128::from(u16::MAX)) as u64;
        let now = if stuck { LEVEL_STUCK } else { WARN_LEVELS.iter().rev().copied().find(|&l| pct >= u64::from(l)).unwrap_or(0) };
        if now > self.level {
            self.level = now;
            return (!pruned).then_some(now);
        }
        if now < self.level && (self.level == LEVEL_STUCK || pct + REARM_POINTS < u64::from(self.level)) {
            self.level = now;
        }
        None
    }
}

impl Warner {
    /// Whether to show [`REFUSED_NOTICE`] for this run: once, until a run goes through again.
    pub fn refusal(&mut self, refused: bool) -> bool {
        let show = refused && !self.refused;
        self.refused = refused;
        show
    }
}

/// Every file that leaves with `clip`: the clip, its recorder thumbnails and
/// its sidecars (only those present in the listing).
pub fn companions(dir: &Path, clip: &str, listing: &Listing) -> Vec<PathBuf> {
    let stem = Path::new(clip).file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    let lower = clip.to_lowercase();
    let sidecars: Vec<String> = [".recall-edit.json", ".recall-marks.json"]
        .iter()
        .flat_map(|sfx| [format!("{lower}{sfx}"), format!("{stem}{sfx}")])
        .collect();
    let mut out = vec![dir.join(clip)];
    out.extend(listing.names.iter().filter(|n| sidecars.contains(&n.to_lowercase())).map(|n| dir.join(n)));
    out.extend(
        listing.thumbs.iter().filter(|n| thumb_stem(n).as_deref() == Some(stem.as_str())).map(|n| dir.join("thumbs").join(n)),
    );
    out
}

/// One listing of `dir` (top level + `thumbs\` names); only Recall-named
/// `.mp4`s are clips. Sizes and times come from the directory enumeration.
pub fn scan(dir: &Path) -> std::io::Result<Listing> {
    let mut listing = Listing::default();
    for entry in std::fs::read_dir(dir)?.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_recall_clip(&name) {
            let modified = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
            listing.clips.push(ClipFile { name: name.clone(), bytes: meta.len(), modified });
        }
        listing.names.push(name);
    }
    if let Ok(thumbs) = std::fs::read_dir(dir.join("thumbs")) {
        listing.thumbs = thumbs.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    }
    Ok(listing)
}

/// `RECALL_LIBRARY_META`, else `%APPDATA%\Recall\library.json`.
pub fn library_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("RECALL_LIBRARY_META").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Recall").join("library.json"))
}

/// Favourites from disk ([`favourites_at`] the [`library_path`]).
fn load_favourites() -> Result<HashSet<String>, String> {
    let Some(path) = library_path() else { return Ok(HashSet::new()) };
    favourites_at(&path)
}

/// `library.json` -> `library.json.bad` (where the app moves a corrupt file).
pub fn quarantine_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_else(|| "library.json".into());
    name.push(".bad");
    path.with_file_name(name)
}

/// Favourites in the library file at `path`. Missing means none -- unless the
/// app quarantined a corrupt copy beside it, which (like an unreadable or
/// corrupt file) leaves favourites unknown, so the run is cancelled.
pub fn favourites_at(path: &Path) -> Result<HashSet<String>, String> {
    match std::fs::read(path) {
        Ok(bytes) => favourites(&bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let bad = quarantine_path(path);
            match bad.try_exists() {
                Ok(false) => Ok(HashSet::new()),
                Ok(true) => Err(format!("{} is missing and {} holds a quarantined corrupt copy", path.display(), bad.display())),
                Err(e) => Err(format!("{}: {e}", bad.display())),
            }
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Move `files` (one clip and its companions) to the Recycle Bin in one shell operation.
pub fn recycle(files: &[PathBuf]) -> Result<(), RecycleError> {
    from_outcome(crate::win_fileop::recycle_files(files).outcome)
}

/// Why one Recycle Bin call did not go through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecycleError {
    /// The clip would not go to the Recycle Bin: the `IFileOperation` sink
    /// refused a delete Windows would have made permanent, or the call was
    /// cancelled. The run stops and the user is told, as for a precheck refusal.
    Refused(String),
    Failed(String),
}

/// One `win_fileop` outcome as the cap run's result.
pub fn from_outcome(outcome: crate::win_fileop::Outcome) -> Result<(), RecycleError> {
    use crate::win_fileop::Outcome;
    match outcome {
        Outcome::Recycled => Ok(()),
        Outcome::Refused(why) => Err(RecycleError::Refused(why)),
        Outcome::Failed(why) => Err(RecycleError::Failed(why)),
    }
}

/// What a run did.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub removed: usize,
    pub freed: u64,
    /// Newest clip that went (last-write Unix seconds).
    pub through: Option<u64>,
    /// Counted clips after the run, and the cap, in bytes.
    pub used: u64,
    pub cap: u64,
    /// Oldest clip that may go next.
    pub next: Option<u64>,
    pub stuck: bool,
    /// Why the run stopped before a clip that cannot go to the Recycle Bin.
    pub refused: Option<String>,
}

/// List and plan (nothing moved). Errors leave favourites unknown or the folder unread.
pub(crate) fn plan_dir(dir: &Path, cap_gb: u32) -> Result<(Plan, Listing), String> {
    if cap_gb < STORAGE_CAP_MIN_GB {
        // `set_config` rejects this; a hand-edited config.toml does not.
        return Err(format!("storage.cap_gb {cap_gb} is below the {STORAGE_CAP_MIN_GB} GiB minimum; nothing removed"));
    }
    let favs = load_favourites().map_err(|e| format!("favourites unknown ({e}); nothing removed"))?;
    plan_listing(dir, &favs, cap_gb as u64 * GIB)
}

/// The read-only half of a run (the dry run is only this): list the folder and choose.
fn plan_listing(dir: &Path, favs: &HashSet<String>, cap: u64) -> Result<(Plan, Listing), String> {
    let listing = scan(dir).map_err(|e| format!("list {}: {e}", dir.display()))?;
    let now_unix = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    Ok((plan(&listing, favs, cap, now_unix), listing))
}

/// Lists and plans a run (production: [`plan_dir`]; tests inject one).
pub(crate) type PlanFn = fn(&Path, u32) -> Result<(Plan, Listing), String>;
/// Moves files to the Recycle Bin (production: [`recycle`]; tests inject one).
pub(crate) type RecycleFn = fn(&[PathBuf]) -> Result<(), RecycleError>;
pub(crate) use crate::win_recycle::{BinCheck, SizeFn};

/// Bytes one Recycle Bin call moves: the clip (its listed size) and its
/// companions as they are on disk (a missing one counts 0).
fn companions_bytes(files: &[PathBuf], clip: Option<&ClipFile>, size: SizeFn) -> Result<u64, String> {
    let clip_bytes = match clip {
        Some(c) => c.bytes,
        None => crate::win_recycle::total_size(&files[..1.min(files.len())], size)?,
    };
    Ok(clip_bytes + crate::win_recycle::total_size(files.get(1..).unwrap_or(&[]), size)?)
}

/// One cap run: one Recycle Bin call per clip (with its thumbnails and
/// sidecars), oldest first. `wanted` is asked before EVERY call: the listing
/// can take long on a sleeping NAS and each call takes a moment on an HDD,
/// and once the cap or folder is no longer the user's setting nothing more
/// moves. `bin` is asked before every call too: a clip whose volume has no
/// Recycle Bin that will take it (a share, a removable drive, a bin turned
/// off or too small) stops the run, so nothing is ever deleted for good.
/// The first failed call ends the run.
pub(crate) fn run_once(dir: &Path, cap_gb: u32, plan: PlanFn, wanted: &dyn Fn() -> bool, bin: (BinCheck, SizeFn), recycle: RecycleFn) -> Result<Outcome, String> {
    let (bin, size) = bin;
    let (plan, listing) = plan(dir, cap_gb)?;
    let cap = cap_gb as u64 * GIB;
    let stuck = plan.stuck(cap);
    let mut out = Outcome { removed: 0, freed: 0, through: None, used: plan.total, cap, next: plan.next, stuck, refused: None };
    let mut failed = None;
    for name in &plan.remove {
        if !wanted() {
            log!("storage: the storage limit or folder changed during the run; stopped after {} clip(s)", out.removed);
            break;
        }
        let files = companions(dir, name, &listing);
        let bytes = companions_bytes(&files, listing.clips.iter().find(|c| c.name == *name), size);
        if let Err(why) = bytes.and_then(|bytes| bin(dir, bytes)) {
            out.refused = Some(why);
            break;
        }
        let moved = recycle(&files);
        // A declined or cancelled call moved nothing; otherwise count what left the folder.
        let gone = !matches!(moved, Err(RecycleError::Refused(_)));
        if let Some(c) = listing.clips.iter().find(|c| c.name == *name).filter(|c| gone && !dir.join(&c.name).exists()) {
            out.removed += 1;
            out.freed += c.bytes;
            out.through = out.through.max(Some(c.modified));
        }
        match moved {
            Ok(()) => {}
            Err(RecycleError::Refused(why)) => {
                out.refused = Some(why);
                break;
            }
            Err(RecycleError::Failed(why)) => {
                failed = Some(why);
                break;
            }
        }
    }
    out.used = plan.total - out.freed;
    // A partial run still reports what went (the toast names the date); the error follows.
    match failed {
        Some(e) if out.removed == 0 => Err(e),
        Some(e) => {
            log!("storage: {e} (after {} clip(s) went)", out.removed);
            Ok(out)
        }
        None => Ok(out),
    }
}

pub fn cleaned_event(o: &Outcome) -> String {
    json!({"type": "storage_cleaned", "removed": o.removed, "freed_bytes": o.freed, "through": o.through, "used_bytes": o.used, "cap_bytes": o.cap})
        .to_string()
}

pub fn warning_event(level: u8, o: &Outcome) -> String {
    json!({"type": "storage_warning", "level": level, "used_bytes": o.used, "cap_bytes": o.cap, "next_through": o.next}).to_string()
}

pub fn preview_event(cap_gb: u32, plan: &Plan) -> String {
    json!({"type": "storage_preview", "cap_gb": cap_gb, "used_bytes": plan.total, "removed": plan.remove.len(), "freed_bytes": plan.freed, "through": plan.through})
        .to_string()
}

#[cfg(test)]
#[path = "record_storage_tests.rs"]
mod tests;
