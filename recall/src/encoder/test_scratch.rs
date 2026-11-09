//! Tests only: a scratch folder in %TEMP% that is removed when the guard drops, also when the
//! test panics (unwinding runs Drop). Failing runs left `recall-retry-*`, `recall-space-*`,
//! `recall-saver-*` and `recall-staging-*` folders behind (F-storage-2).
use std::path::{Path, PathBuf};

pub(crate) struct Scratch(PathBuf);

/// `%TEMP%\<prefix>-<tag>-<uuid>`, created.
pub(crate) fn scratch(prefix: &str, tag: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!("{prefix}-{tag}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl std::ops::Deref for Scratch {
    type Target = PathBuf;
    fn deref(&self) -> &PathBuf {
        &self.0
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

#[test]
fn a_panicking_test_still_removes_its_scratch_folder() {
    let left = std::panic::catch_unwind(|| {
        let dir = scratch("recall-scratch-guard", "panics");
        std::fs::write(dir.join("clip.mp4"), vec![1u8; 4096]).unwrap();
        std::panic::panic_any(dir.to_path_buf());
    })
    .unwrap_err()
    .downcast::<PathBuf>()
    .unwrap();
    assert!(!left.exists(), "{} was left behind", left.display());
}
