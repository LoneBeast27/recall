//! Lines sent while the engine quits (third audit U1): from the `quit` on, every line is
//! refused with why, never written behind the `quit` the engine stops reading at. A game
//! switch says "Switching games"; any other quit (hang or failure restart, shutdown) says
//! "Recorder isn't responding".

use super::tests::wedge_and_fill;
use super::*;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Instant;

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

/// An engine stdin that keeps every byte; says once (`done`) when its writer thread ends.
pub(crate) struct Recorder {
    bytes: Arc<Mutex<Vec<u8>>>,
    done: Option<Sender<()>>,
}

impl std::io::Write for Recorder {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.bytes.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(done) = self.done.take() {
            let _ = done.send(());
        }
    }
}

/// What a [`Recorder`] got, read once its writer has ended.
pub(crate) struct Recorded {
    bytes: Arc<Mutex<Vec<u8>>>,
    done: Receiver<()>,
    /// The child's own stdin, held open (a fake engine ends on stdin EOF).
    _child: Option<stdin::StdinQueue>,
}

impl Recorded {
    /// A recorder for `shared`'s engine stdin.
    pub(super) fn attach(shared: &Shared) -> Recorded {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let (tx, done) = channel();
        let child = shared.child_stdin.lock().unwrap().queue.take();
        shared.attach_writer(Some(Box::new(Recorder { bytes: bytes.clone(), done: Some(tx) })));
        Recorded { bytes, done, _child: child }
    }

    /// A recorder attached as a spawn's stdin whose env was given mute `baked`.
    pub(super) fn attach_spawned(shared: &Shared, baked: Option<bool>) -> Recorded {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let (tx, done) = channel();
        shared.attach_spawned(Some(Box::new(Recorder { bytes: bytes.clone(), done: Some(tx) })), baked);
        Recorded { bytes, done, _child: None }
    }

