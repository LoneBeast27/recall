//! Delete from the library: clips Recall owns go to the Recycle Bin, clips in an imported folder
//! are only removed from Recall. What comes back says which clips really left, so a clip that
//! stayed (in use, locked by another app) keeps its tile.
use std::path::{Path, PathBuf};

use super::{folders, imports, win};

/// What one delete did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeleteReport {
    /// Clips that left the library: recycled ones no longer on disk, the hidden ones, and
    /// clips that were already gone before the delete.
    pub left: Vec<PathBuf>,
    pub recycled: usize,
    pub hidden: usize,
    /// Clips Recall owns that are still on disk (in use, refused).
    pub stayed: usize,
    /// Own clips that were not on disk when the delete started (removed outside Recall): they
    /// leave the library but nothing moved, so they are not counted as recycled.
    pub already_gone: usize,
    pub error: Option<String>,
    /// How many clips this delete was asked to remove (the bar's count, review RI-8).
    pub asked: usize,
}

/// Recycles or hides `clips`, deciding owned vs imported from `folders`: the folder list the
/// confirm dialog used, never a fresh read (a failed read would make every clip "owned" and
/// send another recorder's clips to the Recycle Bin). Reads and moves files: call it off the
/// UI thread.
pub fn delete_or_hide(clips: &[PathBuf], folders: &folders::Folders) -> DeleteReport {
    let (imported, own) = partition_for_delete(clips, folders);
    // Only a clip proven missing counts as already gone; an access error goes to the bin's check.
    let (absent, present): (Vec<PathBuf>, Vec<PathBuf>) = own.into_iter().partition(|path| matches!(path.try_exists(), Ok(false)));
    let hidden = if imported.is_empty() { Ok(0) } else { imports::hide(&imported) };
    let recycled = if present.is_empty() { Ok(0) } else { win::recycle(&present) };
    let mut report = settle(&imported, hidden, &present, recycled, |path| path.exists());
    report.already_gone = absent.len();
    report.left.extend(absent);
    report.asked = clips.len();
    // A clip recorded outside the recordings folders that left: a later file there is not Recall's.
    super::recorded::forget(&report.left);
    report
}

/// (imported: only removed from Recall, own: to the Recycle Bin), by `folders` alone.
pub(crate) fn partition_for_delete(clips: &[PathBuf], folders: &folders::Folders) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let recordings = crate::backend::library_dir_from(folders);
    // Owned wins over linked: a recordings folder under a linked parent (Videos linked, clips in
    // Videos\Recall) keeps Delete-to-Recycle-Bin; only clips Recall does not own are hidden.
    clips.iter().cloned().partition(|clip| !imports::is_owned(clip, &recordings, &folders.former_recordings))
}

/// How many of `clips` a delete only hides (the confirm dialog's count).
pub(crate) fn imported_count(clips: &[PathBuf], folders: &folders::Folders) -> usize {
    partition_for_delete(clips, folders).0.len()
}

/// The report from both halves' results and which own clips are still on disk.
pub(crate) fn settle(imported: &[PathBuf], hidden: Result<usize, String>, own: &[PathBuf], recycled: Result<usize, String>, exists: impl Fn(&Path) -> bool) -> DeleteReport {
    let mut report = DeleteReport::default();
    match hidden {
        Ok(_) => {
            report.left.extend(imported.iter().cloned());
            report.hidden = imported.len();
        }
        Err(error) => report.error = Some(error),
    }
    // Whatever the shell said, a clip still on disk stayed and one that is gone left.
    let (stayed, gone): (Vec<&PathBuf>, Vec<&PathBuf>) = own.iter().partition(|path| exists(path));
    report.recycled = gone.len();
    report.stayed = stayed.len();
    report.left.extend(gone.into_iter().cloned());
    if let Err(error) = recycled {
        report.error.get_or_insert(error);
    }
    report
}

/// The toast after a delete: (is it an error, title, detail).
pub fn delete_toast(report: &DeleteReport) -> (bool, String, Option<String>) {
    let gone = report.already_gone;
    if gone == 0 {
        return moved_toast(report);
    }
    if report.left.len() == gone && report.error.is_none() && report.stayed == 0 {
        let title = if gone == 1 { "That clip was already gone".to_owned() } else { format!("Those {gone} clips were already gone") };
        return (false, title, Some("Nothing moved; removed from Recall.".into()));
    }
    let note = if gone == 1 { "1 was already gone.".to_owned() } else { format!("{gone} were already gone.") };
    let (bad, title, detail) = moved_toast(report);
    (bad, title, Some(detail.map_or_else(|| note.clone(), |detail| format!("{detail} {note}"))))
}

