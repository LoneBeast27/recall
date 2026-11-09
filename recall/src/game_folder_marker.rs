//! A game folder's [`MARKER`](super::MARKER): what it says, which folder it belongs to, and which
//! files in it are Recall's. Part of `game_folder_rule` (std + serde_json; shared by the engine,
//! the controller and the app).
//!
//! - **Bound to its folder.** A marker names the folder's full path and, on a local NTFS or ReFS
//!   volume, its file ID (volume serial + file index). [`game_folder_marker`] refuses a marker
//!   whose path or ID differs, so a copied, renamed or moved folder ("Apex Legends - Copy") is
//!   not a game folder: its files are never counted, owned or recycled. Elsewhere (FAT, exFAT, a
//!   network share, no stable ID) the marker stores `"id": null` and is bound by path only.
//! - **Owned = saved.** In every game folder, made or adopted, a file is Recall's only when its
//!   name is on the marker's `saved` list ([`Marker::owns`]). The engine adds a name after each
//!   final rename into the folder ([`record_saved`]); a `.part` is never on it. Long recordings
//!   still being written are on `writing` ([`record_writing`]), only so the next start recovers
//!   Recall's own parts.
//! - **Legacy markers** (empty, `{"version":1}`, the first build's sentence: no `folder`) still
//!   make a game folder, but own nothing. The engine's next save there rewrites the marker with a
//!   binding and an empty list, so only clips saved from then on are Recall's (the safe rule:
//!   an older build cannot tell its clips from files the user dropped in).
use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;

use super::{is_plain_dir, is_reserved, HIDDEN, MARKER};

/// The largest [`MARKER`] read or written: 4 MiB (about 50,000 saved clip names). A bigger file
/// is not a marker, so the folder is not a game folder.
pub const MARKER_MAX: u64 = 4 * 1024 * 1024;

/// A folder's file ID: its volume's serial number and its file index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileId {
    pub volume: u32,
    pub index: u64,
}

/// The folder a marker belongs to: its full path, and its [`FileId`] where the volume has a
/// stable one (`None`: bound by path only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub folder: String,
    pub id: Option<FileId>,
}

/// Who made the folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Recall made it.
    Made,
    /// The user's folder, adopted ("Use this folder").
    Adopted,
}

/// What a game folder's [`MARKER`] says. Name lists are lowercase, sorted, without repeats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub kind: Kind,
    /// `None`: a legacy marker, which owns nothing.
    pub binding: Option<Binding>,
    /// Adopted: the entries found at adoption (information only).
    pub existing: Vec<String>,
    /// Files Recall saved here under their final names: the only files it owns.
    pub saved: Vec<String>,
    /// Long-recording `.part` files Recall is writing here (recovery only, never owned).
    pub writing: Vec<String>,
}

fn lower(name: &str) -> String {
    name.to_lowercase()
}

fn has(list: &[String], name: &str) -> bool {
    list.binary_search(&lower(name)).is_ok()
}

impl Marker {
    /// A marker for `binding` with nothing saved.
    pub fn new(kind: Kind, binding: Option<Binding>) -> Marker {
        Marker { kind, binding, existing: Vec::new(), saved: Vec::new(), writing: Vec::new() }
    }

    /// `file_name` in this folder is Recall's (counted, owned, recycled): a bound marker that has
    /// it on `saved`. Callers still check the Recall clip name.
    pub fn owns(&self, file_name: &str) -> bool {
        self.binding.is_some() && has(&self.saved, file_name)
    }

    /// `part_name` is a long recording Recall was writing here (recovered at the next start).
    pub fn is_writing(&self, part_name: &str) -> bool {
        self.binding.is_some() && has(&self.writing, part_name)
    }

