//! Game folders ("Sort clips into game folders"): which subfolders of the clip folder are
//! Recall's per-game folders, and which names Recall never gives one. std + serde_json, one file,
//! shared through `#[path]` by the engine (`recall`: where a clip goes), the controller
//! (`recall-hdr`: the storage limit) and the app (`recall-native`: library, ownership, exports).
//!
//! A folder is a game folder only when Recall made it or the user let Recall adopt it ("Use this
//! folder"): it holds a readable [`MARKER`], it is a real folder (not a junction or symlink),
//! and it is not hidden or system. Every other subfolder is the user's, even one named like a
//! game: never counted, listed, recycled or written into. In an adopted folder the files that
//! were there before stay the user's ([`Marker::owns`]).
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

/// `dir` is a game folder: [`game_folder_marker`] finds a readable marker in a real folder.
pub fn is_game_folder(dir: &Path) -> bool {
    game_folder_marker(dir).is_some()
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

/// Longest game folder name, in characters (Windows paths stay short).
pub const GAME_FOLDER_MAX: usize = 60;

/// Game name -> a folder name that is safe on Windows and readable: "Rocket League®" ->
/// "Rocket League". Keeps letters, digits, spaces and a few harmless marks; drops the rest
/// (`<>:"/\|?*`, control characters, symbols); collapses spaces; no leading or trailing dots
/// or spaces; at most [`GAME_FOLDER_MAX`] characters; a Windows device name (`CON`, `COM1`,
/// `COM¹`, ...) gets a `_` in front, a reserved folder name (`thumbs`, `raw`, ...) a `_` on the
/// end. The engine names folders with it; the app computes the same name. Empty -> "Desktop".
pub fn game_folder_name(source: &str) -> String {
    let kept: String = source
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|&c| c.is_alphanumeric() || " -_.,'&()[]!+#=~;".contains(c))
        .collect();
    let mut name = kept.split(' ').filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
    if name.chars().count() > GAME_FOLDER_MAX {
        name = name.chars().take(GAME_FOLDER_MAX).collect();
    }
    let name = name.trim_matches(|c: char| c == '.' || c == ' ').to_string();
    if name.is_empty() {
        return "Desktop".to_string();
    }
    if is_device_name(&name) {
        return format!("_{name}");
    }
    if is_reserved(&name) {
        return format!("{name}_");
    }
    name
}

/// The answer to "Use your existing <Game> folder?" (`recording.game_folder_choices`), for a
/// folder with the game's name that Recall did not make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// Adopt it: [`adopt`] writes the marker, new clips go into it.
    Merge,
    /// Leave it alone: new clips go into `<name> (Recall)`.
    Separate,
}

impl Choice {
    pub fn parse(text: &str) -> Option<Choice> {
        match text.trim() {
            "merge" => Some(Choice::Merge),
            "separate" => Some(Choice::Separate),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Choice::Merge => "merge",
            Choice::Separate => "separate",
        }
    }
}

/// The `recording.game_folder_choices` key for a folder named `folder_name`: lowercase.
pub fn choice_key(folder_name: &str) -> String {
    folder_name.to_lowercase()
}

/// What a game folder's [`MARKER`] says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Marker {
    /// Recall made the folder: every Recall-named clip in it is Recall's.
    Made,
    /// The user's folder, adopted ("Use this folder"): the names in `existing` (lowercase,
    /// sorted) were there before and are never Recall's.
    Adopted { existing: Vec<String> },
}

impl Marker {
    /// `file_name` in this folder is Recall's (counted, owned, recovered). Callers still check
    /// the Recall clip name.
    pub fn owns(&self, file_name: &str) -> bool {
        match self {
            Marker::Made => true,
            Marker::Adopted { existing } => existing.binary_search(&file_name.to_lowercase()).is_err(),
        }
    }

    /// The marker's text: `{"version":1}`, or the adopted form with the names found.
    pub fn to_json(&self) -> String {
        match self {
            Marker::Made => r#"{"version":1}"#.to_string(),
            Marker::Adopted { existing } => serde_json::json!({ "version": 1, "adopted": true, "existing": existing }).to_string(),
        }
    }

    /// Parses a marker's text. Empty and the first build's sentence are [`Marker::Made`];
    /// anything else must be the JSON above. `None`: unreadable, so the folder is not a game
    /// folder (nothing in it is counted or owned).
    pub fn parse(text: &str) -> Option<Marker> {
        let text = text.trim_start_matches('\u{feff}').trim();
        if text.is_empty() || text.starts_with("Recall made this game folder") {
            return Some(Marker::Made);
        }
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        if value.get("version")?.as_u64()? != 1 {
            return None;
        }
        if !value.get("adopted").map_or(Some(false), serde_json::Value::as_bool)? {
            return Some(Marker::Made);
        }
        let list = value.get("existing")?.as_array()?;
        let mut existing = list.iter().map(|v| v.as_str().map(str::to_lowercase)).collect::<Option<Vec<_>>>()?;
        existing.sort();
        existing.dedup();
        Some(Marker::Adopted { existing })
    }
}

/// `dir`'s [`MARKER`], read and parsed; `None` when missing or unreadable.
pub fn read_marker(dir: &Path) -> Option<Marker> {
    let path = dir.join(MARKER);
    if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
        return None;
    }
    Marker::parse(&String::from_utf8(std::fs::read(path).ok()?).ok()?)
}

/// The marker of `dir` when `dir` is a game folder: a name that is not [`is_reserved`], a real
/// folder ([`is_plain_dir`]) and a readable [`MARKER`]. One read for callers that need both.
pub fn game_folder_marker(dir: &Path) -> Option<Marker> {
    let named = dir.file_name().is_some_and(|n| !is_reserved(&n.to_string_lossy()));
    if named && is_plain_dir(dir) {
        read_marker(dir)
    } else {
        None
    }
}

fn write_new_marker(dir: &Path, marker: &Marker) -> std::io::Result<()> {
    let tmp = dir.join(format!("{MARKER}.tmp-{}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.attributes(HIDDEN);
    }
    let written = options.open(&tmp).and_then(|mut file| {
        std::io::Write::write_all(&mut file, marker.to_json().as_bytes())?;
        file.sync_all()
    });
    match written.and_then(|()| std::fs::rename(&tmp, dir.join(MARKER))) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// "Use this folder": adopt the user's folder `dir` as a game folder. The marker names every
/// entry already there (lowercase), so those files are never counted, recycled, owned or
/// recovered; only clips Recall saves after this are. Written atomically (temp file, then
/// rename). Nothing else in `dir` is touched. A marker already there is kept as it is. Refused
/// for a link, a hidden or a system folder.
pub fn adopt(dir: &Path) -> std::io::Result<()> {
    if !is_plain_dir(dir) {
        return Err(std::io::Error::other("not a plain folder (a link, hidden or system); not adopted"));
    }
    if std::fs::symlink_metadata(dir.join(MARKER)).is_ok() {
        return Ok(());
    }
    let mut existing: Vec<String> = std::fs::read_dir(dir)?.flatten().map(|e| e.file_name().to_string_lossy().to_lowercase()).collect();
    existing.sort();
    existing.dedup();
    write_new_marker(dir, &Marker::Adopted { existing })
}

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
        Ok(mut file) => std::io::Write::write_all(&mut file, Marker::Made.to_json().as_bytes()),
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
#[path = "game_folder_rule_tests.rs"]
mod marker_tests;

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
