//! The sink's decision and the outcome mapping (pure), plus a live probe that
//! is ignored by default (run it on purpose; see `live_probe`).

use super::*;
use std::path::Path;

const S_OK_FLAGS: u32 = 0;

#[test]
fn the_sink_lets_a_delete_through_only_when_it_recycles() {
    assert!(allows_delete(TSF_DELETE_RECYCLE_IF_POSSIBLE));
    assert!(allows_delete(TSF_DELETE_RECYCLE_IF_POSSIBLE | 0x2), "other bits do not matter");
    assert!(!allows_delete(S_OK_FLAGS), "no recycle bit: a permanent delete, refused");
    assert!(!allows_delete(0x1 | 0x40));
}

#[test]
fn the_operation_flags_recycle_and_never_show_ui() {
    for flag in [FOFX_RECYCLEONDELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_SILENT, FOF_NOERRORUI] {
        assert_ne!(OP_FLAGS & flag.0, 0, "{:#x} missing", flag.0);
    }
    assert_eq!(OP_FLAGS, 0x80000 | 0x40 | 0x10 | 0x4 | 0x400, "nothing else (no FOF_WANTNUKEWARNING: the sink decides)");
}

#[test]
fn outcomes_from_the_sink_and_the_operation() {
    assert_eq!(classify(false, Ok(()), false), Outcome::Recycled);
    assert!(matches!(classify(true, Err(E_ABORT.0 as u32), true), Outcome::Refused(_)), "the sink refused");
    assert!(matches!(classify(false, Err(0x8007_04C7), false), Outcome::Refused(_)), "ERROR_CANCELLED");
    assert!(matches!(classify(false, Err(0x8027_0000), false), Outcome::Refused(_)), "copy engine cancelled");
    assert!(matches!(classify(false, Ok(()), true), Outcome::Refused(_)), "aborted");
    assert!(matches!(classify(false, Err(0x8007_0005), false), Outcome::Failed(_)), "access denied is a failure");
}

// ---- Live probe (ignored by default; sandbox files under this checkout's .tmp only) ----

fn sandbox() -> PathBuf {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".tmp").join(format!("fileop-probe-{stamp}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).map(|d| crate::win_volume::plain(&d)).unwrap()
}

/// The `$I` / `$R` pair in this volume's `$Recycle.Bin` whose `$I` names `original`.
fn bin_entry(original: &Path) -> Option<(PathBuf, PathBuf)> {
    let root = original.components().next()?.as_os_str().to_string_lossy().into_owned();
    let bin = PathBuf::from(format!(r"{root}\$Recycle.Bin"));
    let want: Vec<u16> = original.as_os_str().encode_wide().collect();
    for sid in std::fs::read_dir(&bin).ok()?.flatten() {
        let Ok(items) = std::fs::read_dir(sid.path()) else { continue };
        for item in items.flatten() {
            let name = item.file_name().to_string_lossy().into_owned();
            let Some(suffix) = name.strip_prefix("$I") else { continue };
            let Ok(bytes) = std::fs::read(item.path()) else { continue };
            // Version 2 layout: 8 version, 8 size, 8 time, 4 length (chars), then the UTF-16 path.
            if bytes.len() < 28 {
                continue;
            }
            let path: Vec<u16> = bytes[28..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|c| *c != 0).collect();
            if String::from_utf16_lossy(&path).eq_ignore_ascii_case(&String::from_utf16_lossy(&want)) {
                return Some((item.path(), sid.path().join(format!("$R{suffix}"))));
            }
        }
    }
    None
}

fn unused_letter() -> Option<char> {
    ('P'..='Z').rev().find(|l| !Path::new(&format!("{l}:\\")).exists())
}

fn hidden(cmd: &str, args: &[&str]) -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new(cmd)
        .args(args)
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Run on purpose: `cargo test -p recall-hdr --offline fileop_live_probe -- --ignored --nocapture`.
/// Recycles one small sandbox file (then removes its `$I`/`$R` from the bin,
/// that entry only) and tries a would-nuke path (`subst`, and an admin share
/// if reachable), expecting a refusal with the file still there.
#[test]
#[ignore]
fn fileop_live_probe() {
    let dir = sandbox();
    let mut letter_made = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // 1. Recycled case: a local fixed volume.
        let file = dir.join("recycle-me.txt");
        std::fs::write(&file, b"recall probe").unwrap();
        let report = recycle_files(std::slice::from_ref(&file));
        println!("PROBE recycled-case: {report:?}; file exists after: {}", file.exists());
        let entry = bin_entry(&file);
        println!("PROBE recycled-case bin entry: {entry:?}");
        if let Some((info, data)) = &entry {
            let removed = std::fs::remove_file(data).is_ok() & std::fs::remove_file(info).is_ok();
            println!("PROBE recycled-case bin entry removed: {removed}; $R gone: {}, $I gone: {}", !data.exists(), !info.exists());
        }
        assert_eq!(report.outcome, Outcome::Recycled);
        assert!(!file.exists() && entry.is_some());

        // 2. Would-nuke case A: a subst drive onto the sandbox.
        if let Some(l) = unused_letter() {
            let drive = format!("{l}:");
            if hidden("subst", &[&drive, &dir.to_string_lossy()]) {
                letter_made = Some(drive.clone());
                let file = PathBuf::from(format!(r"{drive}\nuke-me.txt"));
                std::fs::write(&file, b"recall probe").unwrap();
                let report = recycle_files(std::slice::from_ref(&file));
                let real = dir.join("nuke-me.txt");
                println!("PROBE subst-case {drive}: {report:?}; file exists after: {} (real path {})", real.exists(), real.display());
                println!("PROBE subst-case precheck: {:?}", crate::win_recycle::can_recycle(Path::new(&format!(r"{drive}\")), 12));
                if let Some((info, data)) = bin_entry(&file).or_else(|| bin_entry(&real)) {
                    // Windows recycled it after all: take our one entry back out.
                    let _ = (std::fs::remove_file(&data), std::fs::remove_file(&info));
                    println!("PROBE subst-case was recycled; bin entry removed: {}", !data.exists() && !info.exists());
                }
            }
        }

        // 3. Would-nuke case B: an admin share, only if reachable unelevated.
        let text = dir.to_string_lossy().into_owned();
        let share = PathBuf::from(format!(r"\\localhost\{}${}", &text[..1], &text[2..]));
        if share.is_dir() {
            let file = share.join("share-me.txt");
            if std::fs::write(&file, b"recall probe").is_ok() {
                let report = recycle_files(std::slice::from_ref(&file));
                println!("PROBE share-case {}: {report:?}; file exists after: {}", file.display(), dir.join("share-me.txt").exists());
            }
        } else {
            println!("PROBE share-case: {} not reachable unelevated; skipped", share.display());
        }
    }));
    if let Some(drive) = letter_made {
        let gone = hidden("subst", &[&drive, "/d"]);
        println!("PROBE cleanup: subst {drive} /d ok: {gone}; {drive} exists after: {}", Path::new(&format!(r"{drive}\")).exists());
    }
    let _ = std::fs::remove_dir_all(&dir);
    println!("PROBE cleanup: sandbox {} exists after: {}", dir.display(), dir.exists());
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
