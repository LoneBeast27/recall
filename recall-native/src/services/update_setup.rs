//! Setup's download, check and launch. The file lands in a per-user folder
//! (`%LOCALAPPDATA%\Recall\updates\download`), is re-opened read-only with read-only sharing
//! (no one can write, rename or delete it), hashed from that same handle, and launched with
//! the shell's `open` verb while the handle is still held: what was checked is what runs.
use std::{
    fs::{File, OpenOptions},
    io::{self, Seek, SeekFrom},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

use sha2::{Digest, Sha256};

use super::{Release, SETUP, net};

const FILE_SHARE_READ: u32 = 0x1;

/// A Setup whose bytes matched the release, still held open against changes.
pub(super) struct Verified {
    // Closed before the folder is removed (fields drop in order).
    _held: File,
    pub path: PathBuf,
    pub folder: tempfile::TempDir,
}

/// `%LOCALAPPDATA%\Recall\updates\download`, created on demand.
pub(super) fn download_root() -> Result<PathBuf, String> {
    let base = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set.")?;
    let root = PathBuf::from(base).join("Recall").join("updates").join("download");
    std::fs::create_dir_all(&root).map_err(|e| format!("Cannot create {}: {e}", root.display()))?;
    Ok(root)
}

/// Hash `file` from its start; it must match the release's size and SHA-256.
pub(super) fn verify(mut file: &File, release: &Release) -> Result<(), String> {
    file.seek(SeekFrom::Start(0)).map_err(|e| format!("Setup could not be read: {e}"))?;
    let mut hash = Sha256::new();
    let bytes = io::copy(&mut file, &mut hash).map_err(|e| format!("Setup could not be read: {e}"))?;
    if bytes != release.size || format!("{:x}", hash.finalize()) != release.digest {
        return Err("Setup did not match the GitHub checksum and size.".into());
    }
    Ok(())
}

/// Download Setup into a fresh folder under `root` and verify it from a held, deny-write handle.
pub(super) fn download(net: &dyn net::Transport, url: &str, release: &Release, root: &Path) -> Result<Verified, String> {
    let folder = tempfile::Builder::new().prefix("setup-").tempdir_in(root).map_err(|e| e.to_string())?;
    let path = folder.path().join(SETUP);
    {
        // No sharing while it is written.
        let mut out = OpenOptions::new().write(true).create_new(true).share_mode(0).open(&path).map_err(|e| format!("Cannot save Setup: {e}"))?;
        let status = net::fetch(net, url, release.size, Duration::from_secs(180), &mut out)?;
        if status != 200 {
            return Err(format!("GitHub download returned HTTP {status}."));
        }
        out.sync_all().map_err(|e| format!("Cannot save Setup: {e}"))?;
    }
    let held = OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&path).map_err(|e| format!("Setup is unavailable: {e}"))?;
    verify(&held, release)?;
    Ok(Verified { path, folder, _held: held })
}

/// One command-line argument, quoted for the C runtime's parser.
fn quote(arg: &str) -> String {
    let mut out = String::from('"');
    let mut slashes = 0;
    for ch in arg.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        out.extend(std::iter::repeat_n('\\', if ch == '"' { slashes * 2 + 1 } else { slashes }));
        slashes = 0;
        out.push(ch);
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}
/// Open the verified Setup through the shell (`open` verb) with `--update <install>`; the
/// handle on the file is released only after the shell has started it. Returns the process.
pub(super) fn launch(setup: Verified, install: &Path) -> Result<(windows::Win32::Foundation::HANDLE, tempfile::TempDir), String> {
    use windows::{
        Win32::{
            System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx},
            UI::{
                Shell::{SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
                WindowsAndMessaging::SW_SHOWNORMAL,
            },
        },
        core::{HSTRING, PCWSTR, w},
    };
    let file = HSTRING::from(setup.path.as_os_str());
    let params = HSTRING::from(format!("--update {}", quote(&install.to_string_lossy())));
    let dir = HSTRING::from(setup.folder.path().as_os_str());
    unsafe {
        // ShellExecuteEx wants COM on the calling thread (this is the updater's own thread).
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI;
        info.lpVerb = w!("open");
        info.lpFile = PCWSTR(file.as_ptr());
        info.lpParameters = PCWSTR(params.as_ptr());
        info.lpDirectory = PCWSTR(dir.as_ptr());
        info.nShow = SW_SHOWNORMAL.0;
        ShellExecuteExW(&mut info).map_err(|e| format!("Windows could not open Setup: {}", e.message()))?;
        if info.hProcess.is_invalid() {
            return Err("Windows opened Setup without a process to follow.".into());
        }
        let Verified { folder, _held, .. } = setup;
        drop(_held);
        Ok((info.hProcess, folder))
    }
}

/// Wait for the launched Setup to exit, then close its handle.
pub(super) fn wait(process: windows::Win32::Foundation::HANDLE) {
    use windows::Win32::{
        Foundation::CloseHandle,
        System::Threading::{INFINITE, WaitForSingleObject},
    };
    unsafe {
        WaitForSingleObject(process, INFINITE);
        let _ = CloseHandle(process);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::net::fake::FakeNet;

    const URL: &str = "https://github.com/LoneBeast27/recall/releases/download/v1.10.0/RecallSetup.exe";

    fn release() -> Release {
        crate::update::tests::fixture_release()
    }

    #[test]
    fn a_tampered_download_never_passes_verification() {
        let root = tempfile::tempdir().unwrap();
        let good = FakeNet::default().reply(URL, 200, None, b"abc");
        let setup = download(&good, URL, &release(), root.path()).unwrap();
        assert!(setup.path.starts_with(root.path()));
        for body in [&b"abd"[..], b"ab"] {
            let bad = FakeNet::default().reply(URL, 200, None, body);
            assert!(download(&bad, URL, &release(), root.path()).is_err(), "{body:?}");
        }
        let longer = FakeNet::default().reply(URL, 200, None, b"abcd");
        assert!(download(&longer, URL, &release(), root.path()).is_err(), "more bytes than the release reported");
        let missing = FakeNet::default();
        assert!(download(&missing, URL, &release(), root.path()).err().unwrap().contains("404"));
    }

    #[test]
    fn the_checked_file_cannot_change_until_launch_but_can_still_run() {
        let root = tempfile::tempdir().unwrap();
        let net = FakeNet::default().reply(URL, 200, None, b"abc");
        let setup = download(&net, URL, &release(), root.path()).unwrap();
        let write = OpenOptions::new().write(true).open(&setup.path);
        assert!(write.is_err(), "a writer is refused while the checked handle is held");
        assert!(std::fs::remove_file(&setup.path).is_err(), "and so is a delete");
        // What the loader asks for when it starts an exe: read + execute, sharing read and delete.
        const FILE_READ_DATA: u32 = 0x1;
        const FILE_EXECUTE: u32 = 0x20;
        const FILE_SHARE_DELETE: u32 = 0x4;
        let run = OpenOptions::new().access_mode(FILE_READ_DATA | FILE_EXECUTE).share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE).open(&setup.path);
        assert!(run.is_ok(), "{run:?}");
        drop(run);
        let path = setup.path.clone();
        drop(setup);
        assert!(!path.exists(), "the download folder goes with the handle");
    }

    #[test]
    fn install_paths_are_quoted_for_setup() {
        assert_eq!(quote(r"C:\Users\A B\Recall"), r#""C:\Users\A B\Recall""#);
        assert_eq!(quote(r"C:\"), r#""C:\\""#);
        assert_eq!(quote(r#"a"b"#), r#""a\"b""#);
    }
}
