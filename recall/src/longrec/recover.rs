//! Recovery at engine start (part of `longrec`): every output file the in-progress list
//! (`encoder::in_progress`) names, wherever it is, then once the scan for `.part` files left by
//! builds before that list.
//!
//! - A long recording: repaired in place (`restore`), renamed to `*__rec.mp4` (or
//!   `*-recovered.mp4`), its bookmarks written from the list, `recording_restored` sent for it.
//!   One with nothing playable in it: a diagnostic line and the Recycle Bin.
//! - A replay save: rebuilt from its packet index (`encoder::replay_rebuild`).
//! - Nothing here starts a recording: the app offers Resume recording, and only a click sends
//!   `record_start{continues}` ([`next_part_of`]).
//! - The one-time scan ([`legacy_scan`]) never recycles: it recovers or keeps and logs.
use std::path::{Path, PathBuf};

use super::restore::{self, Repair};
use super::{report, Event};
use crate::encoder::in_progress::{self, Entry, Kind};
use crate::naming;
use crate::protocol::{emit_or_log, Outbound};

/// One long recording recovered at this start.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Recovered {
    pub clip: PathBuf,
    /// Its length after the repair (None: not repaired, unknown).
    pub seconds: Option<f64>,
    pub repaired: bool,
}

/// What one start recovered, and the lines to log.
#[derive(Debug, Default)]
pub(crate) struct Report {
    pub long: Vec<Recovered>,
    /// Replay clips rebuilt or adopted.
    pub clips: Vec<PathBuf>,
    pub logs: Vec<String>,
}

/// Recover what an earlier run left (background thread: the clip folder may be a sleeping HDD).
/// The list is read here, before this engine writes anything, so a file a recorder of this run
/// opens later is never this recovery's; the replay index slot is handed over first.
pub fn recover_parts(out_dir: String, json: bool) {
    let mut logs = Vec::new();
    let entries = start_entries(&mut logs);
    let staging = crate::encoder::staging::recovery_dir(std::env::var(crate::encoder::staging::ENV).ok().as_deref(), std::env::current_exe().ok().as_deref());
    let _ = std::thread::Builder::new().name("recall-longrec-recover".into()).spawn(move || {
        let mut found = run(&entries, Path::new(&out_dir), staging.as_deref());
        logs.append(&mut found.logs);
        found.logs = logs;
        emit(json, found);
    });
}

/// The list as this start found it, with the replay index slot handed to its save first.
pub(crate) fn start_entries(logs: &mut Vec<String>) -> Vec<Entry> {
    match in_progress::try_list() {
        Ok(entries) => claim_replay_slots(entries, logs),
        Err(e) => {
            // An unclaimed save may still need the slot: this run's saves write elsewhere.
            crate::encoder::replay_index::use_run_slot();
            logs.push(format!("in-progress list not read at start ({e}); recovered at a later start; saves use their own index slot"));
            Vec::new()
        }
    }
}

/// A replay save whose packet index is still in the slot gets its own copy before the next
/// save overwrites it (`replay_index::claim`).
pub(crate) fn claim_replay_slots(entries: Vec<Entry>, logs: &mut Vec<String>) -> Vec<Entry> {
    use crate::encoder::replay_index;
    entries
        .into_iter()
        .map(|mut entry| {
            let Some(base) = entry.index.clone().map(PathBuf::from).filter(|b| entry.kind == Kind::Replay && replay_index::is_slot(b)) else { return entry };
            // Only the save whose index the slot holds may take it (a stale entry never does).
            if !replay_index::belongs_to(&base, &entry.part()) {
                return entry;
            }
            match replay_index::claim(&base) {
                Ok(own) => {
                    entry.index = Some(own.to_string_lossy().into_owned());
                    let index = entry.index.clone();
                    let _ = in_progress::update(&entry.part(), |e| e.index = index);
                }
                Err(e) => {
                    // The slot still holds this save's index: this run's saves write elsewhere.
                    crate::encoder::replay_index::use_run_slot();
                    logs.push(format!("replay save {}: packet index not kept ({e}); saves use their own index slot", entry.path));
                }
            }
            entry
        })
        .collect()
}

