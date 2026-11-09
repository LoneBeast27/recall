//! A Setup swap that was cut off (`.swap-journal` beside the program, written by
//! `scripts/setup/SetupSwapJournal.cs` before every move): before Recall starts, the folder is
//! put back to all old exactly as Setup's `InstallSwap.Recover` does, so a mismatched program
//! pair never runs. When that cannot finish (a file still held), Recall refuses to start and
//! says how to get out of it.
//!
//! Lines: `m <name>` (the installed file is about to move into `.old\`), `n1 <name>` (its new
//! copy is about to move in), `n0 <name>` (a new file where none was). A file comes back from
//! `.old\` only when the journal says it was moved; a stale `.old\` file is never used.
use std::path::{Path, PathBuf};

pub const JOURNAL: &str = ".swap-journal";
const HEADER: &str = "recall-swap-journal 1";
const LOCK: &str = ".install-swap-lock";

#[derive(Debug, Clone, Default, PartialEq)]
struct Entry {
    name: String,
    moved: bool,
    placed: bool,
    had: bool,
}

/// The entries in first-written order; None when the text is not a journal.
fn parse(text: &str) -> Option<Vec<Entry>> {
    let mut lines = text.split('\n');
    if lines.next()?.trim() != HEADER {
        return None;
    }
    let mut out: Vec<Entry> = Vec::new();
    for line in lines.map(|l| l.trim_end_matches('\r')) {
        let Some((kind, name)) = line.split_once(' ') else { continue };
        if name.is_empty() || name.contains(['\\', '/', ':']) {
            continue;
        }
        let at = match out.iter().position(|e| e.name.eq_ignore_ascii_case(name)) {
            Some(i) => i,
            None => {
                out.push(Entry { name: name.into(), ..Entry::default() });
                out.len() - 1
            }
        };
        let e = &mut out[at];
        match kind {
            "m" => (e.moved, e.had) = (true, true),
            "n1" => (e.placed, e.had) = (true, true),
            "n0" => e.placed = true,
            _ => {}
        }
    }
    Some(out)
}

/// Every entry back to its old file, newest first, going on past a failing step.
fn roll_back(dir: &Path, entries: &[Entry]) -> Vec<String> {
    let old = dir.join(".old");
    let mut failures = Vec::new();
    for e in entries.iter().rev() {
        let (target, backup) = (dir.join(&e.name), old.join(&e.name));
        let step = if e.had {
            if e.moved && backup.exists() && (e.placed || !target.exists()) { replace(&backup, &target) } else { Ok(()) }
        } else if e.placed && target.exists() {
            std::fs::remove_file(&target).map_err(|err| err.to_string())
        } else {
            Ok(())
        };
        if let Err(err) = step {
            failures.push(format!("{}: {err}", e.name));
        }
    }
    failures
}

fn replace(from: &Path, to: &Path) -> Result<(), String> {
    use windows::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW};
    use windows::core::HSTRING;
    // SAFETY: two owned wide strings for the call.
    unsafe { MoveFileExW(&HSTRING::from(from.as_os_str()), &HSTRING::from(to.as_os_str()), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) }
        .map_err(|e| e.message().to_string())
}

/// Puts `dir` back to all old when a journal is there. Ok(true) = it was recovered now,
/// Ok(false) = nothing to do; Err = it could not finish (the journal stays).
pub fn recover(dir: &Path) -> Result<bool, String> {
    let journal = dir.join(JOURNAL);
    let Ok(text) = std::fs::read_to_string(&journal) else { return Ok(false) };
    // The same lock Setup holds for its swap: a running swap is never undone under it.
    let lock = std::fs::OpenOptions::new().write(true).create_new(true).open(dir.join(LOCK));
    let Ok(lock) = lock else { return Err("Recall Setup is replacing files right now".into()) };
    let result = parse(&text).ok_or_else(|| "the swap journal is unreadable".to_string()).and_then(|entries| {
        let failures = roll_back(dir, &entries);
        if failures.is_empty() {
            std::fs::remove_file(&journal).map_err(|e| e.to_string())
        } else {
            Err(failures.join("; "))
        }
    });
    drop(lock);
    let _ = std::fs::remove_file(dir.join(LOCK));
    result.map(|()| true)
}

/// The folder that holds the running program.
pub fn program_dir() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for n in ["a.exe", "b.dll", "c.exe", "e.exe"] {
            std::fs::write(d.path().join(n), format!("old {n}")).unwrap();
        }
        d
    }

    fn state(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_file())
            .map(|e| format!("{}={}", e.file_name().to_string_lossy(), std::fs::read_to_string(e.path()).unwrap()))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn a_cut_off_swap_is_put_back_all_old_and_a_stale_old_file_is_never_used() {
        let d = folder();
        let (dir, old) = (d.path(), d.path().join(".old"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::rename(dir.join("b.dll"), old.join("b.dll")).unwrap();
        std::fs::write(dir.join("b.dll"), "new b.dll").unwrap();
        std::fs::rename(dir.join("c.exe"), old.join("c.exe")).unwrap();
        std::fs::write(dir.join("d.dll"), "new d.dll").unwrap();
        std::fs::write(old.join("e.exe"), "STALE").unwrap();
        std::fs::write(dir.join(JOURNAL), format!("{HEADER}\nm b.dll\nn1 b.dll\nn0 d.dll\nm c.exe\nn1 c.")).unwrap();
        let before = state(dir);
        assert_eq!(recover(dir), Ok(true));
        assert_eq!(state(dir), vec!["a.exe=old a.exe", "b.dll=old b.dll", "c.exe=old c.exe", "e.exe=old e.exe"], "from {before:?}");
        assert_eq!(recover(dir), Ok(false), "done: nothing left to do");
        assert!(!dir.join(LOCK).exists());
    }

    #[test]
    fn a_held_file_keeps_the_journal_and_refuses() {
        let d = folder();
        let (dir, old) = (d.path(), d.path().join(".old"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::rename(dir.join("a.exe"), old.join("a.exe")).unwrap();
        std::fs::write(dir.join("a.exe"), "new a.exe").unwrap();
        std::fs::write(dir.join(JOURNAL), format!("{HEADER}\nm a.exe\nn1 a.exe\n")).unwrap();
        {
            use std::os::windows::fs::OpenOptionsExt;
            let _held = std::fs::OpenOptions::new().read(true).share_mode(0).open(old.join("a.exe")).unwrap();
            assert!(recover(dir).is_err(), "cannot finish while a file is held");
            assert!(dir.join(JOURNAL).exists(), "the journal stays");
        }
        assert_eq!(recover(dir), Ok(true));
        assert_eq!(std::fs::read_to_string(dir.join("a.exe")).unwrap(), "old a.exe");
        std::fs::write(dir.join(JOURNAL), "garbage").unwrap();
        assert!(recover(dir).is_err(), "an unreadable journal refuses");
    }
}
