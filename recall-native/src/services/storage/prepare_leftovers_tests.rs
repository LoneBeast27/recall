use std::cell::OnceCell;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use super::*;
use crate::test_temp::TempDir;

const KEY: &str = "0123456789abcdef";

fn folder(dir: &Path, files: &[&str]) {
    std::fs::create_dir_all(dir).unwrap();
    for file in files {
        std::fs::write(dir.join(file), vec![7u8; 1024]).unwrap();
    }
}

#[test]
fn only_the_engine_work_folder_name_has_a_pid() {
    assert_eq!(partial_pid(".0123456789abcdef.4120-0"), Some(4120));
    assert_eq!(partial_pid(".0123456789abcdef.4120-31"), Some(4120));
    for other in ["0123456789abcdef", ".0123456789ABCDEF.4120-0", ".0123456789abcde.4120-0", ".0123456789abcdef.4120", ".0123456789abcdef.x-0", ".0123456789abcdef.4120-", ".0123456789abcdef.-0", ".0123456789abcdef.4120-0.flac", "..4120-0", ".0123456789abcdef.99999999999-0", ".clip.recall-export-4120-1-0.mp4"] {
        assert_eq!(partial_pid(other), None, "{other}");
    }
}

/// F-editor-export-1: switching clips during "Preparing sound…" kills the engine before its own
/// cleanup; its partial stems stayed in the cache for good. A stand-in engine makes its work
/// folder under its own pid, like `editor_media.rs`, and hangs until it is stopped.
#[test]
fn a_cut_off_prepare_leaves_no_work_folder() {
    let sandbox = TempDir::sandbox("prep-cut-off");
    let editor = sandbox.join("editor");
    folder(&editor.join(KEY), &["clip.json", "audio-0.flac"]);
    let other = editor.join(format!(".{KEY}.4000000001-0"));
    folder(&other, &["audio-0.flac"]);
    let script = format!("$d = Join-Path '{}' ('.{KEY}.' + $PID + '-0'); New-Item -ItemType Directory $d | Out-Null; Set-Content (Join-Path $d 'audio-0.flac') 'partial'; Start-Sleep 60", editor.display());
    let mut command = Command::new("powershell.exe");
    command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    let seen = OnceCell::new();
    let started = Instant::now();
    let stop = || {
        if seen.get().is_none() {
            let listing = std::fs::read_dir(&editor).into_iter().flatten().flatten().map(|e| e.path());
            if let Some(mine) = listing.filter(|p| p != &other && partial_pid(&p.file_name().unwrap().to_string_lossy()).is_some()).find(|p| p.join("audio-0.flac").is_file()) {
                let _ = seen.set(mine);
            }
        }
        seen.get().is_some() || started.elapsed() > Duration::from_secs(30)
    };
    let error = run_prepare(&mut command, &editor, false, Duration::from_secs(60), &stop).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    let work = seen.get().cloned().expect("the stand-in engine made its work folder");
    assert!(!work.exists(), "the killed engine's partial folder is removed: {}", work.display());
    assert!(editor.join(KEY).join("clip.json").is_file(), "a finished entry stays");
    assert!(other.join("audio-0.flac").is_file(), "another engine's folder is not this kill's to remove");
}

const LATER: Duration = Duration::from_secs(3600);

/// 101 gone, 102 a running engine, 103 can't be told, 104 reused by a program started after the
/// folder, 105 a program that started before it, 106 gone (a junction), 107 gone (holds a foreign file).
fn owner(pid: u32) -> ProcessState {
    let image = |name: &str| format!(r"C:\Programs\{name}");
    match pid {
        102 => ProcessState::Running { started: SystemTime::now(), image: image("Recall.exe") },
        103 => ProcessState::Unknown,
        104 => ProcessState::Running { started: SystemTime::now() + LATER, image: image("notepad.exe") },
        105 => ProcessState::Running { started: SystemTime::UNIX_EPOCH, image: image("notepad.exe") },
        _ => ProcessState::Gone,
    }
}

#[test]
fn the_start_up_sweep_removes_only_dead_engines_partial_folders() {
    let sandbox = TempDir::sandbox("prep-sweep");
    let root = sandbox.join("cache");
    let editor = root.join("editor");
    let partial = |pid: u32| editor.join(format!(".{KEY}.{pid}-0"));
    for pid in [101, 102, 103, 104, 105, 107] {
        folder(&partial(pid), &["audio-0.flac", "audio-1.flac"]);
    }
    std::fs::write(partial(107).join("notes.txt"), b"not the engine's").unwrap();
    folder(&editor.join(KEY), &["clip.json", "audio-0.flac"]);
    folder(&editor.join(format!(".{}.108-0", KEY.to_uppercase())), &["audio-0.flac"]);
    std::fs::write(editor.join(format!(".{KEY}.101-1")), b"a file, not a folder").unwrap();
    let outside = sandbox.join("outside");
    folder(&outside, &["audio-0.flac"]);
    let linked = Command::new("cmd").args(["/c", "mklink", "/J"]).arg(partial(106)).arg(&outside).output().unwrap();
    assert!(linked.status.success() && partial(106).is_dir(), "junction for the test");

    let freed = sweep_dead_partials(&root, owner);

    assert_eq!(freed, 3 * 2048, "101, 104 and 107's stems");
    assert!(!partial(101).exists(), "its engine is gone");
    assert!(!partial(104).exists(), "its pid now runs a program started after the folder");
    for pid in [102, 103, 105] {
        assert!(partial(pid).join("audio-0.flac").is_file(), "{pid} stays");
    }
    assert!(partial(107).join("notes.txt").is_file() && !partial(107).join("audio-0.flac").exists(), "only engine files go");
    assert!(outside.join("audio-0.flac").is_file(), "a junction is never followed");
    assert!(editor.join(KEY).join("clip.json").is_file(), "a finished entry stays");
    assert!(editor.join(format!(".{}.108-0", KEY.to_uppercase())).is_dir(), "another name stays");
    assert!(editor.join(format!(".{KEY}.101-1")).is_file(), "a file stays");
    let _ = std::fs::remove_dir(partial(106));
}

/// The report's check: a dead engine's folder no longer holds the budget once swept, through
/// the live budget pass (`budget::enforce` -> `sweep_old_stems_once`).
#[test]
fn a_dead_engines_folder_no_longer_holds_the_budget() {
    let sandbox = TempDir::sandbox("prep-budget");
    let root = sandbox.join("cache");
    let work = root.join("editor").join(format!(".{KEY}.4000000001-0"));
    folder(&work, &["audio-0.flac"]);
    std::fs::write(work.join("audio-1.flac"), vec![0u8; 1 << 20]).unwrap();
    assert!(super::super::budget::inventory(&root, &|_| 0.0).other > 1 << 20);
    super::super::budget::enforce(&root, 1 << 40, 0);
    assert!(!work.exists(), "a dead engine's work folder is removed");
    // What is left in `other` is the budget pass's own index files (a few bytes).
    assert!(super::super::budget::inventory(&root, &|_| 0.0).other < 1024, "and no longer holds the budget");
}
