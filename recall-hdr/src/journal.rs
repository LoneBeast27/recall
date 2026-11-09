//! Crash-safety state journal (`%APPDATA%\recall-hdr\state.json`): records the
//! pre-mutation display baseline so any exit path -- including a hard `taskkill`
//! -- can restore it. The contract is a strict ordering:
//!
//! 1. Before the *first* mutation of a given display facet, [`Journal::record`]
//!    is called to stamp the original value + set its `*_touched` flag. That
//!    write is durable (atomic tmp + rename, fsynced) *before* the mutating
//!    display call returns to us -- so if we die mid-call the file already
//!    reflects "we were about to touch X, its baseline was Y".
//! 2. On a verified-clean restore, [`Journal::clear`] removes the file.
//! 3. At next launch, [`Journal::is_dirty`] + [`restore_with`] repair a display
//!    left dirty by a hard kill (the in-process guards never got to run).
//!
//! The restore executor is decoupled from the display / clamp drivers (they are
//! built by sibling agents in parallel): the caller supplies a [`RestoreOps`].

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::log;

/// The persisted pre-mutation baseline. Every facet carries a `*_touched` flag
/// plus the original value to restore; a facet with `touched == false` is left
/// exactly as-is on restore (we never learned/changed its baseline).
///
/// The clamp facet has no `*_original` value on purpose: we only ever engage a
/// clamp on top of *no* foreign clamp (see the reconciler's HDR/clamp gate), so
/// restoring the clamp is always "clear it", never "re-apply someone else's".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DisplayBaseline {
    /// We changed Windows HDR; restore it to `hdr_original`.
    pub hdr_touched: bool,
    /// The user's HDR state before we touched it.
    pub hdr_original: bool,
    /// We engaged the gamut clamp; restore = clear it (native gamut).
    pub clamp_touched: bool,
    /// We changed the SDR-content white level; restore it to `sdr_white_original`.
    pub sdr_white_touched: bool,
    /// The user's SDR white level (nits) before we touched it.
    pub sdr_white_original: u32,
}

impl DisplayBaseline {
    /// True if any facet was touched -- i.e. the display owes a restore.
    pub fn any_touched(&self) -> bool {
        self.hdr_touched || self.clamp_touched || self.sdr_white_touched
    }
}

/// Owns the on-disk journal file and an in-memory mirror of the current
/// baseline. Construct with [`Journal::open`]; every [`Journal::record`] mutates
/// the mirror and durably rewrites the file before returning.
pub struct Journal {
    path: PathBuf,
    baseline: DisplayBaseline,
}

impl Journal {
    /// Resolve `%APPDATA%\recall-hdr\state.json`, creating the directory. Falls
    /// back to the OS temp dir if `%APPDATA%` is unset (matches `logging.rs`).
    pub fn path() -> PathBuf {
        let base = crate::user_dirs::appdata().unwrap_or_else(std::env::temp_dir);
        let path = crate::user_dirs::checked(base.join("recall-hdr").join("state.json"));
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        path
    }

    /// Open the journal at the default [`Journal::path`], loading any existing
    /// baseline (a corrupt file is logged, deleted, and treated as empty).
    pub fn open() -> Journal {
        Self::open_at(Self::path())
    }

    /// Open the journal at an explicit path (used by tests against a temp dir).
    pub fn open_at(path: PathBuf) -> Journal {
        let baseline = Self::load_from(&path).unwrap_or_default();
        Journal { path, baseline }
    }

    /// The current in-memory baseline.
    pub fn baseline(&self) -> DisplayBaseline {
        self.baseline
    }

    /// Load the persisted baseline from the default path. `None` if the file is
    /// absent or corrupt. A corrupt file is logged and deleted so a bad journal
    /// never wedges a launch (better to lose the restore than loop on garbage).
    pub fn load() -> Option<DisplayBaseline> {
        Self::load_from(&Self::path())
    }