    /// Every line written but the audio restatement and other audio lines (they carry a `seq`),
    /// once the writer ended (its queue closed: `attach_writer`).
    pub(crate) fn lines(&self) -> Vec<String> {
        self.raw_lines().into_iter().filter(|l| !l.contains(r#""seq":"#)).collect()
    }

    /// Every line written, once the writer ended.
    pub(crate) fn raw_lines(&self) -> Vec<String> {
        self.done.recv_timeout(Duration::from_secs(5)).expect("the stdin writer never ended");
        String::from_utf8_lossy(&self.bytes.lock().unwrap()).lines().map(str::to_string).collect()
    }
}

impl Recorded {
    /// The lines written once `sup`'s engine stdin is closed (its writer ends).
    pub(crate) fn finish(&self, sup: &Supervisor) -> Vec<String> {
        sup.shared.attach_writer(None);
        self.raw_lines()
    }
}

impl Supervisor {
    /// A supervisor with no manager whose engine stdin is a recorder (as after an attach).
    pub(crate) fn recorded_for_test() -> (Supervisor, Recorded) {
        let sup = Supervisor { shared: Arc::new(Shared::new(None)), handle: None };
        let rec = Recorded::attach(&sup.shared);
        (sup, rec)
    }
}

/// The lines written after the `quit` (none may be).
pub(crate) fn after_quit(lines: &[String]) -> Vec<String> {
    let quit = lines.iter().position(|l| l.contains(r#""quit""#)).expect("quit was never written");
    lines[quit + 1..].to_vec()
}

/// An engine stdin for tests elsewhere: `send` answers as `Supervisor::send_engine_line`.
pub(crate) struct TestPipe(Arc<Shared>);

impl TestPipe {
    /// A pipe whose engine reads nothing (every write blocks) and whose queue is full. The
    /// fill starts only once the writer has taken its line and parked, so no slot can free
    /// up afterwards (`tests::wedge_and_fill`).
    pub fn wedged_and_full() -> TestPipe {
        let shared = Arc::new(Shared::new(None));
        wedge_and_fill(&shared);
        TestPipe(shared)
    }

    /// A pipe whose engine reads every line.
    pub fn healthy() -> TestPipe {
        let shared = Arc::new(Shared::new(None));
        shared.attach_writer(Some(Box::new(std::io::sink())));
        TestPipe(shared)
    }

    pub fn send(&self, line: &str) -> Sent {
        self.0.send_line(line)
    }

    /// A pipe whose engine reads every line and was just told to quit (`refusal`: why the
    /// lines after it are refused); the recorder has every line its engine got.
    pub fn quitting(refusal: Sent) -> (TestPipe, Recorded) {
        let shared = Arc::new(Shared::new(None));
        let rec = Recorded::attach(&shared);
        assert!(shared.send_quit(refusal));
        (TestPipe(shared), rec)
    }

    /// A pipe whose engine was told to stop (`retarget(None)`: `set_desired(None, asked)`)
    /// and got its quit as the manager sends it (`send_retarget_quit`).
    pub fn stopping(asked: Sent) -> (TestPipe, Recorded) {
        let shared = Arc::new(Shared::new(None));
        let rec = Recorded::attach(&shared);
        shared.set_desired(None, asked);
        assert!(shared.send_retarget_quit());
        (TestPipe(shared), rec)
    }

    /// The engine is gone: its writer ends (the recorder's lines are then complete).
    pub fn close(&self) {
        self.0.attach_writer(None);
    }
}

const SAVE: &str = r#"{"cmd":"save_clip","seconds":30}"#;

#[test]
fn a_save_after_the_retarget_quit_is_refused_as_switching_and_never_written() {
    let shared = Shared::new(None);
    let rec = Recorded::attach(&shared);
    assert_eq!(shared.send_line(SAVE), Sent::Queued, "before the quit: A's clip");
    assert!(shared.send_quit(Sent::Switching));
    assert_eq!(shared.send_line(SAVE), Sent::Switching, "after the quit: refused, with why");
    assert_eq!(shared.send_line(r#"{"cmd":"bookmark"}"#), Sent::Switching);
    shared.attach_writer(None); // the old engine is gone: still switching until the next one
    assert_eq!(shared.send_line(SAVE), Sent::Switching, "between the old engine and the next");
    let lines = rec.lines();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(after_quit(&lines), Vec::<String>::new(), "a line went behind the quit");
    shared.attach_writer(Some(Box::new(std::io::sink())));
    assert_eq!(shared.send_line(SAVE), Sent::Queued, "the next engine takes lines again");
}

#[test]
fn a_full_queue_and_a_hang_restart_quit_say_not_responding_never_switching() {
    let full = Shared::new(None);
    wedge_and_fill(&full);
    assert_eq!(full.send_line(SAVE), Sent::NotResponding, "a hung engine's full queue");
    let shared = Shared::new(None);
    let rec = Recorded::attach(&shared);
    assert!(shared.send_quit(Sent::NotResponding));
    assert_eq!(shared.send_line(SAVE), Sent::NotResponding, "a hang-restart quit is no game switch");
    shared.attach_writer(None);
    assert_eq!(after_quit(&rec.lines()), Vec::<String>::new(), "a line went behind the quit");
}

/// Send saves until one is refused; the refusal (the fake engine never quits by itself,
/// so the old engine's quit window lasts its whole grace).
fn first_refusal(sup: &Supervisor) -> Sent {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let sent = sup.send_engine_line(SAVE);
        if !sent.queued() {
            return sent;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("no save was refused in 5 s");
}

/// The manager path: `retarget` (a game switch) and `restart` (a hang) each quit the old
/// engine; a save in that window gets the matching reason and never lands behind the quit.
fn quit_window(quit: impl FnOnce(&Supervisor, Option<EngineSpec>)) -> (Sent, Vec<String>) {
    let engine = std::env::var_os("ComSpec").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\cmd.exe"));
    let sup = Supervisor::start_with_locator(move || Some(engine), None);
    sup.shared.restart_grace_ms.store(1_000, Ordering::SeqCst);
    sup.retarget(Some(EngineSpec::default()));
    assert!(wait_for(5, || matches!(sup.health(), EngineHealth::Running { pid } if pid != 0)));
    let rec = Recorded::attach(&sup.shared); // stands in for the engine's stdin
    let next = Some(EngineSpec { env: vec![("RECALL_CAPTURE_NAME".into(), Some("next".into()))] });
    quit(&sup, next);
    let refused = first_refusal(&sup);
    assert!(matches!(sup.health(), EngineHealth::Running { pid } if pid != 0), "refused while the old engine still runs");
    assert!(wait_for(5, || sup.shared.spawn_count.load(Ordering::SeqCst) == 2), "the next engine never started");
    let lines = rec.lines();
    sup.shutdown(Duration::from_millis(50));
    (refused, lines)
}

#[test]
fn a_save_while_the_old_engine_quits_for_a_retarget_says_switching() {
    let (refused, lines) = quit_window(|sup, next| sup.retarget(next));
    assert_eq!(refused, Sent::Switching);
    assert_eq!(after_quit(&lines), Vec::<String>::new(), "a save went behind the quit");
}

#[test]
fn a_save_while_the_old_engine_quits_for_a_hang_restart_says_not_responding() {
    let (refused, lines) = quit_window(|sup, next| sup.restart(next, false));
    assert_eq!(refused, Sent::NotResponding, "a hang restart is no game switch");
    assert_eq!(after_quit(&lines), Vec::<String>::new(), "a save went behind the quit");
}

/// A settings change: `restart_engine(RestartReason::Settings)` (record_actions.rs `request_engine_restart`).
#[test]
fn a_save_while_the_old_engine_quits_for_a_settings_restart_says_restarting() {
    let (refused, lines) = quit_window(|sup, next| sup.restart(next, true));
    assert_eq!(refused, Sent::Restarting, "a planned restart");
    assert_eq!(refused.reason(), Some("Recorder is restarting, try again in a moment"));
    assert_eq!(after_quit(&lines), Vec::<String>::new(), "a save went behind the quit");
}

/// A resume from sleep: `restart_engine(RestartReason::Resume)` (record_power_win.rs RestartEngine),
/// the same engine spec again.
#[test]
fn a_save_while_the_old_engine_quits_for_a_resume_restart_says_restarting() {
    let (refused, lines) = quit_window(|sup, _next| sup.restart(Some(EngineSpec::default()), true));
    assert_eq!(refused, Sent::Restarting);
    assert_eq!(after_quit(&lines), Vec::<String>::new(), "a save went behind the quit");
}

