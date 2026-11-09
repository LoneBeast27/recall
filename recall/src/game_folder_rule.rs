//! Game folders ("Sort clips into game folders"): which subfolders of the clip folder are
//! Recall's per-game folders, and which names Recall never gives one. Plain std, one file,
//! shared through `#[path]` by the engine (`recall`: where a clip goes), the controller
//! (`recall-hdr`: the storage limit) and the app (`recall-native`: library, ownership, exports).
//!
//! A folder is a game folder only when Recall made it: it holds the [`MARKER`] file, it is a
//! real folder (not a junction or symlink), and it is not hidden or system. Every other
//! subfolder is the user's, even one named like a game: never counted, listed, recycled or
//! written into.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// The file Recall writes into each game folder it makes.
pub const MARKER: &str = ".recall-game-folder";

/// Names that are never game folders: Recall's own subfolders, other recorders' bookkeeping
/// folders, and Windows' (case-insensitive). Names starting with `.` or `$` never are either.
pub const RESERVED: &[&str] = &[
    "thumbs",
    "thumbnails",
    "screenshots",
    "exports",
    "edited",
    "raw",
    "kept",
    "staging",
    "editor",
    "edits",
    "projects",
    "temp-capture",
    "$recycle.bin",
    "system volume information",
    "windows",
    "program files",
    "program files (x86)",
    "node_modules",
];

const HIDDEN: u32 = 0x2;
const SYSTEM: u32 = 0x4;
const REPARSE_POINT: u32 = 0x400;

/// `name` is one of [`RESERVED`], or starts with `.` or `$`.
pub fn is_reserved(name: &str) -> bool {
    let lower = name.trim().to_lowercase();
    lower.starts_with('.') || lower.starts_with('$') || RESERVED.contains(&lower.as_str())
}

/// A Windows device name (`CON`, `PRN`, `AUX`, `NUL`, `CONIN$`, `CONOUT$`, `COM0`-`COM9`,
/// `LPT0`-`LPT9`, and `COM¹²³` / `LPT¹²³` with superscript digits), with or without an
/// extension, any case. Such a name opens the device instead of a folder.
pub fn is_device_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    let base = lower.split('.').next().unwrap_or("").trim_end_matches(' ');
    if matches!(base, "con" | "prn" | "aux" | "nul" | "conin$" | "conout$") {
        return true;
    }
    let chars: Vec<char> = base.chars().collect();
    let head: String = chars.iter().take(3).collect();
    (head == "com" || head == "lpt") && chars.len() == 4 && (chars[3].is_ascii_digit() || matches!(chars[3], '¹' | '²' | '³'))
}

fn attributes(meta: &std::fs::Metadata) -> u32 {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes()
    }
    #[cfg(not(windows))]
    {
        let _ = meta;
        0
    }
}

/// `path` is a junction, a symlink or another reparse point (the link itself, not its target).
pub fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink() || attributes(&m) & REPARSE_POINT != 0)
}

/// `path` is a real folder: not a link, not hidden, not system.
pub fn is_plain_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink() && attributes(&m) & (HIDDEN | SYSTEM | REPARSE_POINT) == 0)
}

/// `dir` holds the [`MARKER`] file.
pub fn has_marker(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(MARKER)).is_ok_and(|m| m.is_file())
}

/// `dir` is a game folder Recall made: a name that is not [`is_reserved`], a real folder
/// ([`is_plain_dir`]) and the [`MARKER`] inside.
pub fn is_game_folder(dir: &Path) -> bool {
    dir.file_name().is_some_and(|n| !is_reserved(&n.to_string_lossy())) && is_plain_dir(dir) && has_marker(dir)
}

/// The game folders directly inside `root` ([`is_game_folder`]), sorted; none when `root`
/// can't be read.
pub fn game_folders(root: &Path) -> Vec<PathBuf> {
    let Ok(listing) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out: Vec<PathBuf> = listing.flatten().map(|e| e.path()).filter(|p| is_game_folder(p)).collect();
    out.sort();
    out
}

/// Why `dir` may not be linked as an imported folder: it carries the [`MARKER`], so it is one of
/// Recall's game folders and its clips are already listed. One rule for the app's "Link a
/// recording folder" and the controller's link guard (`record_link_guard`).
pub fn link_refusal(dir: &Path) -> Option<&'static str> {
    has_marker(dir).then_some(LINK_REFUSAL)
}

/// The message [`link_refusal`] gives.
pub const LINK_REFUSAL: &str = "This folder is one of Recall's game folders; it's already in your library.";

/// Write the [`MARKER`] into `dir` (hidden on Windows). An existing marker is kept.
pub fn write_marker(dir: &Path) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.attributes(HIDDEN);
    }
    match options.open(dir.join(MARKER)) {
        Ok(mut file) => std::io::Write::write_all(&mut file, b"Recall made this game folder. Recall counts and manages clips here.\r\n"),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// Make the game folder `dir`: `create_dir` (never `create_dir_all`, so a missing parent stays
/// missing), then the [`MARKER`]. A folder that already exists is an error (`AlreadyExists`),
/// so a folder Recall did not make never gets a marker. When the marker can't be written the
/// new, empty folder is removed again.
pub fn make(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir(dir)?;
    if let Err(e) = write_marker(dir) {
        let _ = std::fs::remove_dir(dir);
        return Err(e);
    }
    Ok(())
}

/// Tests: make `link` a junction to `target` (`mklink /J`, no window); false when it can't be
/// made, so the caller skips with a message.
#[cfg(all(test, windows))]
pub fn junction_for_test(target: &Path, link: &Path) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .is_ok_and(|o| o.status.success())
        && is_link(link)
}

/// Tests: set the hidden attribute on `path` (`attrib +h`, no window).
#[cfg(all(test, windows))]
pub fn hide_for_test(path: &Path) -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("attrib")
        .arg("+h")
        .arg(path)
        .creation_flags(0x0800_0000)
        .output()
        .is_ok_and(|o| o.status.success())
        && !is_plain_dir(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_names_any_case_and_extension() {
        for name in ["CON", "con", "nul.txt", "Aux.tar.gz", "COM1", "com9", "LPT0", "COM¹", "com²", "LPT³", "lpt³.txt", "CONIN$", "CON .txt"] {
            assert!(is_device_name(name), "{name}");
        }
        for name in ["Console", "COM10", "COM", "LPT¹2", "Comet", "con-man", "COM⁴"] {
            assert!(!is_device_name(name), "{name}");
        }
    }

    #[test]
    fn reserved_names() {
        for name in ["thumbs", "Thumbnails", "SCREENSHOTS", "exports", "Edited", "raw", "kept", "staging", "editor", "edits", "projects", "temp-capture", "$RECYCLE.BIN", "System Volume Information", ".x", "$x"] {
            assert!(is_reserved(name), "{name}");
        }
        for name in ["Rocket League", "VALORANT", "Desktop", "Keepers"] {
            assert!(!is_reserved(name), "{name}");
        }
    }

    #[test]
    fn a_marked_folder_is_never_linked() {
        let dir = std::env::temp_dir().join(format!("recall-rule-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let plain = link_refusal(&dir);
        write_marker(&dir).unwrap();
        let marked = link_refusal(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(plain, None);
        assert_eq!(marked, Some(LINK_REFUSAL));
    }
}