    /// The marker's text (a legacy marker without a binding writes `{"version":1}`).
    pub fn to_json(&self) -> String {
        let Some(binding) = &self.binding else { return r#"{"version":1}"#.to_string() };
        let id = binding.id.map(|id| serde_json::json!({ "volume": id.volume, "index": id.index }));
        let mut value = serde_json::json!({
            "version": 1,
            "folder": binding.folder,
            "id": id,
            "saved": self.saved,
            "writing": self.writing,
        });
        if self.kind == Kind::Adopted {
            value["adopted"] = true.into();
            value["existing"] = self.existing.clone().into();
        }
        value.to_string()
    }

    /// Parses a marker's text. Empty, `{"version":1}` without `folder`, and the first build's
    /// sentence are legacy markers (made, unbound, owning nothing). `None`: unreadable, so the
    /// folder is not a game folder.
    pub fn parse(text: &str) -> Option<Marker> {
        let text = text.trim_start_matches('\u{feff}').trim();
        if text.is_empty() || text.starts_with("Recall made this game folder") {
            return Some(Marker::new(Kind::Made, None));
        }
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        if value.get("version")?.as_u64()? != 1 {
            return None;
        }
        let adopted = value.get("adopted").map_or(Some(false), serde_json::Value::as_bool)?;
        let names = |key: &str, required: bool| -> Option<Vec<String>> {
            let Some(list) = value.get(key) else { return (!required).then(Vec::new) };
            let mut names = list.as_array()?.iter().map(|v| v.as_str().map(lower)).collect::<Option<Vec<_>>>()?;
            names.sort();
            names.dedup();
            Some(names)
        };
        let kind = if adopted { Kind::Adopted } else { Kind::Made };
        let existing = names("existing", adopted)?;
        let binding = match value.get("folder") {
            None => None,
            Some(folder) => Some(Binding { folder: folder.as_str()?.to_string(), id: parse_id(value.get("id"))? }),
        };
        // A legacy marker's lists mean nothing: it owns nothing until a save rebinds it.
        let (saved, writing) = if binding.is_some() { (names("saved", false)?, names("writing", false)?) } else { (Vec::new(), Vec::new()) };
        Some(Marker { kind, binding, existing, saved, writing })
    }
}

/// `"id"`: `null` or missing -> path only; `{volume, index}` -> the ID; anything else: unreadable.
fn parse_id(value: Option<&serde_json::Value>) -> Option<Option<FileId>> {
    match value {
        None | Some(serde_json::Value::Null) => Some(None),
        Some(v) => {
            let volume = u32::try_from(v.get("volume")?.as_u64()?).ok()?;
            Some(Some(FileId { volume, index: v.get("index")?.as_u64()? }))
        }
    }
}

/// A path in comparable form: absolute, `\`-separated, no `\\?\` prefix, no trailing `\`,
/// lowercase.
pub fn path_key(path: &Path) -> String {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let text = absolute.to_string_lossy().replace('/', "\\");
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    text.trim_end_matches('\\').to_lowercase()
}

/// `marker` belongs to `dir` now: a legacy marker always (it owns nothing anyway); a bound one
/// when the path matches and, if it stored an ID, `id_of(dir)` gives the same one.
pub fn bound_to(marker: &Marker, dir: &Path, id_of: &dyn Fn(&Path) -> Option<FileId>) -> bool {
    let Some(binding) = &marker.binding else { return true };
    path_key(Path::new(&binding.folder)) == path_key(dir) && binding.id.is_none_or(|id| id_of(dir) == Some(id))
}

/// `dir`'s [`MARKER`], read and parsed; `None` when missing, unreadable or over [`MARKER_MAX`].
/// Says nothing about the binding: [`game_folder_marker`] checks that.
pub fn read_marker(dir: &Path) -> Option<Marker> {
    let path = dir.join(MARKER);
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MARKER_MAX {
        return None;
    }
    // At most one byte past the cap, so a file that grew after the check is refused too.
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(std::fs::File::open(path).ok()?, MARKER_MAX + 1), &mut bytes).ok()?;
    if bytes.len() as u64 > MARKER_MAX {
        return None;
    }
    Marker::parse(&String::from_utf8(bytes).ok()?)
}

/// The marker of `dir` when `dir` is a game folder: a name that is not [`is_reserved`], a real
/// folder ([`is_plain_dir`]), a readable [`MARKER`] and a binding to this very folder
/// ([`bound_to`] with [`folder_id`]). One read for callers that need both.
pub fn game_folder_marker(dir: &Path) -> Option<Marker> {
    let named = dir.file_name().is_some_and(|n| !is_reserved(&n.to_string_lossy()));
    if !named || !is_plain_dir(dir) {
        return None;
    }
    read_marker(dir).filter(|m| bound_to(m, dir, &folder_id))
}

/// The binding for `dir` now (its full path and [`folder_id`]). A volume without a stable ID is
/// noted once ([`take_notes`]).
pub fn bind(dir: &Path) -> Binding {
    bind_with(dir, &folder_id)
}

/// [`bind`] with the ID lookup given (tests inject `None`).
pub fn bind_with(dir: &Path, id_of: &dyn Fn(&Path) -> Option<FileId>) -> Binding {
    let folder = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf()).to_string_lossy().into_owned();
    let id = id_of(dir);
    if id.is_none() {
        note_once(&format!("path-only:{}", path_key(dir)), format!("game folder {folder}: its volume has no stable folder ID; its marker is bound by path only"));
    }
    Binding { folder, id }
}