/// Recover `entries`, then (once) what older builds left in `out_dir` and `staging`.
pub(crate) fn run(entries: &[Entry], out_dir: &Path, staging: Option<&Path>) -> Report {
    let mut found = Report::default();
    // The one-time scan first (it leaves the listed parts alone): a rescan the entries below ask
    // for then stands for the next start.
    legacy_scan(out_dir, staging, &mut found);
    for entry in entries {
        match entry.kind {
            Kind::Long => long_from_entry(entry, &mut found),
            Kind::Replay => crate::encoder::replay_rebuild::recover_entry(entry, &mut found.clips, &mut found.logs),
        }
    }
    found
}

fn emit(json: bool, found: Report) {
    for line in found.logs {
        report(json, Event::Log(line));
    }
    for r in &found.long {
        let out = Outbound::RecordingRestored { path: r.clip.to_string_lossy().into_owned(), seconds: r.seconds, repaired: r.repaired };
        emit_or_log(json, &out, || format!("long recording recovered: {}", r.clip.display()));
    }
    let paths: Vec<String> = found.long.iter().map(|r| &r.clip).chain(&found.clips).map(|p| p.to_string_lossy().into_owned()).collect();
    if let Some(out) = recovered_outbound(paths) {
        emit_or_log(json, &out, || "recovered recordings and clips".into());
    }
}

/// One `recording_recovered` for every file one start recovered (none: nothing to say).
pub(crate) fn recovered_outbound(paths: Vec<String>) -> Option<Outbound> {
    let path = paths.first()?.clone();
    Some(Outbound::RecordingRecovered { path, paths })
}

/// Why `entry` is not a long recording's part Recall wrote (it is then never acted on).
fn invalid_long(entry: &Entry) -> Option<String> {
    let part = entry.part();
    let name = part.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !naming::is_longrec_part(&name) {
        return Some(format!("{} is not a long recording's .part", part.display()));
    }
    let tail = restore::tail_path(&part);
    match &entry.undo {
        Some(u) if !in_progress::same_path(&u.tail, &tail.to_string_lossy()) => Some(format!("its undo record names {} as its tail", u.tail)),
        _ => None,
    }
}

fn long_from_entry(entry: &Entry, found: &mut Report) {
    let part = entry.part();
    if let Some(why) = invalid_long(entry) {
        found.logs.push(format!("in-progress entry {} not acted on: {why}; dropped from the list", entry.path));
        let _ = in_progress::remove(&part);
        return;
    }
    if !part.exists() && !part.parent().is_some_and(Path::is_dir) {
        // Its folder is gone: a drive not mounted yet. Kept for the start that sees it again.
        found.logs.push(format!("long recording {}: its folder is not there now; kept listed", part.display()));
        return;
    }
    if !part.exists() {
        // Finished after all (the list was not updated), or gone: nothing to recover.
        let finished = PathBuf::from(entry.path.strip_suffix(naming::PART_EXT).unwrap_or(&entry.path));
        if finished.exists() && !has_recovered_sibling(&finished) {
            write_marks(&finished, entry, None, &mut found.logs);
        } else if !finished.exists() {
            found.logs.push(format!("long recording {} was listed but is gone", part.display()));
        }
        let _ = in_progress::remove(&part);
        return;
    }
    if let Err(e) = recover_long(&part, false, found) {
        found.logs.push(format!("long recording {} not recovered: {e}", part.display()));
    }
}

