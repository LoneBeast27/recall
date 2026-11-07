//! Keeping folders.json in step with the recorder's output folder (`recording.output_dir`).
//! - The Change button sends the patch and remembers a [`PendingFolder`]; folders.json is
//!   rewritten only when the recorder reports the new folder. The first config event after the
//!   send that still shows the old folder is the recorder's answer to a refused patch (the app
//!   sends SetConfig then GetConfig), so no error text has to be matched.
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
    /// `DaemonSnapshot::config_seq` when the patch was sent: a later config event is the answer.
    pub config_seq: u64,
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
    /// The recorder answered the pending change with the old folder: it refused the patch.
    Rejected,
}

fn same(a: &str, b: &str) -> bool {
    folder_key(a) == folder_key(b)
}

/// Decide what to do when the recorder reports `reported` as its output folder.
/// `config_seq`: the snapshot's config event count (a pending change waits for a later one).
pub fn plan(recordings: Option<&str>, reported: &str, pending: Option<&PendingFolder>, overridden: bool, config_seq: u64) -> FolderPlan {
    if let Some(p) = pending {
        if same(&p.new, reported) {
            return FolderPlan::Commit;
        }
        if config_seq != p.config_seq && same(&p.old, reported) {
            return FolderPlan::Rejected;
        }
    }
    if recordings == Some(reported) {
        return FolderPlan::Nothing;
    }
    if overridden {
        return FolderPlan::Skip;
    }
    FolderPlan::Follow
}

/// Make `new` the recordings folder: it leaves the imported list, and the old ones join it:
/// `old` (the folder the recorder used, an env override's during one) and the recordings folder
/// folders.json named until now (kept by the override skip). True when an old folder was added.
pub fn retarget(folders: &mut Folders, new: &str, old: Option<&str>) -> bool {
    let olds: Vec<String> = old.map(str::to_owned).into_iter().chain(folders.recordings.clone()).collect();
    folders.remove(new);
    folders.recordings = Some(new.to_owned());
    let mut added = false;
    for o in olds.iter().filter(|o| !o.trim().is_empty() && !same(o, new)) {
        added |= folders.add(Path::new(o));
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending() -> PendingFolder {
        PendingFolder { new: r"E:\New".into(), old: r"D:\Old".into(), config_seq: 3 }
    }

    #[test]
    fn a_folder_moved_behind_the_apps_back_keeps_the_old_one_listed() {
        assert_eq!(plan(Some(r"D:\Old"), r"C:\Users\me\Videos\Recall", None, false, 0), FolderPlan::Follow);
        let mut folders = Folders { recordings: Some(r"D:\Old".into()), imported: vec![r"F:\Medal".into()], ..Folders::default() };
        assert!(retarget(&mut folders, r"C:\Users\me\Videos\Recall", None), "the old folder is kept");
        assert_eq!(folders.recordings.as_deref(), Some(r"C:\Users\me\Videos\Recall"));
        assert_eq!(folders.imported, vec![r"F:\Medal".to_string(), r"D:\Old".to_string()]);
    }

    #[test]
    fn an_env_override_never_repoints_the_library() {
        assert_eq!(plan(Some(r"D:\Old"), r"D:\Tmp", None, true, 0), FolderPlan::Skip);
    }

    #[test]
    fn the_change_button_commits_only_once_the_recorder_reports_it() {
        let p = pending();
        assert_eq!(plan(Some(r"D:\Old"), r"D:\Old", Some(&p), false, 3), FolderPlan::Nothing, "no config event since the send");
        assert_eq!(plan(Some(r"D:\Old"), r"e:\new\", Some(&p), false, 4), FolderPlan::Commit);
        let mut folders = Folders { recordings: Some(r"D:\Old".into()), imported: vec![r"E:\New".into()], ..Folders::default() };
        assert!(retarget(&mut folders, &p.new, Some(&p.old)));
        assert_eq!(folders.recordings.as_deref(), Some(r"E:\New"));
        assert_eq!(folders.imported, vec![r"D:\Old".to_string()], "the new folder leaves the imported list");
    }

    #[test]
    fn a_change_during_an_override_keeps_the_real_folder() {
        // Started with -OutDir D:\Tmp; folders.json still names the real folder D:\Real.
        let mut folders = Folders { recordings: Some(r"D:\Real".into()), imported: vec![], ..Folders::default() };
        assert!(retarget(&mut folders, r"E:\New", Some(r"D:\Tmp")));
        assert_eq!(folders.recordings.as_deref(), Some(r"E:\New"));
        assert_eq!(folders.imported, vec![r"D:\Tmp".to_string(), r"D:\Real".to_string()], "both old folders still show");
    }

    #[test]
    fn the_first_config_event_still_showing_the_old_folder_is_the_rejection() {
        let p = pending();
        assert_eq!(plan(Some(r"D:\Old"), r"D:\Old", Some(&p), false, 4), FolderPlan::Rejected);
        // During an env override the old folder is the override's.
        let o = PendingFolder { new: r"E:\New".into(), old: r"D:\Tmp".into(), config_seq: 7 };
        assert_eq!(plan(Some(r"D:\Real"), r"D:\Tmp", Some(&o), true, 7), FolderPlan::Skip, "no answer yet");
        assert_eq!(plan(Some(r"D:\Real"), r"D:\Tmp", Some(&o), true, 8), FolderPlan::Rejected);
    }
}
