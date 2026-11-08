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
        read_early(reader, &quit, |cmd| {
            let _ = tx.send(cmd);
        });
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
    drop(lines);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !quit.load(std::sync::atomic::Ordering::Relaxed) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(quit.load(std::sync::atomic::Ordering::Relaxed), "the end of stdin still quits");
}
