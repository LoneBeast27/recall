//! Control IPC: a named-pipe server (`\\.\pipe\recall-hdr-control-<session>`, this user
//! and SYSTEM only: `control_pipe`) speaking
//! newline-JSON. The controller BROADCASTS namespaced events
//! (`{"mod":"engine"|"session"|"hdr"|"hub","event":{...}}`) to every connected
//! client and forwards each inbound line, tagged with its client id, to the pump
//! thread (commands never execute on pipe threads).
//!
//! # Never block the caller
//! Every client owns a bounded queue drained by its own writer thread, so
//! [`PipeHub::broadcast`] is a `try_send` per client. A client that stops
//! reading fills its queue and further lines for it are DROPPED and counted
//! ([`PipeHub::dropped`]); the engine stdout relay that calls `broadcast` can
//! therefore never stall on a slow UI (which would back-pressure the engine's
//! stdout and stall capture).
//!
//! On connect the hub sends the new client a `hub` hello and injects a
//! synthetic `{"cmd":"status"}` from that client, so the pump replies with the
//! current status (and the cached engine `hello`/`status`) to late joiners.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

use interprocess::local_socket::traits::{Listener, Stream as _};
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, Stream, ToNsName};
use interprocess::os::windows::local_socket::ListenerOptionsExt;

use crate::control_lines::{read_lines_capped, LinesEnd, MAX_LINE};
use crate::control_pipe;

use crate::log;

/// The pipe server gave up (security, name or listen failed): set once, never cleared.
static LINK_DOWN: AtomicBool = AtomicBool::new(false);

/// The control pipe is off for this run, so no app or island can reach the recorder. The
/// tray says so (`record_tray_slot`, audit R2-deep-startup-3); the reason is in the log.
pub fn link_down() -> bool {
    LINK_DOWN.load(Ordering::Acquire)
}

fn give_up(why: &str) {
    log!("control: {why}; control IPC disabled");
    LINK_DOWN.store(true, Ordering::Release);
}

/// Lines buffered per client before new lines for it are dropped.
pub const CLIENT_QUEUE: usize = 256;

/// One inbound command line and the client that sent it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inbound {
    pub client: u64,
    pub line: String,
}

struct Client {
    id: u64,
    tx: SyncSender<Arc<str>>,
    /// Consecutive drops; logs once per stall episode.
    stalled: u64,
}

/// Shared broadcaster + inbound command forwarder.
pub struct PipeHub {
    clients: Mutex<Vec<Client>>,
    commands: Sender<Inbound>,
    next_id: AtomicU64,
    dropped: AtomicU64,
}

impl PipeHub {
    /// Start the accept loop on its own thread. `commands` receives every
    /// inbound line for the pump to drain.
    pub fn start(commands: Sender<Inbound>) -> Arc<PipeHub> {
        let hub = Self::detached(commands);
        let accept_hub = hub.clone();
        std::thread::Builder::new()
            .name("control-pipe-accept".into())
            .spawn(move || accept_loop(accept_hub))
            .expect("spawn control pipe thread");
        hub
    }

    /// A hub with no pipe listener (tests attach in-memory clients).
    pub fn detached(commands: Sender<Inbound>) -> Arc<PipeHub> {
        Arc::new(PipeHub {
            clients: Mutex::new(Vec::new()),
            commands,
            next_id: AtomicU64::new(1),
            dropped: AtomicU64::new(0),
        })
    }

    /// Register a client writer; its own thread drains a bounded queue into it.
    /// The thread ends (and the client is pruned) when a write fails.
    pub fn add_client(&self, mut writer: Box<dyn Write + Send>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = sync_channel::<Arc<str>>(CLIENT_QUEUE);
        let spawned = std::thread::Builder::new()
            .name(format!("control-pipe-writer-{id}"))
            .spawn(move || {
                for line in rx {
                    let mut framed = String::with_capacity(line.len() + 1);
                    framed.push_str(&line);
                    framed.push('\n');
                    if writer.write_all(framed.as_bytes()).and_then(|_| writer.flush()).is_err() {
                        break; // dropping `rx` makes the next try_send Disconnected
                    }
                }
            });
        if spawned.is_ok() {
            if let Ok(mut clients) = self.clients.lock() {
                clients.push(Client { id, tx, stalled: 0 });
                log!("control: client {id} connected ({} total)", clients.len());
            }
        }
        id
    }

    /// Queue one JSON line for every client. Never blocks: a full queue drops
    /// the line for that client; a dead client is pruned.
    pub fn broadcast(&self, line: &str) {
        let line: Arc<str> = Arc::from(line);
        let Ok(mut clients) = self.clients.lock() else { return };
        clients.retain_mut(|c| self.offer(c, &line));
    }

    /// Queue one JSON line for a single client (replies, late-joiner snapshot).
    pub fn send_to(&self, client: u64, line: &str) {
        let line: Arc<str> = Arc::from(line);
        let Ok(mut clients) = self.clients.lock() else { return };
        clients.retain_mut(|c| c.id != client || self.offer(c, &line));
    }

