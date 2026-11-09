use super::*;
use std::fs;
use std::time::Instant;

/// A unique scratch dir under the OS temp dir.
fn temp_dir(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "recall-hdr-suptest-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    fs::create_dir_all(&p).unwrap();
    p
}

fn touch(path: &Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, b"").unwrap();
}

fn wait_for(deadline_secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(deadline_secs);
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    f()
}

// --- exe-location fallback order --------------------------------------

#[test]
fn locate_prefers_sibling_of_current_exe() {
    let root = temp_dir("sibling");
    let install = root.join("install");
    let our_exe = install.join("recall-hdr.exe");
    let engine = install.join("recall.exe");
    touch(&our_exe);
    touch(&engine);
    touch(&root.join("target").join("debug").join("recall.exe"));
    let found = locate_engine_from(&our_exe).expect("engine located");
    assert_eq!(found, engine, "sibling install dir must win over target/");
}

#[test]
fn locate_dev_fallback_prefers_debug_over_release() {
    let root = temp_dir("devfallback");
    let our_exe = root.join("target").join("debug").join("recall-hdr.exe");
    touch(&our_exe);
    let debug_engine = root.join("target").join("debug").join("recall.exe");
    let release_engine = root.join("target").join("release").join("recall.exe");
    touch(&debug_engine);
    touch(&release_engine);
    let found = locate_engine_from(&our_exe).expect("engine located");
    assert_eq!(found, debug_engine, "debug engine (sibling) found first");
}

#[test]
fn locate_dev_fallback_release_when_no_debug() {
    let root = temp_dir("releaseonly");
    let our_exe = root.join("target").join("debug").join("deps").join("recall-hdr.exe");
    touch(&our_exe);
    let release_engine = root.join("target").join("release").join("recall.exe");
    touch(&release_engine);
    let found = locate_engine_from(&our_exe).expect("engine located via fallback");
    assert_eq!(found, release_engine, "falls back to release when no debug");
}

#[test]
fn locate_returns_none_when_absent() {
    let root = temp_dir("absent");
    let our_exe = root.join("bin").join("recall-hdr.exe");
    touch(&our_exe);
    assert!(locate_engine_from(&our_exe).is_none());
}

// --- missing engine gives up without panicking ------------------------

#[test]
fn missing_engine_gives_up_cleanly() {
    let sup = Supervisor::start_with_locator(|| None, Some(EngineSpec::default()));
    assert!(wait_for(2, || sup.health() == EngineHealth::GivenUp), "missing engine -> GivenUp");
    sup.shutdown(Duration::from_millis(200));
}

// --- spawn environment --------------------------------------------------

#[test]
fn engine_command_sets_and_removes_spec_env() {
    let spec = EngineSpec {
        env: vec![
            ("RECALL_CAPTURE_NAME".into(), Some("VALORANT".into())),
            ("RECALL_CAPTURE_HWND".into(), None),
        ],
    };
    let cmd = engine_command(Path::new("recall.exe"), &spec, None);
    let envs: Vec<_> = cmd.get_envs().map(|(k, v)| (k.to_owned(), v.map(|v| v.to_owned()))).collect();
    assert!(envs.contains(&("RECALL_CAPTURE_NAME".into(), Some("VALORANT".into()))));
    assert!(envs.contains(&("RECALL_CAPTURE_HWND".into(), None)), "None removes an inherited value");
    assert_eq!(cmd.get_args().collect::<Vec<_>>(), vec!["--json"]);
}

/// 2026-11-07 audit R2-deep-audio-1: the mic was muted after the engine started, then the engine
/// crashed. The respawn reuses the spec built at the last retarget (`RECALL_MIC_MUTED=0`), so it
/// must take the mute the controller set since, not the spec's.
#[test]
fn a_crash_respawn_starts_with_the_current_mute() {
    let sup = Supervisor::start_with_locator(|| None, None);
    let spec = EngineSpec { env: vec![("RECALL_MIC_MUTED".into(), Some("0".into()))] };
    let mute_env = |cmd: &Command| -> Option<String> {
        cmd.get_envs().find(|(k, _)| *k == "RECALL_MIC_MUTED").and_then(|(_, v)| v).map(|v| v.to_string_lossy().into_owned())
    };
    // The spawn the manager runs (`supervisor_manager.rs` `spawn_engine`).
    let spawn = |shared: &Shared| engine_command(Path::new("recall.exe"), &spec, shared.spawn_mute.get());
    assert_eq!(mute_env(&spawn(&sup.shared)).as_deref(), Some("0"), "no mute change yet: the spec's");
    sup.set_mic_muted(true);
    assert_eq!(mute_env(&spawn(&sup.shared)).as_deref(), Some("1"), "muted since the spawn: the respawn starts muted");
    sup.set_mic_muted(false);
    assert_eq!(mute_env(&spawn(&sup.shared)).as_deref(), Some("0"));
    sup.shutdown(Duration::from_millis(200));
}

