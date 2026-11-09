//! The user's folders every path resolver starts from, and the unit-test guard on them.
//!
//! - [`known`]: Shell known folders; [`appdata`] / [`local_appdata`]: `%APPDATA%` / `%LOCALAPPDATA%`.
//! - Unit tests resolve all of them under a temp root: one injected for the thread ([`inject`]), else the
//!   run's shared `%TEMP%\recall-native-unit-tests`.
//! - [`checked`]: every resolver passes its result through it. Under `cfg(test)` a path inside the
//!   real user's Recall folders panics (unit tests once wrote the real `open-clips.json`, since
//!   nothing marks a `cargo test` run at run time). Release builds compile it to the identity.
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Known {
    Videos,
}

/// The folder now, or `None` when Windows cannot name it.
pub(crate) fn known(which: Known) -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(root) = test_root::get() {
        return Some(root.join(test_root::name(which)));
    }
    os_known(match which {
        Known::Videos => &windows::Win32::UI::Shell::FOLDERID_Videos,
    })
}

/// `%APPDATA%` (the injected root's `AppData\Roaming` in a test that injected one).
pub(crate) fn appdata() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(root) = test_root::get() {
        return Some(root.join(r"AppData\Roaming"));
    }
    std::env::var_os("APPDATA").filter(|p| !p.is_empty()).map(PathBuf::from)
}

/// `%LOCALAPPDATA%` (the injected root's `AppData\Local` in a test).
pub(crate) fn local_appdata() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(root) = test_root::get() {
        return Some(root.join(r"AppData\Local"));
    }
    std::env::var_os("LOCALAPPDATA").filter(|p| !p.is_empty()).map(PathBuf::from)
}

/// Every resolver's result passes through here (see the module doc).
#[inline]
pub(crate) fn checked(path: PathBuf) -> PathBuf {
    #[cfg(test)]
    test_root::assert_outside_real(&path);
    path
}

fn os_known(id: &windows::core::GUID) -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    // SAFETY: the returned buffer is copied, then freed with the allocator that made it.
    unsafe {
        let raw = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let path = raw.to_string().ok();
        CoTaskMemFree(Some(raw.0 as *const _));
        path.filter(|p| !p.is_empty()).map(PathBuf::from)
    }
}

/// Tests: this thread's folders resolve under `root` until the guard drops.
#[cfg(test)]
pub(crate) fn inject(root: &std::path::Path) -> test_root::Injected {
    test_root::set(Some(root.to_path_buf()));
    test_root::Injected(())
}

#[cfg(test)]
pub(crate) mod test_root {
    use super::Known;
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};
    use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, FOLDERID_RoamingAppData, FOLDERID_UserProgramFiles, FOLDERID_Videos};

    thread_local! { static ROOT: RefCell<Option<PathBuf>> = const { RefCell::new(None) }; }

    pub(crate) struct Injected(pub(super) ());

    impl Drop for Injected {
        fn drop(&mut self) {
            set(None);
        }
    }

    pub(super) fn set(root: Option<PathBuf>) {
        ROOT.with(|r| *r.borrow_mut() = root);
    }

    /// This thread's injected root, else the run's shared one (`%TEMP%\recall-native-unit-tests`):
    /// a unit test never starts from the real profile, injected or not.
    pub(crate) fn get() -> Option<PathBuf> {
        ROOT.with(|r| r.borrow().clone()).or_else(|| Some(std::env::temp_dir().join("recall-native-unit-tests")))
    }

    pub(super) fn name(which: Known) -> &'static str {
        match which {
            Known::Videos => "Videos",
        }
    }

    fn key(path: &Path) -> String {
        path.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase()
    }

    /// The real user's Recall folders: known folders and environment both (they may differ).
    fn real_dirs() -> &'static [String] {
        static DIRS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
        DIRS.get_or_init(|| {
            let known = [(&FOLDERID_RoamingAppData, "Recall"), (&FOLDERID_RoamingAppData, "recall-hdr"), (&FOLDERID_LocalAppData, "Recall"), (&FOLDERID_UserProgramFiles, "Recall"), (&FOLDERID_Videos, "Recall")];
            let env = [("APPDATA", "Recall"), ("APPDATA", "recall-hdr"), ("LOCALAPPDATA", "Recall"), ("LOCALAPPDATA", r"Programs\Recall"), ("USERPROFILE", r"Videos\Recall")];
            let from_known = known.into_iter().filter_map(|(id, sub)| super::os_known(id).map(|p| p.join(sub)));
            let from_env = env.into_iter().filter_map(|(var, sub)| std::env::var_os(var).filter(|v| !v.is_empty()).map(|v| PathBuf::from(v).join(sub)));
            from_known.chain(from_env).map(|p| key(&p)).collect()
        })
    }

    pub(super) fn assert_outside_real(path: &Path) {
        let p = key(&std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()));
        if let Some(dir) = real_dirs().iter().find(|d| p == **d || (p.starts_with(d.as_str()) && p.as_bytes().get(d.len()) == Some(&b'\\'))) {
            panic!("unit test resolved {} inside the real user's Recall folder {dir}: inject a temp root (user_dirs::inject) first", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[should_panic(expected = "inside the real user's Recall folder")]
    fn a_path_in_the_real_recall_folders_panics() {
        let appdata = std::env::var_os("APPDATA").map(std::path::PathBuf::from).expect("APPDATA");
        let _ = super::checked(appdata.join("Recall").join("open-clips.json"));
    }

    #[test]
    fn resolvers_land_under_the_injected_root() {
        let root = std::env::temp_dir().join(format!("recall-native-user-dirs-{}", std::process::id()));
        let injected = super::inject(&root);
        assert!(crate::services::library::folders::default_recordings_dir().starts_with(&root));
        drop(injected);
        let shared = std::env::temp_dir().join("recall-native-unit-tests");
        assert!(super::local_appdata().unwrap().starts_with(&shared), "the run's shared root");
    }
}
