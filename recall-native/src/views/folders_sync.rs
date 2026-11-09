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

/// Why Recall won't record into `new`, if it won't. Linked folders are read-only to Recall:
/// refused when `new` is a linked folder, holds one, or lies inside one without being a folder
/// Recall already records into (the recordings folder or a former one). A linked parent of the
/// recordings folder is fine (Videos linked, clips in Videos\Recall). The recorder applies the
/// same rule (recall-hdr `record_link_guard`).
pub fn link_refusal(new: &str, folders: &Folders) -> Option<String> {
    use crate::services::storage::guard::folder_within;
    let owned = folders.recordings.iter().chain(&folders.former_recordings).any(|f| same(f, new));
    let new_path = Path::new(new);
    folders.linked().find_map(|link| {
        let link_path = Path::new(link);
        let holds = folder_within(link_path, new_path);
        let refused = same(link, new) || holds || (folder_within(new_path, link_path) && !owned);
        refused.then(|| format!("{new} is a linked folder; Recall won't record into it"))
    })
}

/// Make `new` the recordings folder: it leaves the imported list, and the old ones join it:
/// `old` (the folder the recorder used, an env override's during one) and the recordings folder
/// folders.json named until now (kept by the override skip). `Ok(true)` when an old folder was
/// added. Every old folder goes through [`Folders::move_recordings`], so none stops being
/// Recall's. A folder [`link_refusal`] refuses changes nothing: the link stays linked.
pub fn retarget(folders: &mut Folders, new: &str, old: Option<&str>) -> Result<bool, String> {
    if let Some(why) = link_refusal(new, folders) {
        return Err(why);
    }
    let olds: Vec<String> = old.map(str::to_owned).into_iter().chain(folders.recordings.clone()).collect();
    folders.remove(new);
    folders.move_recordings(new, None);
    let mut added = false;
    for o in olds.iter().filter(|o| !o.trim().is_empty() && !same(o, new)) {
        // Each old folder stays one Recall manages (L15: Delete-to-Recycle-Bin, edits beside).
        folders.move_recordings(new, Some(o));
        added |= folders.add(Path::new(o));
    }
    Ok(added)
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
        assert!(retarget(&mut folders, r"C:\Users\me\Videos\Recall", None).unwrap(), "the old folder is kept");
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
        // E:\New was a recordings folder before (listed, managed): a linked folder would be refused.
        let mut folders = Folders { recordings: Some(r"D:\Old".into()), imported: vec![r"E:\New".into()], former_recordings: vec![r"E:\New".into()], ..Folders::default() };
        assert!(retarget(&mut folders, &p.new, Some(&p.old)).unwrap());
        assert_eq!(folders.recordings.as_deref(), Some(r"E:\New"));
        assert_eq!(folders.imported, vec![r"D:\Old".to_string()], "the new folder leaves the imported list");
    }

    #[test]
    fn a_change_during_an_override_keeps_the_real_folder() {
        // Started with -OutDir D:\Tmp; folders.json still names the real folder D:\Real.
        let mut folders = Folders { recordings: Some(r"D:\Real".into()), imported: vec![], ..Folders::default() };
        assert!(retarget(&mut folders, r"E:\New", Some(r"D:\Tmp")).unwrap());
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

    #[test]
    fn every_old_folder_a_retarget_leaves_stays_one_recall_manages() {
        // A change during an override (D:\Tmp) away from the real folder D:\Real, then a folder
        // moved behind the app's back: every folder clips were saved to stays Recall-owned.
        let mut folders = Folders { recordings: Some(r"D:\Real".into()), ..Folders::default() };
        retarget(&mut folders, r"E:\New", Some(r"D:\Tmp")).unwrap();
        retarget(&mut folders, r"F:\Moved", None).unwrap();
        assert_eq!(folders.recordings.as_deref(), Some(r"F:\Moved"));
        assert_eq!(folders.former_recordings, vec![r"D:\Tmp".to_string(), r"D:\Real".to_string(), r"E:\New".to_string()]);
        assert_eq!(folders.managed(r"F:\Moved").len(), 3, "listed under Folders Recall manages");
    }

    fn linked(recordings: &str, link: &str) -> Folders {
        Folders { recordings: Some(recordings.into()), imported: vec![link.into()], ..Folders::default() }
    }

    #[test]
    fn an_output_dir_set_to_a_linked_folder_never_unlinks_it() {
        // (A) recording.output_dir hand-set to the linked Moments folder: Follow must refuse.
        let link = r"D:\SteelSeries\Moments";
        let mut folders = linked(r"D:\Old", link);
        let before = folders.clone();
        let _ = retarget(&mut folders, link, None);
        assert_eq!(folders, before, "the link stays linked and D:\\Old stays the recordings folder");
        let mut above = linked(r"D:\Old", link);
        let _ = retarget(&mut above, r"D:\SteelSeries", None);
        assert_eq!(above, linked(r"D:\Old", link), "a folder holding a link is refused too");
    }

    #[test]
    fn a_linked_parent_keeps_the_owned_recordings_folder_but_refuses_others_inside() {
        // (B) Videos linked (ShadowPlay's default) with recordings in Videos\Recall: allowed.
        let folders = linked(r"C:\Users\me\Videos\Recall", r"C:\Users\me\Videos");
        assert_eq!(link_refusal(r"C:\Users\me\Videos\Recall", &folders), None, "the owned folder stays usable");
        // A move into someone else's part of the linked tree is refused.
        let mut moved = folders.clone();
        let _ = retarget(&mut moved, r"C:\Users\me\Videos\Moments", None);
        assert_eq!(moved, folders, "Recall won't record into Videos\\Moments");
        let why = link_refusal(r"C:\Users\me\Videos\Moments", &folders).expect("refused");
        assert!(why.contains("Recall won't record into it"), "{why}");
    }
}