/// [`delete_toast`] for the clips that were on disk.
fn moved_toast(report: &DeleteReport) -> (bool, String, Option<String>) {
    let clips = |n: usize| format!("{n} {}", if n == 1 { "clip" } else { "clips" });
    if report.left.len() == report.already_gone {
        let why = report.error.clone().or_else(|| (report.stayed > 0).then(|| format!("{} in use and stayed.", is_are(report.stayed))));
        return (report.error.is_some() || report.stayed > 0, "Delete failed".into(), why);
    }
    if let Some(error) = &report.error {
        let (title, _) = delete_message(report.recycled, report.hidden);
        return (true, title, Some(error.clone()));
    }
    if report.stayed > 0 {
        let title = format!("Moved {} of {} to the Recycle Bin", report.recycled, clips(report.recycled + report.stayed));
        let mut detail = format!("{} in use and stayed.", is_are(report.stayed));
        if report.hidden > 0 {
            detail.push_str(&format!(" {} removed from Recall.", clips(report.hidden)));
        }
        return (false, title, Some(detail));
    }
    let (title, detail) = delete_message(report.recycled, report.hidden);
    (false, title, detail)
}

fn is_are(n: usize) -> String {
    if n == 1 { "1 is".into() } else { format!("{n} are") }
}

/// The toast after a delete that went through: title and detail.
pub fn delete_message(recycled: usize, hidden: usize) -> (String, Option<String>) {
    let clips = |n: usize| format!("{n} {}", if n == 1 { "clip" } else { "clips" });
    match (recycled, hidden) {
        (r, 0) => (format!("Moved {} to the Recycle Bin", clips(r)), None),
        (0, h) => (format!("Removed {} from Recall", clips(h)), Some("The files stay in their folder.".into())),
        (r, h) => (format!("Removed {}", clips(r + h)), Some(format!("{r} went to the Recycle Bin. {h} from other folders stay where they are."))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn a_clip_that_stayed_on_disk_is_not_forgotten() {
        let own = paths(&[r"X:\R\a.mp4", r"X:\R\b.mp4", r"X:\R\c.mp4"]);
        let report = settle(&[], Ok(0), &own, Ok(2), |path| path.ends_with("c.mp4"));
        assert_eq!(report.left, paths(&[r"X:\R\a.mp4", r"X:\R\b.mp4"]));
        assert_eq!((report.recycled, report.stayed), (2, 1));
        let (bad, title, detail) = delete_toast(&report);
        assert!(!bad);
        assert_eq!(title, "Moved 2 of 3 clips to the Recycle Bin");
        assert_eq!(detail.as_deref(), Some("1 is in use and stayed."));
    }

    #[test]
    fn hidden_clips_leave_even_when_the_recycle_half_fails() {
        let imported = paths(&[r"D:\Old\x.mp4"]);
        let own = paths(&[r"X:\R\a.mp4"]);
        let report = settle(&imported, Ok(1), &own, Err("bin is off".into()), |_| true);
        assert_eq!(report.left, imported, "the hidden clip goes, the clip still on disk stays");
        let (bad, _, detail) = delete_toast(&report);
        assert!(bad && detail.as_deref() == Some("bin is off"));
        let none = settle(&[], Ok(0), &own, Err("refused".into()), |_| true);
        assert_eq!(delete_toast(&none), (true, "Delete failed".into(), Some("refused".into())));
    }

    #[test]
    fn a_whole_delete_reads_as_before() {
        let own = paths(&[r"X:\R\a.mp4"]);
        let report = settle(&paths(&[r"D:\Old\x.mp4"]), Ok(1), &own, Ok(1), |_| false);
        assert_eq!(delete_toast(&report), (false, "Removed 2 clips".into(), Some("1 went to the Recycle Bin. 1 from other folders stay where they are.".into())));
    }

    #[test]
    fn the_dialogs_folder_list_decides_what_is_only_hidden() {
        // Whatever folders.json on disk says (unreadable, defaults), the dialog's list wins.
        let folders = folders::Folders { recordings: Some(r"X:\Recall".into()), imported: vec![r"D:\Medal".into()], ..Default::default() };
        let (imported, own) = partition_for_delete(&paths(&[r"D:\Medal\Game\x.mp4", r"X:\Recall\a.mp4"]), &folders);
        assert_eq!(imported, paths(&[r"D:\Medal\Game\x.mp4"]), "an imported clip is never sent to the Recycle Bin");
        assert_eq!(own, paths(&[r"X:\Recall\a.mp4"]));
    }

    #[test]
    fn a_clip_missing_before_the_delete_is_not_counted_as_recycled() {
        let dir = std::env::temp_dir().join(format!("recall-delete-gone-{}", std::process::id()));
        let missing = dir.join("never-there.mp4");
        let folders = folders::Folders { recordings: Some(dir.to_string_lossy().into_owned()), ..Default::default() };
        let report = delete_or_hide(std::slice::from_ref(&missing), &folders);
        assert_eq!((report.recycled, report.already_gone, report.stayed), (0, 1, 0));
        assert_eq!(report.left, [missing], "it still leaves the library");
        let (bad, title, _) = delete_toast(&report);
        assert!(!bad);
        assert_eq!(title, "That clip was already gone");
    }
}

#[cfg(test)]
#[path = "delete_owned_tests.rs"]
mod owned_tests;
