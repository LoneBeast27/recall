//! Merge gate p9, mute race (P1): stdin is read before any track opens, and Alt+M applies at
//! once. A pipe-like reader stands in for stdin; the ring is a test input (no WASAPI). The only
//! test that moves the engine's global mute: it restores it when done, a panic included.

use std::io::Read;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::read_early;
use crate::audio::{self, MuteNow, TrackRing};
use crate::protocol::Command;

/// Bytes as the controller writes them, a line at a time; ends when the sender is dropped.
struct Pipe {
    rx: Receiver<Vec<u8>>,
    buf: Vec<u8>,
}

impl Read for Pipe {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.buf.is_empty() {
            match self.rx.recv() {
                Ok(bytes) => self.buf = bytes,
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len());
        out[..n].copy_from_slice(&self.buf[..n]);
        self.buf.drain(..n);
        Ok(n)
    }
}

/// Puts the global mute back as it was.
struct Restore(bool);

impl Drop for Restore {
    fn drop(&mut self) {
        audio::set_mic_muted(self.0);
    }
}

fn spawn(quit: Arc<AtomicBool>) -> (Sender<Vec<u8>>, Receiver<Command>) {
    let (line_tx, line_rx) = channel();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(Pipe { rx: line_rx, buf: Vec::new() });
        let end = read_early(reader, |cmd| {
            let _ = tx.send(cmd);
        });
        crate::stdin_reader::reader_ended(end, &quit, &AtomicBool::new(false));
    });
    (line_tx, rx)
}

#[test]
fn an_alt_m_sent_within_50_ms_of_the_spawn_mutes_the_first_samples() {
    let _restore = Restore(audio::mic_muted());
    audio::set_mic_muted(false);
    let quit = Arc::new(AtomicBool::new(false));
    let spawned = Instant::now();
    // The engine reads stdin from here on, before any track opens.
    let (lines, commands) = spawn(quit.clone());
    lines.send(b"{\"cmd\":\"toggle_mic_mute\"}\n".to_vec()).unwrap();
    assert!(spawned.elapsed() < Duration::from_millis(50), "the press came within 50 ms of the spawn");
    // The first track opens after that and stores its first packet.
    std::thread::sleep(Duration::from_millis(50));
    let ring = TrackRing::input_for_test("mic");
    ring.push_live(&[0.5; 960], MuteNow::read());
    let held = ring.snapshot(0.01);
    assert!(held.iter().all(|s| *s == 0.0), "the ring holds no live sample of a muted mic");
    // The later handler gets the state the toggle set (it reports it once `hello` is out).
    assert!(matches!(commands.recv_timeout(Duration::from_secs(2)), Ok(Command::SetMicMute { muted: true })));
    // Post-hoc item 5: nothing but an explicit mute command moves the mute. A source's own
    // mic_mute flag, a device change, a mode or a save line leave the mic muted.
    for line in [
        r#"{"cmd":"set_source","id":"in-1","mic_mute":false}"#,
        r#"{"cmd":"set_source","id":"mic","device_id":"{0.0.1.00000000}.{x}","gain_db":3}"#,
        r#"{"cmd":"set_mode","value":"auto"}"#,
    ] {
        lines.send(format!("{line}
").into_bytes()).unwrap();
        commands.recv_timeout(Duration::from_secs(2)).expect("forwarded");
        assert!(audio::mic_muted(), "{line} unmuted the mic");
    }
    drop(lines);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !quit.load(std::sync::atomic::Ordering::Relaxed) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(quit.load(std::sync::atomic::Ordering::Relaxed), "the end of stdin still quits");
}

/// A pipe already holding `lines`, closed after them only when `close` (else it stays open,
/// as the controller's does after `quit`).
fn early_from(lines: &str, close: bool) -> (super::Early, Arc<crate::capture::CaptureShared>, Sender<Vec<u8>>) {
    let shared = crate::capture::CaptureShared::new();
    let (line_tx, line_rx) = channel();
    line_tx.send(lines.as_bytes().to_vec()).unwrap();
    let keep = line_tx.clone();
    if close {
        drop(line_tx);
    }
    let early = super::spawn_early_from(move || std::io::BufReader::new(Pipe { rx: line_rx, buf: Vec::new() }), shared.clone());
    (early, shared, keep)
}

fn wait_for(flag: &AtomicBool, within: Duration) -> Option<Duration> {
    let started = Instant::now();
    while started.elapsed() < within {
        if flag.load(std::sync::atomic::Ordering::SeqCst) {
            return Some(started.elapsed());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

/// R2-rel-1: Alt+S, then the privacy stop, read back to back while the serve thread is starved
/// for 200 ms. `quit` (which ends the event loop and, through it, the encoder) is not set
/// before the save was handed on, so the save reaches the encoder.
#[test]
fn a_save_read_just_before_a_stop_is_applied_before_quit_even_when_serve_is_starved() {
    let (early, shared, _pipe) = early_from("{\"cmd\":\"save_clip\"}\n{\"cmd\":\"stop_capture\"}\n", false);
    let quit = shared.clone();
    let server = std::thread::spawn(move || {
        let mut applied = Vec::new();
        super::serve_with(early, |cmd| {
            std::thread::sleep(Duration::from_millis(200)); // the starved serve thread
            applied.push((matches!(cmd, Command::SaveClip { .. }), quit.quit.load(std::sync::atomic::Ordering::SeqCst)));
        });
        applied
    });
    let took = wait_for(&shared.quit, Duration::from_secs(3)).expect("the stop still quits");
    assert!(took < super::SERVE_TAKES_END, "set by the serve thread, not the fallback ({took:?})");
    drop(_pipe);
    let applied = server.join().unwrap();
    assert_eq!(applied, vec![(true, false)], "the save was handed on while quit was still unset");
}

/// R2-rel-1, plain quit: the commands before `quit` are applied, then `quit` is set at once.
#[test]
fn a_plain_quit_is_set_by_serve_after_the_commands_before_it() {
    let (early, shared, _pipe) = early_from("{\"cmd\":\"bookmark\"}\n{\"cmd\":\"quit\"}\n", false);
    let seen = Arc::new(AtomicBool::new(false));
    let mark = seen.clone();
    std::thread::spawn(move || super::serve_with(early, move |_| mark.store(true, std::sync::atomic::Ordering::SeqCst)));
    let took = wait_for(&shared.quit, Duration::from_secs(3)).expect("quit");
    assert!(seen.load(std::sync::atomic::Ordering::SeqCst) && took < Duration::from_millis(1_000), "{took:?}");
}

/// R2-rel-1 bound: a serve thread that never takes the end (not started yet, or stuck) holds a
/// quit at most SERVE_TAKES_END; an end of stdin still marks the engine orphaned.
#[test]
fn a_quit_is_never_held_past_the_bound_when_serve_does_not_run() {
    let (_early, shared, _pipe) = early_from("{\"cmd\":\"quit\"}\n", false);
    let took = wait_for(&shared.quit, Duration::from_secs(5)).expect("the fallback quits");
    assert!(took >= super::SERVE_TAKES_END - Duration::from_millis(100) && took < super::SERVE_TAKES_END + Duration::from_secs(1), "{took:?}");
}