/// Final audit F-audio-1: a line-in flipped to "Alt+M mutes it" (sent live, the spec untouched),
/// then the engine crashed. The respawn reads the spec the manager gets (`shared.desired()`,
/// `supervisor_manager.rs`): it must start with the live list, not the retarget's.
#[test]
fn a_crash_respawn_starts_with_the_live_audio_sources() {
    use crate::record_audio_sources::SOURCES_ENV;
    let sup = Supervisor::start_with_locator(|| None, None);
    let old = r#"[{"id":"line","kind":"input","device":"Line (Box)","device_id":"{A}","mic_mute":false}]"#;
    let live = r#"[{"id":"line","kind":"input","device":"Line (Box)","device_id":"{B}","mic_mute":true}]"#;
    sup.shared.set_desired(Some(EngineSpec { env: vec![(SOURCES_ENV.into(), Some(old.into()))] }), crate::record_dispatch::Sent::Waiting);
    let env = |shared: &Shared| -> Option<String> {
        let (_, spec) = shared.desired();
        let cmd = engine_command(Path::new("recall.exe"), &spec.expect("a spec"), shared.spawn_mute.get());
        cmd.get_envs().find(|(k, _)| *k == SOURCES_ENV).and_then(|(_, v)| v).map(|v| v.to_string_lossy().into_owned())
    };
    assert_eq!(env(&sup.shared).as_deref(), Some(old), "no live change yet: the spec's");
    sup.update_audio(Some(live), &[]); // what apply_config_effects calls on the change
    assert_eq!(env(&sup.shared).as_deref(), Some(live), "the respawn starts with the live flag and the picked twin");
    sup.shutdown(Duration::from_millis(200));
}

/// Review R2-audio-r9-1: a source change between the manager's spec read and the spawn's pid
/// reached neither the env nor the engine (no engine ran to send it to). The manager's check
/// right after the pid (`supervisor_manager.rs`) retargets once; no change, no respawn.
#[test]
fn a_source_change_during_a_spawn_respawns_the_engine_on_it() {
    use crate::record_audio_sources::SOURCES_ENV;
    let shared = Shared::new(None);
    const A: &str = r#"[{"id":"mic","kind":"input","name":"Mic","device":"Mic (Desk)"}]"#;
    const A_FLAG: &str = r#"[{"id":"mic","kind":"input","name":"Mic","device":"Mic (Desk)","mic_mute":false}]"#;
    const B: &str = r#"[{"id":"mic","kind":"input","name":"Mic","device":"Mic (Other)"}]"#;
    let spec = |v: &str| Some(EngineSpec { env: vec![(SOURCES_ENV.into(), Some(v.into()))] });
    shared.set_desired(spec(A), crate::record_dispatch::Sent::Waiting);
    let (gen, _) = shared.desired(); // the manager reads the spec
    shared.attach_writer(Some(Box::new(std::io::sink()))); // the stdin attach restates and syncs
    assert!(!shared.respawn_if_sources_moved(gen), "no change during the spawn: no respawn");
    shared.spawn_sources.set(A_FLAG); // a flag only: restated at the attach, no respawn
    shared.attach_writer(Some(Box::new(std::io::sink())));
    assert!(!shared.respawn_if_sources_moved(gen), "a live change: no respawn");
    assert_eq!(shared.generation(), gen);
    let (gen, _) = shared.desired();
    shared.spawn_sources.set(B); // another device, while the pid is still 0
    shared.attach_writer(Some(Box::new(std::io::sink()))); // no sync: withheld
    assert!(shared.respawn_if_sources_moved(gen), "the change during the spawn respawns");
    assert_eq!(shared.generation(), gen + 1, "a retarget to the same target");
    assert_eq!(shared.desired.lock().unwrap().2, crate::record_dispatch::Sent::Restarting, "a line meanwhile says restarting");
    let (_, respawn) = shared.desired();
    assert_eq!(respawn.unwrap().env, vec![(SOURCES_ENV.to_string(), Some(B.to_string()))], "on the new list");
    assert!(!shared.respawn_if_sources_moved(gen + 1), "once");
    // The manager calls it right after the pid shows (source guard: the loop needs an engine).
    let manager: String = include_str!("supervisor_manager.rs").split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(manager.contains("shared.set_health(EngineHealth::Running { pid }); shared.respawn_if_sources_moved(gen);"), "the manager checks right after the pid");
}