/// Repair `part`, write its bookmarks, rename it into place. `legacy` (the one-time scan): one
/// with nothing playable is kept and logged, never recycled.
fn recover_long(part: &Path, legacy: bool, found: &mut Report) -> std::io::Result<PathBuf> {
    if !exclusive(part) {
        return Err(std::io::Error::other("open by a running recorder"));
    }
    let entry = in_progress::get(part);
    let (seconds, repaired) = match restore::repair(part) {
        Repair::Repaired { seconds } => (Some(seconds), true),
        Repair::Whole { seconds } => (seconds, true),
        Repair::AsIs(why) => {
            found.logs.push(format!("long recording {}: not repaired ({why}); recovered as it is", part.display()));
            (None, false)
        }
        // Only a test hook stops a repair midway; recovered as it is.
        Repair::Interrupted => (None, false),
        // An earlier repair that cannot be undone: left as it is, listed, its tail beside it.
        Repair::Stuck(why) => return Err(std::io::Error::other(format!("kept listed: {why}"))),
        Repair::Unrecoverable(why) if legacy => {
            found.logs.push(format!("long recording {} from an older build has nothing playable ({why}); kept as it is: boxes {}", part.display(), crate::encoder::recycle::layout_summary(part)));
            return Err(std::io::Error::other(why));
        }
        Repair::Unrecoverable(why) => {
            found.logs.extend(crate::encoder::recycle::dispose_unrecoverable(part, &why));
            if part.exists() {
                // Kept (it holds media but no whole fragment): the next start's scan looks again.
                if let Err(e) = in_progress::request_legacy_scan() {
                    found.logs.push(format!("the next start's scan could not be asked for: {e}"));
                }
            }
            let _ = in_progress::remove(part);
            return Err(std::io::Error::other(why));
        }
    };
    let target = naming::recovered_path(part, Path::exists);
    // Bookmarks first: a crash between the rename and the unlisting must not lose them.
    if let Some(entry) = &entry {
        write_marks(&target, entry, seconds, &mut found.logs);
    }
    std::fs::rename(part, &target)?;
    // The rename stands even when the game folder's marker can't be updated (the file then
    // stays unowned).
    let _ = crate::game_folder_rule::record_saved(&target, Some(part));
    let _ = in_progress::remove(part);
    found.long.push(Recovered { clip: target.clone(), seconds, repaired });
    Ok(target)
}

/// No other handle may write it: a live recorder's part (FILE_SHARE_READ only) is refused.
fn exclusive(part: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().read(true).write(true).share_mode(0).open(part).is_ok()
}

/// A `<stem>-recovered[-N].mp4` beside `finished`: its name was taken, so the recording this
/// entry was for went there (with its bookmarks), and `finished` is another recording.
fn has_recovered_sibling(finished: &Path) -> bool {
    let stem = finished.file_stem().map(|s| format!("{}-recovered", s.to_string_lossy().to_lowercase())).unwrap_or_default();
    let names = finished.parent().and_then(|d| std::fs::read_dir(d).ok()).into_iter().flatten().flatten();
    names.map(|e| e.file_name().to_string_lossy().to_lowercase()).any(|n| n.starts_with(&stem) && n.ends_with(".mp4"))
}

/// The bookmarks the list kept for a recording, as its sidecar (`bookmarks`).
pub(crate) fn write_marks(clip: &Path, entry: &Entry, seconds: Option<f64>, logs: &mut Vec<String>) {
    let Some(base) = entry.base_us else { return };
    let end = seconds.map_or(u64::MAX, |s| base + (s * 1e6) as u64);
    let rel = crate::bookmarks::window_marks(entry.marks.iter().copied(), base, end);
    if rel.is_empty() || crate::bookmarks::sidecar_path(clip).exists() {
        return;
    }
    if let Err(e) = crate::bookmarks::write_sidecar(clip, &rel) {
        logs.push(format!("long recording marks not written: {e}"));
    }
}

/// Every `*__rec.mp4.part` directly in `dir` or in one of its game folders (one level down,
/// "Sort clips into game folders": only folders Recall made, `game_folder_rule`), recovered
/// (or the error). Parts the in-progress list names are left to it.
#[cfg(test)]
pub(crate) fn recover_in(dir: &Path) -> Vec<(PathBuf, std::io::Result<PathBuf>)> {
    recover_in_to(dir, &mut Report::default())
}

