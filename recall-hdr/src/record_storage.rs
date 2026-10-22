//! Storage cap + auto-clean (`[storage] cap_gb`: 0 = off, else at least
//! [`STORAGE_CAP_MIN_GB`]). After each saved clip (debounced) and hourly,
//! when the top-level Recall clips in `recording.output_dir` total more than
//! the cap, the OLDEST clips go to the Recycle Bin (`SHFileOperationW` +
//! `FOF_ALLOWUNDO`, with their recorder thumbnails and `.recall-edit.json` /
//! `.recall-marks.json` sidecars) until the folder is under the cap.
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
//! Runs on its own background-I/O thread (the clip folder is an SMR HDD); one
//! directory listing per run, no per-file stats. Results: a log line and
//! `{"type":"storage_cleaned","removed":N,"freed_bytes":B}` on `mod:"session"`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::control::PipeHub;
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
    let by_age = |a: &usize, b: &usize| clips[*a].modified.cmp(&clips[*b].modified).then_with(|| clips[*a].name.cmp(&clips[*b].name));
    let newest = (0..clips.len()).max_by(by_age);
    let settled = |c: &ClipFile| c.modified.saturating_add(RECENT_SECS) <= now_unix;
    let mut order: Vec<usize> =
        (0..clips.len()).filter(|&i| Some(i) != newest && settled(&clips[i]) && !protected(&clips[i])).collect();
    order.sort_by(by_age);
    let mut out = Vec::new();
    for i in order {
        if total <= cap {
            break;
        }
        total -= clips[i].bytes;
        out.push(i);
    }
    out
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
    Plan {
        remove: picks.iter().map(|&i| listing.clips[i].name.clone()).collect(),
        freed: picks.iter().map(|&i| listing.clips[i].bytes).sum(),
        total: listing.clips.iter().map(|c| c.bytes).sum(),
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

/// Move `files` to the Recycle Bin in one shell operation.
pub fn recycle(files: &[PathBuf]) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::{BOOL, PCWSTR};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING, FO_DELETE,
        SHFILEOPSTRUCTW,
    };
    if files.is_empty() {
        return Ok(());
    }
    let mut list: Vec<u16> = Vec::new();
    for f in files {
        list.extend(f.as_os_str().encode_wide());
        list.push(0);
    }
    list.push(0);
    let mut op = SHFILEOPSTRUCTW {
        hwnd: HWND(std::ptr::null_mut()),
        wFunc: FO_DELETE,
        pFrom: PCWSTR(list.as_ptr()),
        pTo: PCWSTR::null(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI | FOF_WANTNUKEWARNING).0 as u16,
        fAnyOperationsAborted: BOOL(0),
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: PCWSTR::null(),
    };
    // SAFETY: `list` is a double-NUL-terminated wide path list alive for the call.
    let code = unsafe { SHFileOperationW(&mut op) };
    if code != 0 || op.fAnyOperationsAborted.as_bool() {
        return Err(format!("Recycle Bin move failed (shell error {code:#x})"));
    }
    Ok(())
}

/// What a run did.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub removed: usize,
    pub freed: u64,
}

fn run_once(dir: &Path, cap_gb: u32) -> Result<Outcome, String> {
    if cap_gb < STORAGE_CAP_MIN_GB {
        // `set_config` rejects this; a hand-edited config.toml does not.
        return Err(format!("storage.cap_gb {cap_gb} is below the {STORAGE_CAP_MIN_GB} GiB minimum; nothing removed"));
    }
    let favs = load_favourites().map_err(|e| format!("favourites unknown ({e}); nothing removed"))?;
    let listing = scan(dir).map_err(|e| format!("list {}: {e}", dir.display()))?;
    let now_unix = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let plan = plan(&listing, &favs, cap_gb as u64 * GIB, now_unix);
    if plan.remove.is_empty() {
        return Ok(Outcome { removed: 0, freed: 0 });
    }
    let files: Vec<PathBuf> = plan.remove.iter().flat_map(|c| companions(dir, c, &listing)).collect();
    recycle(&files)?;
    let removed = plan.remove.iter().filter(|c| !dir.join(c).exists()).count();
    let freed = listing.clips.iter().filter(|c| plan.remove.contains(&c.name) && !dir.join(&c.name).exists()).map(|c| c.bytes).sum();
    Ok(Outcome { removed, freed })
}

pub fn cleaned_event(o: &Outcome) -> String {
    json!({"type": "storage_cleaned", "removed": o.removed, "freed_bytes": o.freed}).to_string()
}

struct Job {
    dir: PathBuf,
    cap_gb: u32,
}

/// Schedules runs (after saves, hourly, on demand) onto one worker thread.
pub struct Janitor {
    tx: Sender<Job>,
    after_save: Option<Instant>,
    next_hourly: Instant,
}

impl Janitor {
    pub fn start(hub: Arc<PipeHub>) -> Self {
        let (tx, rx) = channel::<Job>();
        let _ = std::thread::Builder::new().name("recall-storage".into()).spawn(move || {
            background_io();
            while let Ok(mut job) = rx.recv() {
                while let Ok(newer) = rx.try_recv() {
                    job = newer; // coalesce queued runs
                }
                match run_once(&job.dir, job.cap_gb) {
                    Ok(o) if o.removed > 0 => {
                        log!("storage: cap {} GiB -> {} clip(s) to the Recycle Bin, {} bytes freed", job.cap_gb, o.removed, o.freed);
                        hub.event("session", &cleaned_event(&o));
                    }
                    Ok(_) => log!("storage: {} is within its {} GiB cap", job.dir.display(), job.cap_gb),
                    Err(e) => {
                        log!("storage: clean-up skipped -- {e}");
                        hub.event("session", &crate::record_status::error_event(&format!("storage clean-up: {e}")));
                    }
                }
            }
        });
        Janitor { tx, after_save: None, next_hourly: Instant::now() + HOURLY }
    }

    pub fn saved(&mut self, now: Instant) {
        self.after_save = Some(now + AFTER_SAVE);
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.after_save
    }

    /// Run now (cap changed / pipe `storage_clean`); 0 = off.
    pub fn run_now(&mut self, dir: &str, cap_gb: u32, now: Instant) {
        self.after_save = None;
        self.next_hourly = now + HOURLY;
        if cap_gb > 0 {
            let _ = self.tx.send(Job { dir: PathBuf::from(dir), cap_gb });
        }
    }

    pub fn tick(&mut self, dir: &str, cap_gb: u32, now: Instant) {
        let due = self.after_save.is_some_and(|t| now >= t) || now >= self.next_hourly;
        if due {
            self.run_now(dir, cap_gb, now);
        }
    }
}

/// Background I/O + CPU priority for this thread (the HDD stays responsive).
fn background_io() {
    use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN};
    // SAFETY: the current-thread pseudo-handle needs no closing.
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
}

#[cfg(test)]
#[path = "record_storage_tests.rs"]
mod tests;
