//! What the launcher-library refresher (`launcher_libraries_win`) waits on between scans,
//! in place of its fixed 5 min rescan: the registry keys the scan reads
//! (`RegNotifyChangeKeyValue`) and the folders it lists (`FindFirstChangeNotificationW`).
//! A key or folder that does not exist yet (a launcher not installed) is watched through
//! its nearest existing parent, for a new child only; once it appears the rescan watches it.
//! A long safety rescan ([`SAFETY_EVERY`]) stays for what no notification covers (more than
//! [`MAX_WATCHES`] folders, a watch Windows refused).

use std::time::Duration;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Registry::{
    RegCloseKey, RegNotifyChangeKeyValue, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_NOTIFY, REG_NOTIFY_CHANGE_LAST_SET,
    REG_NOTIFY_CHANGE_NAME, REG_NOTIFY_FILTER,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForMultipleObjects};

use crate::launcher_libraries::LibrarySources;

/// The rescan with no notification at all.
// timer-ok: a safety net for what the notifications cannot cover (see the module doc).
pub(crate) const SAFETY_EVERY: Duration = Duration::from_secs(30 * 60);
/// A burst of changes (an install writes many files and values) is scanned once, this long
/// after the first.
pub(crate) const SETTLE: Duration = Duration::from_secs(5);
/// `WaitForMultipleObjects` takes at most 64 handles.
pub(crate) const MAX_WATCHES: usize = 60;

/// The `HKLM` keys `launcher_libraries` reads (GOG, Ubisoft, EA, Battle.net's uninstall entries).
pub(crate) const REG_KEYS: [&str; 5] = [
    "SOFTWARE\\WOW6432Node\\GOG.com\\Games",
    "SOFTWARE\\WOW6432Node\\Ubisoft\\Launcher\\Installs",
    "SOFTWARE\\WOW6432Node\\Electronic Arts",
    "SOFTWARE\\WOW6432Node\\EA Games",
    "SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall",
];

const FILE_NOTIFY_CHANGE_FILE_NAME: u32 = 0x1;
const FILE_NOTIFY_CHANGE_DIR_NAME: u32 = 0x2;
const FILE_NOTIFY_CHANGE_LAST_WRITE: u32 = 0x10;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn FindFirstChangeNotificationW(path: *const u16, subtree: i32, filter: u32) -> isize;
    fn FindCloseChangeNotification(handle: isize) -> i32;
}

/// A folder to watch: its path, and whether changes below it count (an itch location's
/// games hold their receipt two levels down).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DirWatch {
    pub path: String,
    pub subtree: bool,
}

/// The folders the scan lists: Epic's manifests, itch's settings and install locations
/// (`itch_locations`: the scan's own list of them, passed in).
pub(crate) fn dirs(src: &dyn LibrarySources, itch_locations: &[String]) -> Vec<DirWatch> {
    let mut out = Vec::new();
    if let Some(pd) = src.env("ProgramData") {
        out.push(DirWatch { path: format!("{}\\Epic\\EpicGamesLauncher\\Data\\Manifests", pd.trim_end_matches('\\')), subtree: false });
    }
    if let Some(appdata) = src.env("APPDATA") {
        out.push(DirWatch { path: format!("{}\\itch", appdata.trim_end_matches('\\')), subtree: false });
    }
    out.extend(itch_locations.iter().map(|l| DirWatch { path: l.trim().trim_end_matches(['\\', '/']).to_string(), subtree: true }));
    out
}

/// The nearest existing folder of `path` (itself first), and whether it is `path` itself.
pub(crate) fn nearest(path: &str, exists: impl Fn(&str) -> bool) -> Option<(String, bool)> {
    let mut at = path.trim_end_matches('\\').to_string();
    let mut own = true;
    loop {
        if exists(&at) {
            return Some((at, own));
        }
        let cut = at.rfind('\\')?;
        at.truncate(cut);
        own = false;
        if at.ends_with(':') {
            at.push('\\'); // a drive root
            return exists(&at).then_some((at, false));
        }
    }
}