/// F-audio-1, the controller side: every change of the list, the live-only ones included, gives
/// the list a later spawn must start with; no change gives none.
#[test]
fn every_source_change_reaches_the_spawn_list() {
    use crate::record_audio_sources::{engine_value, spawn_update, AudioSource};
    let line: AudioSource = serde_json::from_value(serde_json::json!({"id": "line", "kind": "input", "device": "Line (Box)", "device_id": "{A}", "mic_mute": false})).unwrap();
    let muted = AudioSource { mic_mute: Some(true), ..line.clone() };
    let repicked = AudioSource { device_id: "{B}".into(), ..line.clone() };
    assert_eq!(spawn_update(std::slice::from_ref(&line), std::slice::from_ref(&line)), None);
    assert_eq!(spawn_update(std::slice::from_ref(&line), std::slice::from_ref(&muted)), Some(engine_value(&[muted])));
    assert_eq!(spawn_update(std::slice::from_ref(&line), std::slice::from_ref(&repicked)), Some(engine_value(&[repicked])));
}

// --- intended restarts (fake engine: cmd.exe idles on its piped stdin) --

/// `cmd.exe` stands in for the engine: it ignores `--json`, idles reading
/// stdin, and never answers `quit`, so every stop exercises grace -> kill.
/// No capture, no GPU.
fn fake_engine() -> PathBuf {
    let comspec = std::env::var_os("ComSpec").map(PathBuf::from);
    comspec.unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\cmd.exe"))
}

#[test]
fn retargets_restart_on_purpose_and_never_trip_give_up() {
    let engine = fake_engine();
    let sup = Supervisor::start_with_locator(move || Some(engine), None);
    sup.shared.restart_grace_ms.store(50, Ordering::SeqCst);

    // Idle until told what to capture.
    thread::sleep(Duration::from_millis(200));
    assert_eq!(sup.health(), EngineHealth::Stopped);
    assert_eq!(sup.shared.spawn_count.load(Ordering::SeqCst), 0);

    // More retargets than the give-up threshold (6): none may count.
    for n in 1..=8u32 {
        sup.retarget(Some(EngineSpec { env: vec![("RECALL_CAPTURE_NAME".into(), Some(format!("game-{n}")))] }));
        assert!(
            wait_for(5, || sup.shared.spawn_count.load(Ordering::SeqCst) == n
                && matches!(sup.health(), EngineHealth::Running { pid } if pid != 0)),
            "retarget {n}: health {:?}, spawns {}",
            sup.health(),
            sup.shared.spawn_count.load(Ordering::SeqCst)
        );
    }
    assert_ne!(sup.health(), EngineHealth::GivenUp);

    // retarget(None) stops the engine and idles.
    sup.retarget(None);
    assert!(wait_for(5, || sup.health() == EngineHealth::Stopped));
    assert_eq!(sup.shared.spawn_count.load(Ordering::SeqCst), 8, "no respawn while idle");
    sup.shutdown(Duration::from_millis(50));
}

#[test]
fn encoder_recovery_restarts_to_the_same_target_never_raise_give_up() {
    // `record_recover` restarts with the SAME spec (`restart_engine(RestartReason::Recovery)`).
    let engine = fake_engine();
    let sup = Supervisor::start_with_locator(move || Some(engine), None);
    sup.shared.restart_grace_ms.store(50, Ordering::SeqCst);
    let spec = EngineSpec { env: vec![("RECALL_CAPTURE_NAME".into(), Some("VALORANT".into()))] };
    for n in 1..=8u32 {
        sup.retarget(Some(spec.clone()));
        assert!(
            wait_for(5, || sup.shared.spawn_count.load(Ordering::SeqCst) == n
                && matches!(sup.health(), EngineHealth::Running { pid } if pid != 0)),
            "restart {n}: health {:?}",
            sup.health()
        );
    }
    assert!(!matches!(sup.health(), EngineHealth::GivenUp | EngineHealth::Restarting { .. }));
    sup.shutdown(Duration::from_millis(50));
}