/// Writes `marker` to `dir`'s [`MARKER`] through a temp file and a rename (replacing one there).
pub fn write_new_marker(dir: &Path, marker: &Marker) -> std::io::Result<()> {
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

/// One read-modify-write of a marker at a time in this process: the saver, the mover and the
/// long-recording thread all go through [`record_saved`] / [`record_writing`].
static MARKER_LOCK: Mutex<()> = Mutex::new(());

/// Folders whose marker could not take another name (a failed write, or [`MARKER_MAX`]): no
/// more saves go there this run ([`save_refused`]).
static REFUSED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// One-time notes for the engine's log ([`take_notes`]): keys already noted, lines not taken yet.
static NOTES: Mutex<(Option<HashSet<String>>, Vec<String>)> = Mutex::new((None, Vec::new()));

fn note_once(key: &str, line: String) {
    let mut notes = NOTES.lock().unwrap_or_else(|p| p.into_inner());
    if notes.0.get_or_insert_with(HashSet::new).insert(key.to_string()) {
        notes.1.push(line);
    }
}

/// The notes since the last call, each said once per run.
pub fn take_notes() -> Vec<String> {
    std::mem::take(&mut NOTES.lock().unwrap_or_else(|p| p.into_inner()).1)
}

/// A marker update failed for `dir` earlier this run: new clips for it go to the clip folder.
pub fn save_refused(dir: &Path) -> bool {
    REFUSED.lock().unwrap_or_else(|p| p.into_inner()).as_ref().is_some_and(|set| set.contains(&path_key(dir)))
}

fn refuse(dir: &Path, why: &std::io::Error) {
    REFUSED.lock().unwrap_or_else(|p| p.into_inner()).get_or_insert_with(HashSet::new).insert(path_key(dir));
    note_once(&format!("refused:{}", path_key(dir)), format!("game folder {}: marker not updated ({why}); new clips for it go to the clip folder", dir.display()));
}

fn is_part(name: &str) -> bool {
    lower(name).ends_with(".part")
}

/// The engine renamed a file into place as `path` (a clip in place, a mover's copy or rename, a
/// long recording or one of its `rec-NN` parts finalized, a kept or recovered recording). When
/// `path`'s folder is a game folder, the name joins the marker's `saved` list, so the folder owns
/// it; `replaces` (the `.part` it came from) leaves `writing`. Call it only after the rename
/// succeeded. A `.part` name is refused (`InvalidInput`). In any other folder: nothing to do.
/// On a failure, or when the marker would pass [`MARKER_MAX`], the file stays unowned (never
/// evicted) and the folder takes no more saves this run ([`save_refused`]).
pub fn record_saved(path: &Path, replaces: Option<&Path>) -> std::io::Result<()> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if is_part(&name) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "a .part is never a saved clip"));
    }
    let gone = replaces.and_then(Path::file_name).map(|n| lower(&n.to_string_lossy()));
    update(path, |m| {
        m.saved.push(lower(&name));
        if let Some(gone) = &gone {
            m.writing.retain(|n| n != gone);
        }
    })
}

/// A long recording's `.part` at `path` is open for writing: it joins `writing`, so a crash leaves
/// a part the next start recovers. Never owned. Same rules as [`record_saved`].
pub fn record_writing(path: &Path) -> std::io::Result<()> {
    let name = path.file_name().map(|n| lower(&n.to_string_lossy())).unwrap_or_default();
    update(path, |m| m.writing.push(name.clone()))
}

fn update(path: &Path, change: impl FnOnce(&mut Marker)) -> std::io::Result<()> {
    let Some(dir) = path.parent().filter(|_| path.file_name().is_some()) else { return Ok(()) };
    let _lock = MARKER_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    if save_refused(dir) {
        return Err(std::io::Error::other("this game folder's marker could not be updated earlier this run"));
    }
    let Some(mut marker) = game_folder_marker(dir) else { return Ok(()) };
    if marker.binding.is_none() {
        // A legacy marker: bound now, and only what is saved from now on is Recall's.
        marker = Marker { existing: marker.existing, ..Marker::new(marker.kind, Some(bind(dir))) };
    }
    change(&mut marker);
    // Names that are gone (deleted, moved) leave the lists, only after a complete listing.
    let name_of = |e: std::fs::DirEntry| lower(&e.file_name().to_string_lossy());
    let present = std::fs::read_dir(dir).ok().and_then(|l| l.map(|e| e.map(name_of)).collect::<std::io::Result<HashSet<_>>>().ok());
    for list in [&mut marker.saved, &mut marker.writing] {
        if let Some(present) = &present {
            list.retain(|n| present.contains(n));
        }
        list.sort();
        list.dedup();
    }
    let result = if marker.to_json().len() as u64 > MARKER_MAX {
        Err(std::io::Error::other("the marker would pass 4 MiB"))
    } else {
        write_new_marker(dir, &marker)
    };
    if let Err(e) = &result {
        refuse(dir, e);
    }
    result
}

