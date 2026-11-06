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
    let cmd = engine_command(Path::new("recall.exe"), &spec);
    let envs: Vec<_> = cmd.get_envs().map(|(k, v)| (k.to_owned(), v.map(|v| v.to_owned()))).collect();
    assert!(envs.contains(&("RECALL_CAPTURE_NAME".into(), Some("VALORANT".into()))));
    assert!(envs.contains(&("RECALL_CAPTURE_HWND".into(), None)), "None removes an inherited value");
    assert_eq!(cmd.get_args().collect::<Vec<_>>(), vec!["--json"]);
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
    // `record_recover` restarts with the SAME spec (`restart_engine_for_settings`).
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

/// An engine stdin whose pipe is full and never read: every write blocks for good.
pub(crate) struct Wedged;

impl std::io::Write for Wedged {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
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
    shared.attach_writer(Some(Box::new(Wedged)));
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
    sup.shared.attach_writer(Some(Box::new(Wedged)));
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
    assert!(!shared.send_line(r#"{"cmd":"save_clip"}"#), "no engine: nothing queued");
    shared.attach_writer(Some(Box::new(Wedged)));
    let queued = (0..200).filter(|_| shared.send_line(r#"{"cmd":"save_clip"}"#)).count();
    assert!(queued <= stdin::QUEUE + 1, "{queued} lines queued to a 64-line queue");
    assert!(!shared.send_line(r#"{"cmd":"save_clip"}"#), "a full queue said it queued the line");
}

/// A writer that takes lines only once `open` says so (a healthy engine that is busy).
struct Gated {
    open: Arc<AtomicBool>,
    seen: Arc<Mutex<Vec<String>>>,
}

impl std::io::Write for Gated {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
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
    shared.attach_writer(Some(Box::new(Gated { open: open.clone(), seen: seen.clone() })));
    let filled = (0..1_000).take_while(|_| shared.send_line(r#"{"cmd":"bookmark"}"#)).count();
    assert!(filled < 1_000, "the queue never said it was full");
    assert!(shared.send_quit(), "quit found the queue full");
    open.store(true, Ordering::SeqCst);
    assert!(wait_for(5, || seen.lock().unwrap().iter().any(|l| l.contains("quit"))), "quit never reached the engine");
}
