//! The live readers for `launcher_libraries` (registry, files, environment)
//! and its background refresher. Read-only registry access (`KEY_READ`).

use std::time::Duration;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_SZ,
};

use crate::launcher_libraries::{collect_roots, store_roots, Hive, LibrarySources, REFRESH_EVERY};
use crate::log;

/// The machine's launchers, as they are now.
pub(crate) struct WinSources;

impl LibrarySources for WinSources {
    fn env(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn list_dir(&self, dir: &str) -> Vec<String> {
        let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
        rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()
    }

    fn read_file(&self, path: &str) -> Option<String> {
        // A manifest is a few KiB; anything huge is not one.
        let meta = std::fs::metadata(path).ok()?;
        if meta.len() > 4 * 1024 * 1024 {
            return None;
        }
        let bytes = std::fs::read(path).ok()?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn reg_subkeys(&self, hive: Hive, path: &str, values: &[&str]) -> Vec<(String, Vec<Option<String>>)> {
        let root = match hive {
            Hive::LocalMachine => HKEY_LOCAL_MACHINE,
        };
        // SAFETY: read-only keys, each closed before return; buffers outlive their calls.
        unsafe {
            let Some(key) = open(root, path) else { return Vec::new() };
            let mut out = Vec::new();
            for name in subkey_names(key) {
                let vals = values.iter().map(|v| string_value(key, &name, v)).collect();
                out.push((name, vals));
            }
            let _ = RegCloseKey(key);
            out
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn open(parent: HKEY, sub: &str) -> Option<HKEY> {
    let sub_w = wide(sub);
    let mut key = HKEY::default();
    RegOpenKeyExW(parent, PCWSTR(sub_w.as_ptr()), Some(0), KEY_READ, &mut key).is_ok().then_some(key)
}

/// Subkey names of `key` (capped: an Uninstall key holds a few hundred).
unsafe fn subkey_names(key: HKEY) -> Vec<String> {
    let mut names = Vec::new();
    for idx in 0..4096u32 {
        let mut buf = [0u16; 256];
        let mut len = buf.len() as u32;
        let st = RegEnumKeyExW(key, idx, Some(PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None);
        if st.is_err() {
            break;
        }
        names.push(String::from_utf16_lossy(&buf[..len as usize]));
    }
    names
}

/// `key\sub` value `name` as a string (REG_SZ; RegGetValueW expands a
/// REG_EXPAND_SZ into one).
unsafe fn string_value(key: HKEY, sub: &str, name: &str) -> Option<String> {
    let (sub_w, name_w) = (wide(sub), wide(name));
    let flags = RRF_RT_REG_SZ;
    let mut size = 0u32;
    let st = RegGetValueW(key, PCWSTR(sub_w.as_ptr()), PCWSTR(name_w.as_ptr()), flags, None, None, Some(&mut size));
    if st.is_err() || !(2..=64 * 1024).contains(&size) {
        return None;
    }
    let mut buf = vec![0u16; (size as usize).div_ceil(2)];
    let st = RegGetValueW(key, PCWSTR(sub_w.as_ptr()), PCWSTR(name_w.as_ptr()), flags, None, Some(buf.as_mut_ptr().cast()), Some(&mut size));
    if st.is_err() {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

/// Scan now, then every [`REFRESH_EVERY`], on a background thread at idle cost
/// (a few registry keys and small JSON files per scan).
pub(crate) fn start_refresher() {
    let spawned = std::thread::Builder::new().name("launcher-libraries".into()).spawn(|| loop {
        let roots = collect_roots(&WinSources);
        let count = roots.len();
        if store_roots(roots) {
            log!("detect: launcher game libraries (Epic, GOG, Ubisoft, EA, Battle.net, itch): {count} folder(s)");
        }
        std::thread::sleep(REFRESH_EVERY.max(Duration::from_secs(1)));
    });
    if let Err(e) = spawned {
        log!("detect: launcher game libraries not read ({e}); only Steam libraries count");
    }
}
