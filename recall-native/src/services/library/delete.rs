//! Delete from the library: clips Recall owns go to the Recycle Bin, clips in an imported folder
//! are only removed from Recall. What comes back says which clips really left, so a clip that
//! stayed (in use, locked by another app) keeps its tile.
use std::path::{Path, PathBuf};

use super::{folders, imports, win};

/// What one delete did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeleteReport {
    /// Clips that left the library: recycled ones no longer on disk, plus the hidden ones.
    pub left: Vec<PathBuf>,
    pub recycled: usize,
    pub hidden: usize,
    /// Clips Recall owns that are still on disk (in use, refused).
    pub stayed: usize,
    pub error: Option<String>,
}

/// Recycles or hides `clips`. Reads and moves files: call it off the UI thread.
pub fn delete_or_hide(clips: &[PathBuf]) -> DeleteReport {
    let folders = folders::Folders::load();
    let recordings = crate::backend::library_dir_from(&folders);
    let (imported, own): (Vec<PathBuf>, Vec<PathBuf>) =
        clips.iter().cloned().partition(|c| imports::is_imported(&c.to_string_lossy(), &recordings, &folders.imported, &folders.former_recordings));
    let hidden = if imported.is_empty() { Ok(0) } else { imports::hide(&imported) };
    let recycled = if own.is_empty() { Ok(0) } else { win::recycle(&own) };
    settle(&imported, hidden, &own, recycled, |path| path.exists())
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
    let clips = |n: usize| format!("{n} {}", if n == 1 { "clip" } else { "clips" });
    if report.left.is_empty() {
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
        let own = paths(&[r"V:\R\a.mp4", r"V:\R\b.mp4", r"V:\R\c.mp4"]);
        let report = settle(&[], Ok(0), &own, Ok(2), |path| path.ends_with("c.mp4"));
        assert_eq!(report.left, paths(&[r"V:\R\a.mp4", r"V:\R\b.mp4"]));
        assert_eq!((report.recycled, report.stayed), (2, 1));
        let (bad, title, detail) = delete_toast(&report);
        assert!(!bad);
        assert_eq!(title, "Moved 2 of 3 clips to the Recycle Bin");
        assert_eq!(detail.as_deref(), Some("1 is in use and stayed."));
    }

    #[test]
    fn hidden_clips_leave_even_when_the_recycle_half_fails() {
        let imported = paths(&[r"D:\Old\x.mp4"]);
        let own = paths(&[r"V:\R\a.mp4"]);
        let report = settle(&imported, Ok(1), &own, Err("bin is off".into()), |_| true);
        assert_eq!(report.left, imported, "the hidden clip goes, the clip still on disk stays");
        let (bad, _, detail) = delete_toast(&report);
        assert!(bad && detail.as_deref() == Some("bin is off"));
        let none = settle(&[], Ok(0), &own, Err("refused".into()), |_| true);
        assert_eq!(delete_toast(&none), (true, "Delete failed".into(), Some("refused".into())));
    }

    #[test]
    fn a_whole_delete_reads_as_before() {
        let own = paths(&[r"V:\R\a.mp4"]);
        let report = settle(&paths(&[r"D:\Old\x.mp4"]), Ok(1), &own, Ok(1), |_| false);
        assert_eq!(delete_toast(&report), (false, "Removed 2 clips".into(), Some("1 went to the Recycle Bin. 1 from other folders stay where they are.".into())));
    }
}
