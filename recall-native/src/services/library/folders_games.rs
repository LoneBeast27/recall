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

/// `clip` is Recall's recorder clip in a game folder of `folder` (`<folder>\<Game>\<clip>`): made
/// by Recall, or saved there by Recall after the folder was adopted (the marker's `saved` list).
/// Looks at the disk (the marker, parsed once per version: [`marker_of`]).
pub fn in_game_folder(clip: &Path, folder: &Path) -> bool {
    let Some(game) = clip.parent() else { return false };
    clip.file_name().is_some_and(|n| is_recall_named(&n.to_string_lossy()))
        && game.parent().is_some_and(|parent| {
            let key = folder_key(&folder.to_string_lossy());
            !key.is_empty() && folder_key(&parent.to_string_lossy()) == key
        })
        // In an adopted folder ("Use this folder") only the clips Recall saved there are owned;
        // the user's files are not, so Delete only hides them and edits go beside, as for links.
        && marker_of(game).is_some_and(|m| clip.file_name().is_some_and(|n| m.owns(&n.to_string_lossy())))
}

/// What a cached marker was read from: the marker file's size and time, and the folder's ID.
type Version = (u64, std::time::SystemTime, Option<rule::FileId>);
type MarkerCache = std::sync::Mutex<std::collections::HashMap<PathBuf, (Version, std::sync::Arc<rule::Marker>)>>;

/// `game`'s marker when it is a game folder ([`rule::game_folder_marker`]: bound to this very
/// folder), parsed once per version of the marker file and of the folder (a folder swapped in
/// under the same name has another ID): a listing asks for every clip, and a marker can hold
/// thousands of names.
fn marker_of(game: &Path) -> Option<std::sync::Arc<rule::Marker>> {
    static CACHE: std::sync::OnceLock<MarkerCache> = std::sync::OnceLock::new();
    let named = game.file_name().is_some_and(|n| !rule::is_reserved(&n.to_string_lossy()));
    let meta = std::fs::symlink_metadata(game.join(rule::MARKER)).ok()?;
    if !named || !rule::is_plain_dir(game) || !meta.is_file() {
        return None;
    }
    let version: Version = (meta.len(), meta.modified().ok()?, rule::folder_id(game));
    let key = PathBuf::from(folder_key(&game.to_string_lossy()));
    let cache = CACHE.get_or_init(Default::default);
    if let Some((seen, marker)) = cache.lock().unwrap_or_else(|p| p.into_inner()).get(&key)
        && *seen == version
    {
        return Some(marker.clone());
    }
    let marker = std::sync::Arc::new(rule::game_folder_marker(game)?);
    cache.lock().unwrap_or_else(|p| p.into_inner()).insert(key, (version, marker.clone()));
    Some(marker)
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

/// Marked folders directly inside `recordings` whose marker no longer binds them (a USB drive
/// back under another letter, a moved recordings folder, a mapped drive spelled as UNC, a copy):
/// a real, unreserved folder holding a [`rule::MARKER`] that [`game_folders`] leaves out. The
/// library still lists their Recall-named clips, as not owned (Delete only hides them, edits go
/// to C:), so a broken binding never looks like lost clips. The storage limit never counts them.
pub fn unbound_game_folders(recordings: &Path, linked: &[String]) -> Vec<PathBuf> {
    let Ok(listing) = std::fs::read_dir(recordings) else { return Vec::new() };
    let bound = rule::game_folders(recordings);
    let mut out: Vec<PathBuf> = listing
        .flatten()
        .map(|e| e.path())
        .filter(|dir| dir.file_name().is_some_and(|n| !rule::is_reserved(&n.to_string_lossy())))
        .filter(|dir| rule::is_plain_dir(dir) && rule::has_marker(dir) && !bound.contains(dir))
        .filter(|dir| !under_link(dir, recordings, linked))
        .collect();
    out.sort();
    out
}

#[cfg(test)]
#[path = "folders_games_tests.rs"]
mod tests;