/// The armed notifications; dropping them closes every handle.
pub(crate) struct Watches {
    keys: Vec<(HKEY, HANDLE)>,
    dirs: Vec<isize>,
}

impl Watches {
    /// Arm a watch on every key in [`REG_KEYS`] and every folder in `dirs` (each, or its
    /// nearest existing parent for a new child), up to [`MAX_WATCHES`].
    pub(crate) fn arm(dirs: &[DirWatch]) -> Watches {
        let mut w = Watches { keys: Vec::new(), dirs: Vec::new() };
        for key in REG_KEYS {
            w.arm_key(key);
        }
        for d in dirs {
            if w.len() >= MAX_WATCHES {
                break;
            }
            w.arm_dir(d);
        }
        w
    }

    pub(crate) fn len(&self) -> usize {
        self.keys.len() + self.dirs.len()
    }

    fn arm_key(&mut self, path: &str) {
        let mut at = path.to_string();
        let mut own = true;
        // SAFETY: read-only notify keys and an event, all closed on drop.
        unsafe {
            loop {
                let wide: Vec<u16> = at.encode_utf16().chain(Some(0)).collect();
                let mut key = HKEY::default();
                if RegOpenKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(wide.as_ptr()), Some(0), KEY_NOTIFY, &mut key).is_ok() {
                    let filter: REG_NOTIFY_FILTER = if own { REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_LAST_SET } else { REG_NOTIFY_CHANGE_NAME };
                    let Ok(event) = CreateEventW(None, true, false, None) else {
                        let _ = RegCloseKey(key);
                        return;
                    };
                    if RegNotifyChangeKeyValue(key, own, filter, Some(event), true).is_ok() {
                        self.keys.push((key, event));
                    } else {
                        let _ = RegCloseKey(key);
                        let _ = CloseHandle(event);
                    }
                    return;
                }
                let Some(cut) = at.rfind('\\') else { return };
                at.truncate(cut);
                own = false;
            }
        }
    }

    fn arm_dir(&mut self, d: &DirWatch) {
        let Some((at, own)) = nearest(&d.path, |p| std::path::Path::new(p).is_dir()) else { return };
        // A subtree (an itch location, where games write logs as they run) counts only names
        // coming and going (a new game folder, its receipt); a plain folder (Epic's manifests,
        // itch's settings) also counts a rewrite.
        let filter = match (own, d.subtree) {
            (false, _) => FILE_NOTIFY_CHANGE_DIR_NAME,
            (true, true) => FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME,
            (true, false) => FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE,
        };
        let wide: Vec<u16> = at.encode_utf16().chain(Some(0)).collect();
        // SAFETY: a NUL-terminated path; the handle is closed on drop.
        let h = unsafe { FindFirstChangeNotificationW(wide.as_ptr(), i32::from(own && d.subtree), filter) };
        if h != 0 && h != -1 {
            self.dirs.push(h);
        }
    }

    /// Wait up to `timeout` for any notification: true when one fired.
    pub(crate) fn wait(&self, timeout: Duration) -> bool {
        let handles: Vec<HANDLE> = self.keys.iter().map(|(_, e)| *e).chain(self.dirs.iter().map(|h| HANDLE(*h as *mut _))).collect();
        let ms = timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32;
        if handles.is_empty() {
            std::thread::sleep(timeout);
            return false;
        }
        // SAFETY: every handle is open until `self` drops.
        let got = unsafe { WaitForMultipleObjects(&handles, false, ms) };
        got.0.wrapping_sub(WAIT_OBJECT_0.0) < handles.len() as u32
    }
}

