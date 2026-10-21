//! Voice-app PID resolution with injected env + process lists (no WASAPI).

use super::*;
use std::collections::HashMap;

fn p(pid: u32, parent: u32, exe: &str) -> ProcEntry {
    ProcEntry {
        pid,
        parent,
        exe: exe.to_string(),
    }
}

/// explorer -> Discord (200) -> renderer/gpu/audio children, plus a lone
/// second Discord.exe, an unrelated app and the game.
fn desktop() -> Vec<ProcEntry> {
    vec![
        p(4, 0, "System"),
        p(100, 4, "explorer.exe"),
        p(200, 100, "Discord.exe"),
        p(210, 200, "Discord.exe"),
        p(220, 200, "Discord.exe"),
        p(230, 210, "Discord.exe"),
        p(400, 100, "Discord.exe"),
        p(500, 100, "notepad.exe"),
        p(600, 100, "VALORANT-Win64-Shipping.exe"),
    ]
}

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    move |key| map.get(key).cloned()
}

#[test]
fn tree_root_picks_the_largest_tree_case_insensitively() {
    let procs = desktop();
    for exe in ["Discord.exe", "discord", "DISCORD.EXE", r"C:\Apps\Discord\Discord.exe"] {
        assert_eq!(tree_root(&procs, exe), Some(200), "{exe}");
    }
    assert_eq!(tree_root(&procs, "notepad"), Some(500));
    assert_eq!(tree_root(&procs, "Slack.exe"), None);
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

#[test]
fn exe_env_resolves_the_running_app_tree() {
    let got = resolve(env(&[(EXE_ENV, "Discord")]), || Ok(desktop()), Some(600)).unwrap();
    assert_eq!(
        got,
        Resolved::Target(LoopbackTarget {
            pid: 200,
            label: "Discord.exe pid 200 (include tree)".into(),
        })
    );
}

#[test]
fn pid_env_wins_and_never_lists_processes() {
    let got = resolve(
        env(&[(PID_ENV, " 4242 "), (EXE_ENV, "Discord.exe")]),
        || panic!("an explicit PID must not need a process listing"),
        None,
    )
    .unwrap();
    assert_eq!(
        got,
        Resolved::Target(LoopbackTarget {
            pid: 4242,
            label: "process 4242 (include tree)".into(),
        })
    );
}

#[test]
fn missing_app_or_no_choice_means_no_track_without_error() {
    let not_running = resolve(env(&[(EXE_ENV, "Discord.exe")]), || Ok(vec![]), None).unwrap();
    assert_eq!(not_running, Resolved::Off("Discord.exe is not running".into()));
    let unset = resolve(env(&[(EXE_ENV, "  ")]), || panic!("not listed"), None).unwrap();
    assert!(matches!(unset, Resolved::Off(ref r) if r.starts_with("no voice app chosen")));
}

#[test]
fn voice_app_equal_to_the_game_is_skipped_and_bad_pids_are_errors() {
    let same = resolve(env(&[(PID_ENV, "600")]), || Ok(desktop()), Some(600)).unwrap();
    assert!(matches!(same, Resolved::Off(ref r) if r.contains("is the game process")));
    let zero = resolve(env(&[(PID_ENV, "0")]), || Ok(vec![]), None).unwrap_err();
    assert_eq!(zero, "RECALL_AUDIO_VOICE_PID must be a positive process ID, got 0");
    assert!(resolve(env(&[(PID_ENV, "discord")]), || Ok(vec![]), None).is_err());
    let list_failed = resolve(env(&[(EXE_ENV, "Discord")]), || Err("denied".into()), None);
    assert_eq!(list_failed.unwrap_err(), "denied");
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