#[test]
fn every_engine_exit_reaches_the_controller_after_its_last_event() {
    let engine = fake_engine();
    let sup = Supervisor::start_with_locator(move || Some(engine), None);
    sup.shared.restart_grace_ms.store(50, Ordering::SeqCst);
    let (tx, rx) = std::sync::mpsc::channel();
    sup.attach_events(tx);
    sup.retarget(Some(EngineSpec { env: vec![("RECALL_CAPTURE_NAME".into(), Some("game-1".into()))] }));
    assert!(wait_for(5, || matches!(sup.health(), EngineHealth::Running { pid } if pid != 0)));
    sup.retarget(Some(EngineSpec { env: vec![("RECALL_CAPTURE_NAME".into(), Some("game-2".into()))] }));
    // The fake engine has no protocol marker: its spawn also tells the controller so (P1).
    let event = std::iter::from_fn(|| rx.recv_timeout(Duration::from_secs(5)).ok()).find(|e| e["type"] != crate::supervisor::floor::ENGINE_OUTDATED).expect("the first engine's exit is reported");
    assert_eq!(event["type"], ENGINE_EXITED);
    sup.shutdown(Duration::from_millis(50));
}

// --- LIVE test against the real engine --------------------------------
//
// Requires the real capture engine. It starts screen capture for a few
// seconds. Ignored by default; run with `-- --ignored --nocapture`.

fn real_engine_path() -> Option<PathBuf> {
    ["release", "debug"]
        .iter()
        .map(|profile| target_dir().join(profile).join("recall.exe"))
        .find(|p| p.is_file())
}

/// `CARGO_TARGET_DIR`, else the workspace's own `target` folder.
fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("target"))
}

fn pid_alive(pid: u32) -> bool {
    match Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH"]).output() {
        Ok(o) => {
            let s = String::from_utf8_lossy(&o.stdout);
            s.contains(&pid.to_string()) && s.to_lowercase().contains("recall")
        }
        Err(_) => false,
    }
}

#[test]
#[ignore = "live: spawns the real recall.exe (screen capture) for a few seconds"]
fn live_spawn_hello_shutdown_no_orphan() {
    if !crate::hw_gate::enabled("live_spawn_hello_shutdown_no_orphan") {
        return;
    }
    let Some(engine) = real_engine_path() else {
        panic!("real recall.exe not found under {}", target_dir().display());
    };
    let sup = Supervisor::start_with_locator(move || Some(engine), Some(EngineSpec::default()));
    let mut pid = 0;
    assert!(wait_for(5, || {
        if let EngineHealth::Running { pid: p } = sup.health() {
            pid = p;
        }
        pid != 0
    }));
    assert!(pid_alive(pid), "OS confirms engine pid {pid} is alive");
    assert!(wait_for(5, || sup.hello_count() > 0), "no hello parsed within 5s");
    assert!(!sup.cached_engine_events().is_empty(), "hello cached for late joiners");
    sup.shutdown(Duration::from_secs(3));
    assert!(wait_for(3, || !pid_alive(pid)), "orphan: engine pid {pid} still alive");
}

#[test]
fn quit_grace_outlasts_the_engines_own_quit_path() {
    // recall/src/main.rs: long recording finalize 1.5 s, then the in-flight
    // save 3 s, plus the event loop's own exit.
    let engine_quit = Duration::from_millis(1_500) + Duration::from_secs(3);
    assert!(QUIT_GRACE >= engine_quit + Duration::from_secs(3), "{QUIT_GRACE:?}");
    assert_eq!(RETARGET_GRACE, QUIT_GRACE, "a retarget never kills a save either");
    assert_eq!(Shared::new(None).restart_grace(), QUIT_GRACE);
}

// --- a wedged engine stdin (gate P2-c) ----------------------------------

/// An engine stdin whose pipe is full and never read: every write blocks for good. With
/// `parked`, it says so once its writer thread has taken a line and is stuck in `write`.
pub(crate) struct Wedged {
    parked: Option<std::sync::mpsc::Sender<()>>,
}

impl Wedged {
    pub(crate) fn new() -> Wedged {
        Wedged { parked: None }
    }
}