/// Tests: forget the folders [`save_refused`] holds.
#[cfg(test)]
pub fn clear_refused_for_test() {
    *REFUSED.lock().unwrap_or_else(|p| p.into_inner()) = None;
}

/// `dir`'s [`FileId`] when it sits on a local NTFS or ReFS volume that gives a non-zero one;
/// `None` elsewhere (FAT, exFAT, a network share or mapped drive, an error).
#[cfg(windows)]
pub fn folder_id(dir: &Path) -> Option<FileId> {
    win::folder_id(dir)
}

#[cfg(not(windows))]
pub fn folder_id(_dir: &Path) -> Option<FileId> {
    None
}

#[cfg(windows)]
mod win {
    use super::FileId;
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;

    #[repr(C)]
    #[derive(Default)]
    struct ByHandle {
        attributes: u32,
        times: [u32; 6],
        volume: u32,
        size_high: u32,
        size_low: u32,
        links: u32,
        index_high: u32,
        index_low: u32,
    }

    // The app also links these through the `windows` crate, with wrapper types of the same ABI
    // (HANDLE, BOOL, the struct); this file stays std-only for the engine and the controller.
    #[allow(clashing_extern_declarations)]
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(file: *mut c_void, info: *mut ByHandle) -> i32;
        fn GetVolumeInformationByHandleW(file: *mut c_void, name: *mut u16, name_len: u32, serial: *mut u32, max_component: *mut u32, flags: *mut u32, fs_name: *mut u16, fs_len: u32) -> i32;
        fn GetDriveTypeW(root: *const u16) -> u32;
    }

    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const DRIVE_REMOTE: u32 = 4;

    pub(super) fn folder_id(dir: &Path) -> Option<FileId> {
        let absolute = std::path::absolute(dir).ok()?;
        let text = absolute.to_string_lossy().to_string();
        let plain = text.strip_prefix(r"\\?\").unwrap_or(&text);
        // A share (`\\server\...`, `\\?\UNC\...`) or anything without a drive letter: path only.
        let drive = plain.as_bytes();
        if drive.len() < 2 || drive[1] != b':' || !drive[0].is_ascii_alphabetic() {
            return None;
        }
        let root: Vec<u16> = std::ffi::OsStr::new(&format!("{}:\\", drive[0] as char)).encode_wide().chain([0]).collect();
        // SAFETY: `root` is a NUL-terminated UTF-16 string that outlives the call.
        if unsafe { GetDriveTypeW(root.as_ptr()) } == DRIVE_REMOTE {
            return None;
        }
        let file = std::fs::OpenOptions::new().access_mode(FILE_READ_ATTRIBUTES).share_mode(7).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(&absolute).ok()?;
        let handle = file.as_raw_handle();
        let mut fs = [0u16; 32];
        // SAFETY: `handle` is open for the call; `fs` is a writable buffer of the length given; the
        // other out pointers may be null.
        let named = unsafe { GetVolumeInformationByHandleW(handle, std::ptr::null_mut(), 0, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), fs.as_mut_ptr(), fs.len() as u32) };
        let fs_name = String::from_utf16_lossy(&fs[..fs.iter().position(|&c| c == 0).unwrap_or(fs.len())]).to_uppercase();
        if named == 0 || !matches!(fs_name.as_str(), "NTFS" | "REFS") {
            return None;
        }
        let mut info = ByHandle::default();
        // SAFETY: `handle` is open for the call and `info` matches BY_HANDLE_FILE_INFORMATION.
        if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
            return None;
        }
        let index = (u64::from(info.index_high) << 32) | u64::from(info.index_low);
        (info.volume != 0 && index != 0).then_some(FileId { volume: info.volume, index })
    }
}

#[cfg(test)]
#[path = "game_folder_marker_tests.rs"]
mod tests;
