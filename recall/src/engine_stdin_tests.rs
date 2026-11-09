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

/// Tests that move the engine's global mute run one at a time.
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Puts the global mute back as it was, then lets the next test in.
struct Restore(bool, #[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

fn restore() -> Restore {
    let lock = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    Restore(audio::mic_muted(), lock)
}

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
    let _restore = restore();
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
    assert!(matches!(commands.recv_timeout(Duration::from_secs(2)), Ok(Command::SetMicMute { muted: true, .. })));
    drop(lines);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !quit.load(std::sync::atomic::Ordering::Relaxed) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(quit.load(std::sync::atomic::Ordering::Relaxed), "the end of stdin still quits");
}

/// Gate review P2-b: a resync (`set_mic_mute` with no `press`) is not a press, so it never
/// releases an input held on an unsure sibling; the user's change (`press`) is one.
#[test]
fn a_resync_is_not_a_press() {
    let _restore = restore();
    audio::set_mic_muted(false);
    let presses = MuteNow::read().presses;
    let lines = "{\"cmd\":\"set_mic_mute\",\"muted\":true}\n{\"cmd\":\"set_mic_mute\",\"muted\":false}\n";
    read_early(std::io::Cursor::new(lines), |_| {});
    assert_eq!(MuteNow::read().presses, presses, "two resyncs that changed the state: no press");
    read_early(std::io::Cursor::new("{\"cmd\":\"set_mic_mute\",\"muted\":true,\"press\":true}\n"), |_| {});
    assert_eq!(MuteNow::read(), MuteNow { muted: true, presses: presses + 1 }, "the user's change is a press");
}

/// Review R2-audio-r9-2: a line-in set to "Alt+M mutes it", and a twin re-picked, while the
/// engine starts: the `set_source` is read before the tracks start and in force at their first
/// packet, not only once `serve` runs after them.
#[test]
fn a_set_source_queued_before_audio_start_is_in_force_at_the_first_packet() {
    let _restore = restore();
    read_early(std::io::Cursor::new("{\"cmd\":\"set_source\",\"id\":\"line\",\"mic_mute\":true,\"device_id\":\"{B}\"}\n"), |_| {});
    let env = r#"[{"id":"line","kind":"input","name":"Line-in","device":"Line (Box)","device_id":"{A}","mic_mute":false}]"#;
    let list = audio::sources::resolve_for_start(|key| (key == "RECALL_AUDIO_SOURCES").then(|| env.to_owned())).unwrap();
    assert_eq!(list[0].mic_mute, Some(true), "the line-in starts muted by Alt+M");
    assert_eq!(list[0].device_id, "{B}", "the twin the user picked");
}