    /// Load + validate the baseline at `path`. `None` on absent/corrupt; corrupt
    /// files are deleted as a side effect.
    pub fn load_from(path: &PathBuf) -> Option<DisplayBaseline> {
        let bytes = fs::read(path).ok()?;
        match serde_json::from_slice::<DisplayBaseline>(&bytes) {
            Ok(b) => Some(b),
            Err(e) => {
                log!(
                    "journal: corrupt state.json at {} ({e}); deleting",
                    path.display()
                );
                let _ = fs::remove_file(path);
                None
            }
        }
    }

    /// Mutate the baseline via `f`, then durably persist it **before returning**.
    ///
    /// This is the crash-safety hinge: callers invoke `record` to stamp the
    /// original value + set the `*_touched` flag, and only *after* it returns do
    /// they issue the actual display mutation. So the on-disk journal always
    /// leads the live display -- a kill between `record` and the display call
    /// leaves a journal that (harmlessly) claims a facet was touched that in
    /// fact wasn't yet, and restore just re-asserts the recorded baseline.
    ///
    /// The write is atomic (tmp file + fsync + rename) so a crash mid-write can
    /// never truncate the live journal.
    pub fn record(&mut self, f: impl FnOnce(&mut DisplayBaseline)) -> Result<(), String> {
        f(&mut self.baseline);
        Self::write_atomic(&self.path, &self.baseline)
    }

    /// Atomically write `baseline` to `path`: serialize -> tmp sibling -> fsync
    /// -> rename over the target. Rename is atomic on NTFS for same-directory
    /// paths, so a reader/crash sees either the old or the new file, never a
    /// half-written one.
    fn write_atomic(path: &PathBuf, baseline: &DisplayBaseline) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(baseline)
            .map_err(|e| format!("journal: serialize failed: {e}"))?;

        let tmp = path.with_extension("json.tmp");
        {
            let mut f: File = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp)
                .map_err(|e| format!("journal: open tmp {} failed: {e}", tmp.display()))?;
            f.write_all(&json)
                .map_err(|e| format!("journal: write tmp failed: {e}"))?;
            // Durability: the bytes must hit the platter before the rename, or a
            // power loss could rename an empty inode over the good journal.
            f.sync_all()
                .map_err(|e| format!("journal: fsync tmp failed: {e}"))?;
        }

        fs::rename(&tmp, path).map_err(|e| {
            // Best-effort cleanup so a failed rename doesn't litter a stale tmp.
            let _ = fs::remove_file(&tmp);
            format!("journal: rename {} -> {} failed: {e}", tmp.display(), path.display())
        })
    }

    /// The display has been verified clean -> drop the journal entirely and reset
    /// the in-memory mirror. Idempotent: clearing an already-absent journal is Ok.
    pub fn clear(&mut self) -> Result<(), String> {
        self.baseline = DisplayBaseline::default();
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!(
                "journal: clear {} failed: {e}",
                self.path.display()
            )),
        }
    }

    /// Static dirty check for the default path: does a journal exist that claims
    /// any facet was touched? Cheap gate used at launch before spinning up the
    /// restore machinery. A corrupt file is treated as *not* dirty (it will be
    /// deleted by [`Journal::load`]/`load_from`).
    pub fn is_dirty() -> bool {
        Self::load()
            .map(|b| b.any_touched())
            .unwrap_or(false)
    }
}

/// Executor for a restore, decoupled from the concrete display / clamp drivers
/// (built in parallel). The startup-repair and clean-shutdown paths both drive a
/// restore through this trait, supplying whatever real ops they have wired up.
pub trait RestoreOps {
    /// Set Windows HDR on/off for the primary display.
    fn set_hdr(&mut self, on: bool) -> Result<(), String>;
    /// Clear any active gamut clamp (reset CSC to the panel's native gamut).
    fn clear_clamp(&mut self) -> Result<(), String>;
    /// Set the SDR-content white level (nits).
    fn set_sdr_white(&mut self, nits: u32) -> Result<(), String>;
}