    /// try_send to one client; `false` = disconnected (prune it).
    fn offer(&self, c: &mut Client, line: &Arc<str>) -> bool {
        match c.tx.try_send(line.clone()) {
            Ok(()) => {
                if c.stalled > 0 {
                    log!("control: client {} drained again after {} dropped line(s)", c.id, c.stalled);
                    c.stalled = 0;
                }
                true
            }
            Err(TrySendError::Full(_)) => {
                if c.stalled == 0 {
                    log!("control: client {} is not reading; dropping its lines", c.id);
                }
                c.stalled += 1;
                self.dropped.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(TrySendError::Disconnected(_)) => {
                log!("control: client {} disconnected", c.id);
                false
            }
        }
    }

    /// Broadcast an event under a module namespace. `event_json` must be a
    /// complete JSON object (string-concatenated to avoid a parse round-trip).
    pub fn event(&self, module: &str, event_json: &str) {
        self.broadcast(&envelope(module, event_json));
    }

    /// Send an event under a module namespace to one client.
    pub fn event_to(&self, client: u64, module: &str, event_json: &str) {
        self.send_to(client, &envelope(module, event_json));
    }

    /// Lines dropped for stalled clients since start.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn client_count(&self) -> usize {
        self.clients.lock().map(|c| c.len()).unwrap_or(0)
    }

