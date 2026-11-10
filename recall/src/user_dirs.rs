//! The user's folders the engine's path resolvers start from (`%APPDATA%`, `%USERPROFILE%`), and
//! the unit-test guard on them.
//!
//! - Unit tests resolve both under a temp root: one injected for the thread ([`inject`]), else the
//!   run's shared `%TEMP%\recall-engine-unit-tests`.
//! - [`checked`]: every resolver passes its result through it. Under `cfg(test)` a path inside the
//!   real user's Recall folders panics (unit tests once wrote the real `open-clips.json`, since
//!   nothing marks a `cargo test` run at run time). Release builds compile it to the identity.
use std::path::PathBuf;

/// `%APPDATA%`.
pub(crate) fn appdata() -> Option<PathBuf> {
    #[cfg(test)]
    return Some(test_root::get().join(r"AppData\Roaming"));
    #[allow(unreachable_code)]
    std::env::var_os("APPDATA").filter(|p| !p.is_empty()).map(PathBuf::from)
}

/// `%LOCALAPPDATA%`.
pub(crate) fn localappdata() -> Option<PathBuf> {
    #[cfg(test)]
    return Some(test_root::get().join(r"AppData\Local"));
    #[allow(unreachable_code)]
    std::env::var_os("LOCALAPPDATA").filter(|p| !p.is_empty()).map(PathBuf::from)
}

/// `%USERPROFILE%` (empty when unset, as before).
pub(crate) fn profile() -> PathBuf {
    #[cfg(test)]
    return test_root::get();
    #[allow(unreachable_code)]
    PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default())
}

/// Every resolver's result passes through here (see the module doc).
#[inline]
pub(crate) fn checked(path: PathBuf) -> PathBuf {
    #[cfg(test)]
    test_root::assert_outside_real(&path);
    path
}

/// Tests: this thread's folders resolve under `root` until the guard drops.
#[cfg(test)]
pub(crate) fn inject(root: &std::path::Path) -> test_root::Injected {
    test_root::ROOT.with(|r| *r.borrow_mut() = Some(root.to_path_buf()));
    test_root::Injected(())
}

#[cfg(test)]
pub(crate) mod test_root {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    thread_local! { pub(super) static ROOT: RefCell<Option<PathBuf>> = const { RefCell::new(None) }; }

    pub(crate) struct Injected(pub(super) ());

    impl Drop for Injected {
        fn drop(&mut self) {
            ROOT.with(|r| *r.borrow_mut() = None);
        }
    }

    /// This thread's injected root, else the run's shared one.
    pub(crate) fn get() -> PathBuf {
        ROOT.with(|r| r.borrow().clone()).unwrap_or_else(|| std::env::temp_dir().join("recall-engine-unit-tests"))
    }

    fn key(path: &Path) -> String {
        path.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase()
    }

    /// The real user's Recall folders, from the environment (no test changes these variables).
    fn real_dirs() -> &'static [String] {
        static DIRS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
        DIRS.get_or_init(|| {
            let env = [("APPDATA", "Recall"), ("APPDATA", "recall-hdr"), ("LOCALAPPDATA", "Recall"), ("LOCALAPPDATA", r"Programs\Recall"), ("USERPROFILE", r"Videos\Recall")];
            env.into_iter().filter_map(|(var, sub)| std::env::var_os(var).filter(|v| !v.is_empty()).map(|v| key(&PathBuf::from(v).join(sub)))).collect()
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
        let _ = super::checked(appdata.join("Recall").join("folders.json"));
    }

    #[test]
    fn resolvers_land_under_the_test_root() {
        let root = std::env::temp_dir().join(format!("recall-engine-user-dirs-{}", std::process::id()));
        let injected = super::inject(&root);
        assert!(std::path::Path::new(&crate::naming::out_dir()).starts_with(&root) || std::env::var_os("RECALL_OUT_DIR").is_some());
        drop(injected);
        let shared = std::env::temp_dir().join("recall-engine-unit-tests");
        assert!(super::appdata().unwrap().starts_with(&shared), "the run's shared root");
    }
}