impl Drop for Watches {
    fn drop(&mut self) {
        // SAFETY: handles this value opened, closed once.
        unsafe {
            for (key, event) in self.keys.drain(..) {
                let _ = RegCloseKey(key);
                let _ = CloseHandle(event);
            }
            for h in self.dirs.drain(..) {
                let _ = FindCloseChangeNotification(h);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_the_scan_reads_is_watched_and_no_other() {
        // The scan's key literals, as its source writes them ("SOFTWARE\\...", escaped).
        let scan = include_str!("launcher_libraries.rs");
        let mut read: Vec<String> = scan
            .split(r#""SOFTWARE\\"#)
            .skip(1)
            .map(|rest| format!(r"SOFTWARE\{}", &rest[..rest.find('"').expect("closing quote")]).replace(r"\\", r"\"))
            .collect();
        read.sort();
        read.dedup();
        let mut watched: Vec<String> = REG_KEYS.iter().map(|k| k.to_string()).collect();
        watched.sort();
        assert_eq!(read, watched);
    }

    #[test]
    fn a_missing_folder_is_watched_through_its_nearest_parent() {
        let have = ["C:\\", "C:\\ProgramData", "C:\\ProgramData\\Epic"];
        let exists = |p: &str| have.contains(&p);
        assert_eq!(nearest("C:\\ProgramData\\Epic", exists), Some(("C:\\ProgramData\\Epic".into(), true)));
        assert_eq!(nearest("C:\\ProgramData\\Epic\\EpicGamesLauncher\\Data\\Manifests", exists), Some(("C:\\ProgramData\\Epic".into(), false)));
        assert_eq!(nearest("D:\\Games\\itch", exists), None, "no such drive");
        assert_eq!(nearest("C:\\Nope\\x", exists), Some(("C:\\".into(), false)));
    }

    #[test]
    fn the_folders_are_epics_manifests_and_itchs_settings_and_locations() {
        struct Env;
        impl LibrarySources for Env {
            fn env(&self, name: &str) -> Option<String> {
                match name {
                    "ProgramData" => Some("C:\\ProgramData".into()),
                    "APPDATA" => Some("C:\\Users\\u\\AppData\\Roaming\\".into()),
                    _ => None,
                }
            }
            fn list_dir(&self, _: &str) -> Vec<String> {
                Vec::new()
            }
            fn read_file(&self, _: &str) -> Option<String> {
                None
            }
            fn reg_subkeys(&self, _: crate::launcher_libraries::Hive, _: &str, _: &[&str]) -> Vec<(String, Vec<Option<String>>)> {
                Vec::new()
            }
        }
        let got = dirs(&Env, &["D:\\Games\\".to_string()]);
        assert_eq!(got, vec![
            DirWatch { path: "C:\\ProgramData\\Epic\\EpicGamesLauncher\\Data\\Manifests".into(), subtree: false },
            DirWatch { path: "C:\\Users\\u\\AppData\\Roaming\\itch".into(), subtree: false },
            DirWatch { path: "D:\\Games".into(), subtree: true },
        ]);
    }

    #[test]
    fn a_watch_fires_when_its_folder_changes() {
        let dir = std::env::temp_dir().join(format!("recall-libwatch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut w = Watches { keys: Vec::new(), dirs: Vec::new() };
        w.arm_dir(&DirWatch { path: dir.to_string_lossy().into_owned(), subtree: false });
        assert_eq!(w.len(), 1);
        assert!(!w.wait(Duration::from_millis(50)), "nothing changed");
        std::fs::write(dir.join("a.item"), b"{}").unwrap();
        assert!(w.wait(Duration::from_secs(5)), "a new manifest wakes the refresher");
        drop(w);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_itch_location_wakes_for_a_new_name_not_a_log_write() {
        let dir = std::env::temp_dir().join(format!("recall-libwatch-sub-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("game")).unwrap();
        std::fs::write(dir.join("game").join("log.txt"), b"a").unwrap();
        let mut w = Watches { keys: Vec::new(), dirs: Vec::new() };
        w.arm_dir(&DirWatch { path: dir.to_string_lossy().into_owned(), subtree: true });
        std::fs::write(dir.join("game").join("log.txt"), b"a game writing its log").unwrap();
        assert!(!w.wait(Duration::from_millis(300)), "a write to a file that exists: no rescan");
        std::fs::create_dir_all(dir.join("game").join(".itch")).unwrap();
        assert!(w.wait(Duration::from_secs(5)), "a new folder (an install's receipt folder) wakes it");
        drop(w);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
