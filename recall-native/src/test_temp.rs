//! Tests only: a scratch folder in %TEMP% that is deleted when the guard drops, also when the
//! test panics (unwinding runs Drop). The storage tests used to leave hundreds of MB per run
//! there; on 2026-11-05 the leftovers filled C: (about 130 GB).
use std::ops::Deref;
use std::path::{Path, PathBuf};

pub struct TempDir(PathBuf);

impl TempDir {
    /// `%TEMP%\<name>`, emptied first (a crashed run may have left it), then created.
    pub fn new(name: impl AsRef<str>) -> Self {
        let dir = std::env::temp_dir().join(name.as_ref());
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create a test scratch folder");
        TempDir(dir)
    }

    /// `<worktree>\.tmp\sandbox\<name>-<pid>`, emptied first, then created: tests that place
    /// user-like clips never touch %TEMP%, a real cache or a real clip folder.
    pub fn sandbox(name: impl AsRef<str>) -> Self {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".tmp").join("sandbox");
        let dir = base.join(format!("{}-{}", name.as_ref(), std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create a test sandbox folder");
        TempDir(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// Removes `%TEMP%\<prefix>-<pid>...` folders whose process has ended: the shared per-process test
/// folders (the test cache, the imports table, the live-sound clips) cannot be dropped by any
/// one test, so each run clears the ones earlier runs left (at most one run's worth remains).
pub fn sweep_dead(prefix: &str) {
    let me = std::process::id();
    let Ok(listing) = std::fs::read_dir(std::env::temp_dir()) else { return };
    for entry in listing.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix(prefix).and_then(|r| r.strip_prefix('-')) else { continue };
        let Ok(pid) = rest.split('-').next().unwrap_or("").parse::<u32>() else { continue };
        if pid != me && !alive(pid) && entry.file_type().is_ok_and(|t| t.is_dir()) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// A process with this id is still running AND is a Recall test binary (a reused pid of another
/// program does not keep an old run's folder).
fn alive(pid: u32) -> bool {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: a query-only handle, closed before returning; the buffer outlives the call.
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut code = 0u32;
        let running = GetExitCodeProcess(handle, &mut code).is_ok() && code == STILL_ACTIVE.0 as u32;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let named = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(handle);
        let image = String::from_utf16_lossy(&buf[..len as usize]).to_ascii_lowercase();
        let ours = !named || std::path::Path::new(&image).file_name().is_some_and(|n| n.to_string_lossy().starts_with("recall"));
        running && ours
    }
}

impl Deref for TempDir {
    type Target = PathBuf;
    fn deref(&self) -> &PathBuf {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::TempDir;

    #[test]
    fn the_folder_goes_when_the_guard_drops_even_on_a_panic() {
        let path = {
            let dir = TempDir::new(format!("recall-temp-guard-{}", std::process::id()));
            std::fs::write(dir.join("a.bin"), b"x").unwrap();
            dir.to_path_buf()
        };
        assert!(!path.exists(), "dropped: gone");
        let name = format!("recall-temp-guard-panic-{}", std::process::id());
        let caught = std::panic::catch_unwind(|| {
            let dir = TempDir::new(&name);
            std::fs::write(dir.join("a.bin"), b"x").unwrap();
            panic!("a failing test");
        });
        assert!(caught.is_err());
        assert!(!std::env::temp_dir().join(&name).exists(), "a panicking test leaves nothing");
    }
}