    /// Greet a new client: hub hello + a synthetic status request so the pump
    /// replies with the current snapshot.
    fn welcome(&self, client: u64) {
        let hello = serde_json::json!({
            "type": "hello", "daemon": "recall-hdr", "version": env!("CARGO_PKG_VERSION")
        });
        self.event_to(client, "hub", &hello.to_string());
        let _ = self.commands.send(Inbound { client, line: r#"{"cmd":"status"}"#.into() });
    }

    /// Forward every line a client sends until EOF. A line past `MAX_LINE` bytes with no
    /// newline drops the client (it can't make the controller buffer without limit).
    fn read_client(&self, client: u64, reader: impl std::io::Read) {
        let end = read_lines_capped(reader, MAX_LINE, |line| self.commands.send(Inbound { client, line: line.to_string() }).is_ok());
        if end == LinesEnd::TooLong {
            log!("control: client {client} sent a line over {MAX_LINE} bytes; dropping it");
        }
        // Prune now: an idle recorder may not broadcast again for a long time, and
        // until then the dead client's writer thread and pipe handle would stay.
        self.remove(client);
    }

    /// Drop a client whose pipe ended; its writer thread exits when `tx` drops.
    fn remove(&self, client: u64) {
        let Ok(mut clients) = self.clients.lock() else { return };
        let before = clients.len();
        clients.retain(|c| c.id != client);
        if clients.len() != before {
            log!("control: client {client} disconnected ({} left)", clients.len());
        }
    }
}

fn envelope(module: &str, event_json: &str) -> String {
    format!(r#"{{"mod":"{module}","event":{event_json}}}"#)
}

fn accept_loop(hub: Arc<PipeHub>) {
    let pipe = control_pipe::current_pipe_name();
    let security = match control_pipe::owner_only_descriptor() {
        Ok(sd) => sd,
        Err(e) => {
            // Never fall back to the default DACL (read access for every local account).
            give_up(&format!("pipe security could not be built ({e})"));
            return;
        }
    };
    let name = match pipe.as_str().to_ns_name::<GenericNamespaced>() {
        Ok(n) => n,
        Err(e) => {
            give_up(&format!("pipe name invalid ({e})"));
            return;
        }
    };
    let listener = match ListenerOptions::new().name(name).security_descriptor(security).create_sync() {
        Ok(l) => l,
        Err(e) => {
            give_up(&format!("pipe listen failed ({e})"));
            return;
        }
    };
    log!("control: pipe listening (\\\\.\\pipe\\{pipe}, this user and SYSTEM only)");

    loop {
        let stream: Stream = match listener.accept() {
            Ok(s) => s,
            Err(_) => continue,
        };
        let (reader, writer) = stream.split();
        let id = hub.add_client(Box::new(writer));
        hub.welcome(id);
        let reader_hub = hub.clone();
        std::thread::Builder::new()
            .name("control-pipe-client".into())
            .spawn(move || reader_hub.read_client(id, reader))
            .ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{channel, Receiver};
    use std::time::{Duration, Instant};

    /// Collects everything written to it.
    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl Write for Sink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Sink {
        fn lines(&self) -> Vec<String> {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap().lines().map(str::to_owned).collect()
        }
    }

    /// A client that never reads: every write blocks until released.
    struct Stalled(Arc<Mutex<Receiver<()>>>);
    impl Write for Stalled {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            let _ = self.0.lock().unwrap().recv(); // blocks until the test drops the sender
            Err(std::io::Error::other(format!("released after {} bytes", b.len())))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A client whose pipe is already broken.
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn wait_until(mut f: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        f()
    }

    #[test]
    fn stalled_client_never_blocks_broadcast_and_drops_are_counted() {
        let (tx, _rx) = channel();
        let hub = PipeHub::detached(tx);
        let (release, gate) = channel::<()>();
        hub.add_client(Box::new(Stalled(Arc::new(Mutex::new(gate)))));
        let healthy = Sink::default();
        hub.add_client(Box::new(healthy.clone()));

        // Broadcast in half-queue batches, letting the healthy reader catch up
        // between batches (it always has room); the stalled one never drains.
        let n = CLIENT_QUEUE * 4;
        let batch = CLIENT_QUEUE / 2;
        let mut in_broadcast = Duration::ZERO;
        for start in (0..n).step_by(batch) {
            let t = Instant::now();
            for i in start..start + batch {
                hub.event("engine", &format!(r#"{{"type":"status","i":{i}}}"#));
            }
            in_broadcast += t.elapsed();
            assert!(wait_until(|| healthy.lines().len() == start + batch), "healthy client got {}", healthy.lines().len());
        }
        assert!(in_broadcast < Duration::from_secs(1), "broadcast blocked for {in_broadcast:?}");
        // The stalled client's writer holds one line in-flight plus a full queue;
        // everything past that was dropped (and counted), never waited on.
        assert!(hub.dropped() >= (n - CLIENT_QUEUE - 1) as u64, "dropped {}", hub.dropped());
        assert!(hub.dropped() <= (n - CLIENT_QUEUE) as u64, "only the stalled client dropped: {}", hub.dropped());
        assert_eq!(healthy.lines()[0], r#"{"mod":"engine","event":{"type":"status","i":0}}"#);
        assert_eq!(hub.client_count(), 2, "a stall is not a disconnect");
        drop(release); // let the stalled writer finish so its thread exits
    }

    #[test]
    fn broken_client_is_pruned_and_send_to_targets_one_client() {
        let (tx, _rx) = channel();
        let hub = PipeHub::detached(tx);
        hub.add_client(Box::new(Broken));
        let a = Sink::default();
        let b = Sink::default();
        let id_a = hub.add_client(Box::new(a.clone()));
        hub.add_client(Box::new(b.clone()));
        hub.event_to(id_a, "session", r#"{"type":"config"}"#);
        // The broken writer exits on its first write; later sends prune it.
        assert!(wait_until(|| {
            hub.broadcast("{}");
            hub.client_count() == 2
        }));
        assert!(wait_until(|| a.lines().first().map(String::as_str) == Some(r#"{"mod":"session","event":{"type":"config"}}"#)));
        assert!(b.lines().iter().all(|l| !l.contains("config")), "send_to reached only its client");
    }

    #[test]
    fn welcome_greets_and_requests_a_status_snapshot() {
        let (tx, rx) = channel();
        let hub = PipeHub::detached(tx);
        let sink = Sink::default();
        let id = hub.add_client(Box::new(sink.clone()));
        hub.welcome(id);
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), Inbound { client: id, line: r#"{"cmd":"status"}"#.into() });
        assert!(wait_until(|| sink.lines().iter().any(|l| l.contains(r#""mod":"hub""#) && l.contains("hello"))));
        // Inbound lines carry their client id, trimmed, blanks skipped.
        hub.read_client(id, &b"  {\"cmd\":\"quit\"}  \n\n"[..]);
        assert_eq!(rx.recv().unwrap(), Inbound { client: id, line: r#"{"cmd":"quit"}"#.into() });
        assert!(rx.try_recv().is_err());
        assert_eq!(hub.client_count(), 0, "EOF pruned the client");
    }

    /// G14: a client that writes 2 MiB with no newline is dropped; nothing reaches the pump.
    #[test]
    fn a_line_with_no_newline_past_the_cap_drops_the_client() {
        let (tx, rx) = channel();
        let hub = PipeHub::detached(tx);
        let id = hub.add_client(Box::new(Sink::default()));
        let flood = vec![b'x'; 2 << 20];
        hub.read_client(id, &flood[..]);
        assert!(rx.try_recv().is_err(), "the oversized line never reaches the pump");
        assert_eq!(hub.client_count(), 0, "the client was pruned");
    }

    /// A client that hangs up is pruned at EOF with no broadcast after it: an idle
    /// recorder (engine stopped) sends nothing, and dead clients piled up (935 on 11-05).
    #[test]
    fn a_client_that_hangs_up_is_pruned_without_any_broadcast() {
        let (tx, _rx) = channel();
        let hub = PipeHub::detached(tx);
        let stay = hub.add_client(Box::new(Sink::default()));
        for _ in 0..50 {
            let id = hub.add_client(Box::new(Sink::default()));
            hub.read_client(id, &b"{\"cmd\":\"status\"}\n"[..]);
        }
        assert_eq!(hub.client_count(), 1, "only the open client is left");
        hub.event_to(stay, "session", "{}");
        assert_eq!(hub.client_count(), 1);
    }
}
