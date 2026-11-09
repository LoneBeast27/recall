//! "Sort clips into game folders" in the library: which subfolders of the recordings folder are
//! Recall's game folders, and which clips in them are Recall's. A folder counts only when
//! Recall made it (`game_folder_rule`: marker, real folder, not hidden or system), and a linked
//! (Folder Mapper) folder always wins over the game-folder rule.

use std::path::{Path, PathBuf};

use super::{folder_key, is_inside};

/// The rule shared with the engine and the storage limit.
#[path = "../../../../recall/src/game_folder_rule.rs"]
pub mod rule;

/// A recorder clip name, `<GAME>__YYYY-MM-DD__HH-MM-SS[...].mp4` (case-insensitive). Game
/// folders list only these, so other videos someone keeps in a game folder stay out.
pub fn is_recall_named(name: &str) -> bool {
    const SHAPE: &[u8] = b"__dddd-dd-dd__dd-dd-dd";
    let lower = name.to_ascii_lowercase();
    let Some(stem) = lower.strip_suffix(".mp4") else { return false };
    let b = stem.as_bytes();
    (1..b.len()).any(|i| b.len() >= i + SHAPE.len() && SHAPE.iter().zip(&b[i..]).all(|(&s, &c)| if s == b'd' { c.is_ascii_digit() } else { s == c }))
}

/// `clip` is a recorder clip in a game folder Recall made in `folder` (`<folder>\<Game>\<clip>`).
/// Looks at the disk (the marker).
pub fn in_game_folder(clip: &Path, folder: &Path) -> bool {
    let Some(game) = clip.parent() else { return false };
    clip.file_name().is_some_and(|n| is_recall_named(&n.to_string_lossy()))
        && game.parent().is_some_and(|parent| {
            let key = folder_key(&folder.to_string_lossy());
            !key.is_empty() && folder_key(&parent.to_string_lossy()) == key
        })
        && rule::is_game_folder(game)
}

/// `path` is a linked folder in `linked`, or inside one, where that link is not the recordings
/// folder or a parent of it. A link above the recordings folder (Videos linked, clips in
/// Videos\Recall) keeps Recall's clips Recall's; a link at a game folder makes it the link's.
pub fn under_link(path: &Path, recordings: &Path, linked: &[String]) -> bool {
    let (path, recordings) = (path.to_string_lossy(), recordings.to_string_lossy());
    linked.iter().any(|link| {
        let at_or_in = |p: &str| folder_key(p) == folder_key(link) || is_inside(p, link);
        at_or_in(&path) && !at_or_in(&recordings)
    })
}

/// The game folders inside `recordings` now (sorted; none when it can't be read), without
/// any that is a linked folder ([`under_link`]): the link's listing shows those.
pub fn game_folders(recordings: &Path, linked: &[String]) -> Vec<PathBuf> {
    rule::game_folders(recordings).into_iter().filter(|game| !under_link(game, recordings, linked)).collect()
}

#[cfg(test)]
#[path = "folders_games_tests.rs"]
mod tests;
