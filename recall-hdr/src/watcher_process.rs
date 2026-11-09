//! Process identity reads for the watcher (vendored from `recall/src/source.rs`):
//! the exe path / stem / name of a pid, a window's title, and the env-pinned
//! target lookup. Split out of `watcher.rs` (file size); behaviour unchanged.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextW, GetWindowThreadProcessId};

use crate::game_library::{game_library, Library};

// vendored from recall/src/source.rs (exe_stem): the image path
// (`image_path::exe_path`), then take the file stem. Kept local to avoid a
// cross-crate dependency for two small helpers.
pub(crate) fn exe_stem(pid: u32) -> Option<String> {
    exe_identity(pid).map(|(stem, _, _)| stem)
}

/// `(file stem, file name, the game library it lives in)` of `pid`'s
/// executable image.
pub(super) fn exe_identity(pid: u32) -> Option<(String, String, Library)> {
    let full = exe_path(pid)?;
    let path = std::path::Path::new(&full);
    Some((
        path.file_stem()?.to_string_lossy().into_owned(),
        path.file_name()?.to_string_lossy().into_owned(),
        game_library(&full),
    ))
}

/// The full path of `pid`'s executable image (Win32 form).
/// Paths past `MAX_PATH` are read too (`image_path`).
pub fn exe_path(pid: u32) -> Option<String> {
    crate::image_path::exe_path(pid)
}

/// Public identity lookup for env-pinned capture targets (`Start-Recall.ps1`).
pub fn window_identity(hwnd: isize) -> Option<(u32, String, String, String)> {
    let hwnd = HWND(hwnd as *mut _);
    let mut pid = 0u32;
    // SAFETY: GetWindowThreadProcessId tolerates a stale handle (pid stays 0).
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return None;
    }
    let (stem, name, _) = exe_identity(pid)?;
    Some((pid, stem, name, window_title(hwnd)))
}

// vendored from recall/src/source.rs (window_title): GetWindowTextW into a fixed
// buffer, UTF-16-lossy decode. Empty string on no title / error.
pub(super) fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if len <= 0 {
        String::new()
    } else {
        String::from_utf16_lossy(&buf[..len as usize])
    }
}
