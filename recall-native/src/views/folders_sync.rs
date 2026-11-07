//! Keeping folders.json in step with the recorder's output folder (`recording.output_dir`).
//! - The Change button sends the patch and remembers a [`PendingFolder`]; folders.json is
//!   rewritten only when the recorder reports the new folder (a rejected patch changes nothing).
//! - A folder that moved without the button (a reset config, a hand edit) keeps the old folder
//!   as an imported one, so its clips still show.
//! - While a startup env override sets the folder (`Start-Recall.ps1 -OutDir`), folders.json is
//!   left alone: the override is temporary.
use std::path::Path;

use crate::services::library::folders::{Folders, folder_key};

/// A Change-button folder change the recorder has not confirmed yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingFolder {
    pub new: String,
    pub old: String,
}

/// What a reported output folder means for folders.json.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderPlan {
    /// Already the recordings folder (or a pending change not confirmed yet).
    Nothing,
    /// An env override sets it: folders.json stays as it is.
    Skip,
    /// The pending Change-button folder is now the recorder's: apply it.
    Commit,
    /// It moved without the button: follow it, keeping the old folder imported.
    Follow,
}

fn same(a: &str, b: &str) -> bool {
    folder_key(a) == folder_key(b)
}

/// Decide what to do when the recorder reports `reported` as its output folder.
pub fn plan(recordings: Option<&str>, reported: &str, pending: Option<&PendingFolder>, overridden: bool) -> FolderPlan {
    if pending.is_some_and(|p| same(&p.new, reported)) {
        return FolderPlan::Commit;
    }
    if recordings == Some(reported) {
        return FolderPlan::Nothing;
    }
    if overridden {
        return FolderPlan::Skip;
    }
    FolderPlan::Follow
}

/// Make `new` the recordings folder: it leaves the imported list, and the old one (`old`, else
/// the current recordings folder) joins it. True when the old folder was added.
pub fn retarget(folders: &mut Folders, new: &str, old: Option<&str>) -> bool {
    let old = old.map(str::to_owned).or_else(|| folders.recordings.clone());
    folders.remove(new);
    folders.recordings = Some(new.to_owned());
    old.filter(|o| !o.trim().is_empty() && !same(o, new)).is_some_and(|o| folders.add(Path::new(&o)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending() -> PendingFolder {
        PendingFolder { new: r"E:\New".into(), old: r"D:\Old".into() }
    }

    #[test]
    fn a_folder_moved_behind_the_apps_back_keeps_the_old_one_listed() {
        assert_eq!(plan(Some(r"D:\Old"), r"C:\Users\me\Videos\Recall", None, false), FolderPlan::Follow);
        let mut folders = Folders { recordings: Some(r"D:\Old".into()), imported: vec![r"F:\Medal".into()], ..Folders::default() };
        assert!(retarget(&mut folders, r"C:\Users\me\Videos\Recall", None), "the old folder is kept");
        assert_eq!(folders.recordings.as_deref(), Some(r"C:\Users\me\Videos\Recall"));
        assert_eq!(folders.imported, vec![r"F:\Medal".to_string(), r"D:\Old".to_string()]);
    }

    #[test]
    fn an_env_override_never_repoints_the_library() {
        assert_eq!(plan(Some(r"D:\Old"), r"D:\Tmp", None, true), FolderPlan::Skip);
    }

    #[test]
    fn the_change_button_commits_only_once_the_recorder_reports_it() {
        let p = pending();
        assert_eq!(plan(Some(r"D:\Old"), r"D:\Old", Some(&p), false), FolderPlan::Nothing, "the rejected patch's reply");
        assert_eq!(plan(Some(r"D:\Old"), r"e:\new\", Some(&p), false), FolderPlan::Commit);
        let mut folders = Folders { recordings: Some(r"D:\Old".into()), imported: vec![r"E:\New".into()], ..Folders::default() };
        assert!(retarget(&mut folders, &p.new, Some(&p.old)));
        assert_eq!(folders.recordings.as_deref(), Some(r"E:\New"));
        assert_eq!(folders.imported, vec![r"D:\Old".to_string()], "the new folder leaves the imported list");
    }
}