fn recover_in_to(dir: &Path, found: &mut Report) -> Vec<(PathBuf, std::io::Result<PathBuf>)> {
    let Ok(listing) = std::fs::read_dir(dir) else { return Vec::new() };
    let is_file = |e: &std::fs::DirEntry| e.file_type().is_ok_and(|t| t.is_file());
    let mut files: Vec<_> = listing.flatten().filter(is_file).collect();
    for (folder, marker) in crate::game_folder_rule::game_folders_marked(dir) {
        // In a game folder only the parts on the marker's `writing` list are Recall's: the rest
        // (a copy, a file the user put there) is left alone.
        let entries = std::fs::read_dir(folder).into_iter().flatten().flatten().filter(is_file);
        files.extend(entries.filter(|e| marker.is_writing(&e.file_name().to_string_lossy())));
    }
    let listed = in_progress::list();
    files
        .into_iter()
        .filter(|e| naming::is_longrec_part(&e.file_name().to_string_lossy()))
        .map(|e| e.path())
        .filter(|p| !listed.iter().any(|l| l.part().to_string_lossy().eq_ignore_ascii_case(&p.to_string_lossy())))
        .map(|part| {
            let result = recover_long(&part, true, found);
            (part, result)
        })
        .collect()
}

/// Once (`in_progress::legacy_scanned`): the `.part` files builds before the in-progress list
/// left in `out_dir` (and its game folders) and `staging`. Never recycles anything.
pub(crate) fn legacy_scan(out_dir: &Path, staging: Option<&Path>, found: &mut Report) {
    if in_progress::legacy_scanned() {
        return;
    }
    // The former clip folders too (the app's `folders.json`): a resumed part beside an old
    // folder's recording sits there.
    let former = crate::encoder::staging_guard::former_recordings();
    let dirs = std::iter::once(out_dir.to_path_buf()).chain(former.into_iter().filter(|f| !in_progress::same_path(&f.to_string_lossy(), &out_dir.to_string_lossy())));
    for dir in dirs.collect::<Vec<_>>() {
        for (part, result) in recover_in_to(&dir, found) {
            if let Err(e) = result {
                found.logs.push(format!("long recording {} not recovered: {e}", part.display()));
            }
        }
    }
    for dir in std::iter::once(out_dir).chain(staging) {
        for f in crate::encoder::part_recover::recover_replay_parts(dir) {
            found.logs.push(f.line());
            if let crate::encoder::part_recover::Found::Adopted { clip, .. } = f {
                found.clips.push(clip);
            }
        }
    }
    if let Err(e) = in_progress::mark_legacy_scanned() {
        found.logs.push(format!("the one-time scan for older .part files will run again: {e}"));
    }
}

/// `record_start{continues}`: the next part of the recovered recording `recovered`, beside it
/// (`naming::longrec_next_part`: `__rec-2.mp4`, …). None (logged): it is not a long recording,
/// or its folder is gone; the recording then starts as a new one.
pub(crate) fn next_part_of(recovered: &Path, notes: &mut Vec<Event>) -> Option<PathBuf> {
    let name = recovered.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // The recording's own name: `<stem>__rec[-N]`, without a `-recovered[-N]` the recovery added.
    let lower = name.to_ascii_lowercase();
    let base = lower.strip_suffix(".mp4").and_then(|stem| stem.rfind("__rec").map(|at| at + "__rec".len())).map(|end| format!("{}.mp4", &name[..end]));
    let (Some(base), Some(dir)) = (base, recovered.parent().filter(|d| d.is_dir())) else {
        notes.push(Event::Log(format!("record_start: {} cannot be continued; starting a new recording", recovered.display())));
        return None;
    };
    Some(naming::longrec_next_part(&dir.join(base), Path::exists))
}
