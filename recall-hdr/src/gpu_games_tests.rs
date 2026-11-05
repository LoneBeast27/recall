use std::time::{Duration, Instant};

use super::*;

struct Fake;

impl GameKnowledge for Fake {
    fn vendor(&self) -> &'static str {
        "FAKE"
    }
    fn games(&self, exe_paths: &[String]) -> Result<Vec<bool>, String> {
        if exe_paths.iter().any(|p| p.contains("broken")) {
            return Err("driver said no".into());
        }
        Ok(exe_paths.iter().map(|p| p.ends_with("game.exe")).collect())
    }
}

fn fake() -> Result<Box<dyn GameKnowledge>, String> {
    Ok(Box::new(Fake))
}

fn absent() -> Result<Box<dyn GameKnowledge>, String> {
    Err("nvapi64.dll not loaded".into())
}

/// Ask until the worker has answered (test only; the controller never waits).
fn settled(k: &mut KnownGames, path: &str) -> bool {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(v) = k.for_path(path) {
            return v;
        }
        assert!(Instant::now() < end, "no answer for {path}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn first_ask_is_pending_then_the_verdict_is_cached() {
    let mut k = KnownGames::with(fake);
    assert_eq!(k.for_path(r"C:\Games\Some\GAME.exe"), None, "the pump thread never waits");
    assert!(settled(&mut k, r"c:\games\some\game.exe"), "case-insensitive path");
    assert_eq!(k.vendor(), "FAKE");
    assert!(!settled(&mut k, r"C:\Program Files\Chat\chat.exe"));
    assert_eq!(k.asked.len(), 2, "one lookup per path");
    assert_eq!(k.for_path(r"C:\GAMES\SOME\GAME.EXE"), Some(true));
    assert_eq!(k.asked.len(), 2, "a cached path is not asked again");
}

#[test]
fn no_vendor_means_never_a_known_game() {
    let mut k = KnownGames::with(absent);
    assert_eq!(k.for_path(r"C:\x\game.exe"), None);
    assert!(!settled(&mut k, r"C:\x\game.exe"), "falls back to today's rules");
    assert!(k.off);
    assert_eq!(k.for_path(r"C:\y\game.exe"), Some(false), "no further lookups once off");
}

#[test]
fn a_failed_lookup_leaves_the_exe_to_the_render_check() {
    let mut k = KnownGames::with(fake);
    assert!(!settled(&mut k, r"C:\broken\game.exe"));
    assert!(!k.off, "one failure does not turn the vendor off");
    assert!(settled(&mut k, r"C:\fine\game.exe"));
}

#[test]
fn own_process_resolves_by_pid() {
    let mut k = KnownGames::with(fake);
    let me = std::process::id();
    let name = std::env::current_exe().unwrap().file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(k.for_pid(me, &name), None);
    let (_, path) = k.paths.get(&me).cloned().expect("pid resolved to a path");
    assert!(path.ends_with(&name.to_lowercase()), "{path}");
    assert!(!settled(&mut k, &path), "a test binary is no game");
    assert_eq!(k.for_pid(me, &name), Some(false));
}