impl std::io::Write for Wedged {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        if let Some(parked) = self.parked.take() {
            let _ = parked.send(());
        }
        loop {
            thread::park();
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_wedged_engine_stdin_never_blocks_the_pump() {
    let shared = Arc::new(Shared::new(None));
    shared.attach_writer(Some(Box::new(Wedged::new())));
    let pump = shared.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let started = Instant::now();
        for _ in 0..1_000 {
            pump.send_line(r#"{"cmd":"record_fail","reason":"the encoder isn't responding"}"#);
        }
        let _ = tx.send(started.elapsed());
    });
    let took = rx.recv_timeout(Duration::from_secs(3)).expect("the pump blocked on a wedged engine stdin");
    assert!(took < Duration::from_millis(500), "1000 lines took {took:?}");
}

#[test]
fn a_wedged_engine_stdin_still_gets_killed_within_its_grace() {
    let engine = fake_engine();
    // Never dropped on a failure: a manager blocked on stdin would hang the drop.
    let sup = std::mem::ManuallyDrop::new(Supervisor::start_with_locator(move || Some(engine), None));
    sup.shared.restart_grace_ms.store(50, Ordering::SeqCst);
    sup.retarget(Some(EngineSpec::default()));
    assert!(wait_for(5, || matches!(sup.health(), EngineHealth::Running { pid } if pid != 0)));
    // The engine stops reading its stdin (the pipe fills); the pump writes on.
    sup.shared.attach_writer(Some(Box::new(Wedged::new())));
    let pump = sup.shared.clone();
    thread::spawn(move || pump.send_line(r#"{"cmd":"save_clip"}"#));
    thread::sleep(Duration::from_millis(100));
    let asked = Instant::now();
    sup.retarget(Some(EngineSpec { env: vec![("RECALL_CAPTURE_NAME".into(), Some("next".into()))] }));
    let replaced = wait_for(5, || sup.shared.spawn_count.load(Ordering::SeqCst) == 2);
    assert!(replaced, "the wedged engine was never killed (manager stuck on its stdin)");
    assert!(asked.elapsed() < Duration::from_secs(3), "the kill took {:?}", asked.elapsed());
    std::mem::ManuallyDrop::into_inner(sup).shutdown(Duration::from_millis(50));
}

#[test]
fn a_line_to_a_full_queue_is_reported_not_queued() {
    let shared = Arc::new(Shared::new(None));
    assert!(!shared.send_line(r#"{"cmd":"save_clip"}"#).queued(), "no engine: nothing queued");
    // The writer takes one line and wedges on it; only then is the queue filled, so no line
    // the writer takes late can free a slot after the fill (the 11-08 flake).
    let queued = wedge_and_fill(&shared);
    assert_eq!(queued, stdin::QUEUE, "{queued} lines queued behind the wedged one");
    assert!(!shared.send_line(r#"{"cmd":"save_clip"}"#).queued(), "a full queue said it queued the line");
}

/// Attach a wedged writer, wait until it has taken one line and parked, then fill the queue
/// until a send says no. The number of lines queued behind the wedged one. The attach's
/// restatement (`sync`, design-audio-mute P8) is the line it wedges on, so the first line sent
/// here waits in the queue too.
pub(crate) fn wedge_and_fill(shared: &Shared) -> usize {
    let (tx, rx) = std::sync::mpsc::channel();
    shared.attach_writer(Some(Box::new(Wedged { parked: Some(tx) })));
    rx.recv_timeout(Duration::from_secs(5)).expect("the wedged writer never took its line");
    let queued = (0..1_000).take_while(|_| shared.send_line("{}").queued()).count();
    assert!(queued < 1_000, "the queue never said it was full");
    queued
}

/// A writer that takes lines only once `open` says so (a healthy engine that is busy). It
/// says once (`entered`) when its writer thread is stuck on its first line.
struct Gated {
    open: Arc<AtomicBool>,
    seen: Arc<Mutex<Vec<String>>>,
    entered: Option<std::sync::mpsc::Sender<()>>,
}

impl std::io::Write for Gated {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if let Some(entered) = self.entered.take() {
            let _ = entered.send(());
        }
        while !self.open.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(5));
        }
        self.seen.lock().unwrap().push(String::from_utf8_lossy(b).into_owned());
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn quit_keeps_a_slot_so_a_busy_engine_behind_a_full_queue_still_finishes_cleanly() {
    let shared = Arc::new(Shared::new(None));
    let open = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (tx, rx) = std::sync::mpsc::channel();
    shared.attach_writer(Some(Box::new(Gated { open: open.clone(), seen: seen.clone(), entered: Some(tx) })));
    // The writer is stuck on its first line (the attach's `sync`) before the fill, so the fill
    // really ends full.
    rx.recv_timeout(Duration::from_secs(5)).expect("the writer never took its first line");
    let filled = (0..1_000).take_while(|_| shared.send_line(r#"{"cmd":"bookmark"}"#).queued()).count();
    assert_eq!(filled, stdin::QUEUE, "the queue never said it was full");
    assert!(shared.send_quit(Sent::Switching), "quit found the queue full");
    open.store(true, Ordering::SeqCst);
    assert!(wait_for(5, || seen.lock().unwrap().iter().any(|l| l.contains("quit"))), "quit never reached the engine");
}
