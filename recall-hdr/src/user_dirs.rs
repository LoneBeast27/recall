//! The user's known folders (Shell known-folder API, not environment variables). Unit tests
//! inject a temp root per thread ([`inject`]); the known folders then resolve under it.
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Known {
    LocalAppData,
}

/// The folder now, or `None` when Windows cannot name it.
pub(crate) fn known(which: Known) -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(root) = test_root::get() {
        return Some(root.join(test_root::name(which)));
    }
    os_known(which)
}

fn os_known(which: Known) -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    let id = match which {
        Known::LocalAppData => &FOLDERID_LocalAppData,
    };
    // SAFETY: the returned buffer is copied, then freed with the allocator that made it.
    unsafe {
        let raw = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let path = raw.to_string().ok();
        CoTaskMemFree(Some(raw.0 as *const _));
        path.filter(|p| !p.is_empty()).map(PathBuf::from)
    }
}

/// Tests: this thread's known folders resolve under `root` until the guard drops.
#[cfg(test)]
pub(crate) fn inject(root: &std::path::Path) -> test_root::Injected {
    test_root::set(Some(root.to_path_buf()));
    test_root::Injected(())
}

#[cfg(test)]
pub(crate) mod test_root {
    use super::Known;
    use std::cell::RefCell;
    use std::path::PathBuf;

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

    pub(crate) fn get() -> Option<PathBuf> {
        ROOT.with(|r| r.borrow().clone())
    }

    pub(super) fn name(which: Known) -> &'static str {
        match which {
            Known::LocalAppData => r"AppData\Local",
        }
    }
}