/// Load the journal and apply the recorded baseline through `ops`, touching only
/// the facets whose `*_touched` flag is set. On *full* success the journal is
/// cleared (the display is verified clean); if any op errors the journal is left
/// intact so the next launch retries. Returns whether anything was restored.
///
/// Ordering note: HDR is restored last. The NvAPI clamp is rejected while HDR is
/// ON, so we clear the clamp (and set SDR white) *before* potentially turning HDR
/// back on -- otherwise a re-enable could strand a clear that the driver refused.
pub fn restore_with(ops: &mut dyn RestoreOps) -> Result<bool, String> {
    let mut journal = Journal::open();
    let base = journal.baseline;

    if !base.any_touched() {
        return Ok(false);
    }

    log!(
        "journal: restoring baseline (hdr_touched={} clamp_touched={} sdr_white_touched={})",
        base.hdr_touched,
        base.clamp_touched,
        base.sdr_white_touched
    );

    // Clamp first (must be cleared while HDR is still in its current state), then
    // SDR white, then HDR last. Collect the first error but keep going so we
    // don't strand a facet we *could* have restored behind one that failed --
    // then propagate the error (leaving the journal dirty for a retry).
    let mut first_err: Option<String> = None;
    let mut record_err = |r: Result<(), String>| {
        if let Err(e) = r {
            if first_err.is_none() {
                first_err = Some(e);
            }
        }
    };

    if base.clamp_touched {
        record_err(ops.clear_clamp());
    }
    if base.sdr_white_touched {
        record_err(ops.set_sdr_white(base.sdr_white_original));
    }
    if base.hdr_touched {
        record_err(ops.set_hdr(base.hdr_original));
    }

    if let Some(e) = first_err {
        log!("journal: restore incomplete ({e}); leaving journal dirty for retry");
        return Err(e);
    }

    journal.clear()?;
    log!("journal: restore complete; journal cleared");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A unique temp dir + journal path per test (no shared global state, never
    /// the real `%APPDATA%`). The `Journal` is opened via `open_at`.
    struct TempJournal {
        dir: PathBuf,
    }

    impl TempJournal {
        fn new(tag: &str) -> TempJournal {
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "recall-hdr-jtest-{tag}-{}-{n}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).unwrap();
            TempJournal { dir }
        }
        fn path(&self) -> PathBuf {
            self.dir.join("state.json")
        }
        fn journal(&self) -> Journal {
            Journal::open_at(self.path())
        }
    }

    impl Drop for TempJournal {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// A mock RestoreOps that records the calls it received and can be told to
    /// fail a specific facet.
    #[derive(Default)]
    struct MockOps {
        hdr: Vec<bool>,
        clamp_cleared: u32,
        sdr_white: Vec<u32>,
        fail_clamp: bool,
        fail_hdr: bool,
    }
    impl RestoreOps for MockOps {
        fn set_hdr(&mut self, on: bool) -> Result<(), String> {
            self.hdr.push(on);
            if self.fail_hdr {
                Err("mock hdr fail".into())
            } else {
                Ok(())
            }
        }
        fn clear_clamp(&mut self) -> Result<(), String> {
            self.clamp_cleared += 1;
            if self.fail_clamp {
                Err("mock clamp fail".into())
            } else {
                Ok(())
            }
        }
        fn set_sdr_white(&mut self, nits: u32) -> Result<(), String> {
            self.sdr_white.push(nits);
            Ok(())
        }
    }

    #[test]
    fn write_load_roundtrip() {
        let tj = TempJournal::new("roundtrip");
        let mut j = tj.journal();
        j.record(|b| {
            b.hdr_touched = true;
            b.hdr_original = true;
            b.sdr_white_touched = true;
            b.sdr_white_original = 240;
        })
        .unwrap();

        let loaded = Journal::load_from(&tj.path()).expect("should load");
        assert!(loaded.hdr_touched);
        assert!(loaded.hdr_original);
        assert!(loaded.sdr_white_touched);
        assert_eq!(loaded.sdr_white_original, 240);
        assert!(!loaded.clamp_touched);
    }

    #[test]
    fn record_persists_before_returning() {
        // After record() returns, the file must already be on disk and readable
        // -- the whole crash-safety contract. We read it back independently
        // (fresh load, not the in-memory mirror) to prove durability.
        let tj = TempJournal::new("persist");
        let mut j = tj.journal();
        j.record(|b| {
            b.clamp_touched = true;
        })
        .unwrap();

        let on_disk = Journal::load_from(&tj.path()).expect("file present after record");
        assert!(on_disk.clamp_touched, "record() must persist synchronously");
    }

    #[test]
    fn clear_removes_file() {
        let tj = TempJournal::new("clear");
        let mut j = tj.journal();
        j.record(|b| b.hdr_touched = true).unwrap();
        assert!(tj.path().exists());

        j.clear().unwrap();
        assert!(!tj.path().exists(), "clear() must remove the file");
        // Clearing an already-absent journal is idempotent-Ok.
        j.clear().unwrap();
    }

    #[test]
    fn corrupt_file_loads_none_and_is_deleted() {
        let tj = TempJournal::new("corrupt");
        fs::write(tj.path(), b"{ not valid json ]]").unwrap();

        let loaded = Journal::load_from(&tj.path());
        assert!(loaded.is_none(), "corrupt file must load as None");
        assert!(
            !tj.path().exists(),
            "corrupt file must be deleted on load"
        );
    }

    #[test]
    fn restore_applies_only_touched_and_clears_on_success() {
        let tj = TempJournal::new("restore-ok");
        let mut j = tj.journal();
        j.record(|b| {
            b.hdr_touched = true;
            b.hdr_original = false; // restore HDR back to OFF
            b.clamp_touched = true;
            // sdr_white intentionally NOT touched
        })
        .unwrap();
        drop(j);

        let mut ops = MockOps::default();
        // Point restore at the temp path by exercising the same logic the public
        // fn runs, but against our temp journal.
        let restored = restore_via(&tj.path(), &mut ops).unwrap();

        assert!(restored);
        assert_eq!(ops.hdr, vec![false], "only touched HDR restored");
        assert_eq!(ops.clamp_cleared, 1, "touched clamp cleared");
        assert!(ops.sdr_white.is_empty(), "untouched sdr_white left alone");
        assert!(!tj.path().exists(), "journal cleared on full success");
    }

    #[test]
    fn restore_not_cleared_when_an_op_errors() {
        let tj = TempJournal::new("restore-fail");
        let mut j = tj.journal();
        j.record(|b| {
            b.hdr_touched = true;
            b.hdr_original = true;
            b.clamp_touched = true;
        })
        .unwrap();
        drop(j);

        let mut ops = MockOps {
            fail_clamp: true,
            ..Default::default()
        };
        let res = restore_via(&tj.path(), &mut ops);

        assert!(res.is_err(), "an op error must surface as Err");
        // Even though clamp failed, we still attempted HDR (don't strand a
        // restorable facet), but the journal must remain for a retry.
        assert_eq!(ops.hdr, vec![true], "other facets still attempted");
        assert!(
            tj.path().exists(),
            "journal must NOT be cleared when a restore op errored"
        );
    }

    #[test]
    fn is_dirty_helper_via_path() {
        let tj = TempJournal::new("dirty");
        // Empty (untouched) baseline is not dirty.
        let mut j = tj.journal();
        j.record(|_| {}).unwrap();
        assert!(
            !Journal::load_from(&tj.path()).unwrap().any_touched(),
            "no touched flags -> not dirty"
        );

        j.record(|b| b.hdr_touched = true).unwrap();
        assert!(
            Journal::load_from(&tj.path()).unwrap().any_touched(),
            "a touched flag -> dirty"
        );
    }

    /// Test-only mirror of [`restore_with`] that targets an explicit path so we
    /// never touch the real `%APPDATA%`. Kept in lockstep with the public fn.
    fn restore_via(path: &PathBuf, ops: &mut dyn RestoreOps) -> Result<bool, String> {
        let mut journal = Journal::open_at(path.clone());
        let base = journal.baseline;
        if !base.any_touched() {
            return Ok(false);
        }
        let mut first_err: Option<String> = None;
        let mut record_err = |r: Result<(), String>| {
            if let Err(e) = r {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        };
        if base.clamp_touched {
            record_err(ops.clear_clamp());
        }
        if base.sdr_white_touched {
            record_err(ops.set_sdr_white(base.sdr_white_original));
        }
        if base.hdr_touched {
            record_err(ops.set_hdr(base.hdr_original));
        }
        if let Some(e) = first_err {
            return Err(e);
        }
        journal.clear()?;
        Ok(true)
    }
}
