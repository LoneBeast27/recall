//! App process-tree lookup with injected process lists (no WASAPI).

use super::*;

fn p(pid: u32, parent: u32, exe: &str) -> ProcEntry {
    ProcEntry {
        pid,
        parent,
        exe: exe.to_string(),
    }
}

/// explorer -> chat (200) -> renderer/gpu/audio children, plus a lone
/// second chat.exe, an unrelated app and the game.
fn desktop() -> Vec<ProcEntry> {
    vec![
        p(4, 0, "System"),
        p(100, 4, "explorer.exe"),
        p(200, 100, "chat.exe"),
        p(210, 200, "chat.exe"),
        p(220, 200, "chat.exe"),
        p(230, 210, "chat.exe"),
        p(400, 100, "chat.exe"),
        p(500, 100, "notepad.exe"),
        p(600, 100, "VALORANT-Win64-Shipping.exe"),
    ]
}

#[test]
fn tree_root_picks_the_largest_tree_case_insensitively() {
    let procs = desktop();
    for exe in ["chat.exe", "chat", "CHAT.EXE", r"C:\Apps\chat\chat.exe"] {
        assert_eq!(tree_root(&procs, exe), Some(200), "{exe}");
    }
    assert_eq!(tree_root(&procs, "notepad"), Some(500));
    assert_eq!(tree_root(&procs, "mail.exe"), None);
}

#[test]
fn tree_root_breaks_ties_by_lowest_pid_and_survives_parent_cycles() {
    let lone = vec![p(900, 1, "app.exe"), p(700, 1, "app.exe")];
    assert_eq!(tree_root(&lone, "app.exe"), Some(700));
    // Stale parent IDs after PID reuse can point at each other.
    let cycle = vec![p(510, 500, "app.exe"), p(500, 510, "app.exe")];
    assert_eq!(tree_root(&cycle, "app.exe"), Some(500));
    let own_parent = vec![p(42, 42, "app.exe")];
    assert_eq!(tree_root(&own_parent, "app.exe"), Some(42));
}

/// Read-only: the hand-declared PROCESSENTRY32W layout must match Windows,
/// or Process32FirstW rejects it (ERROR_BAD_LENGTH) and nothing is listed.
#[test]
fn live_snapshot_lists_this_test_process_with_its_parent() {
    let procs = snapshot_processes().expect("toolhelp snapshot");
    let me = procs
        .iter()
        .find(|p| p.pid == std::process::id())
        .expect("current process missing from the snapshot");
    assert!(me.exe.to_lowercase().ends_with(".exe"), "{me:?}");
    assert!(me.parent != 0, "{me:?}");
    assert!(procs.len() > 10, "only {} processes listed", procs.len());
}
