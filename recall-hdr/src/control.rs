//! Control IPC for the Recall Hub: a named-pipe server
//! (`\\.\pipe\recall-hdr-control`) speaking newline-JSON, mirroring the engine
//! protocol's shape. The daemon BROADCASTS namespaced events
//! (`{"mod":"engine"|"hdr"|"hub","event":{...}}`) to every connected client and
//! forwards each inbound line to the pump thread (commands never execute on
//! pipe threads — display state stays single-threaded).

use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use interprocess::local_socket::traits::{Listener, Stream as _};
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, SendHalf, Stream, ToNsName};

use crate::log;

pub const PIPE_NAME: &str = "recall-hdr-control";

/// Shared broadcaster + inbound command forwarder.
pub struct PipeHub {
    clients: Mutex<Vec<SendHalf>>,
    commands: Sender<String>,
}

impl PipeHub {
    /// Start the accept loop on its own thread. `commands` receives every
    /// inbound line (raw JSON) for the pump to drain.
    pub fn start(commands: Sender<String>) -> Arc<PipeHub> {
        let hub = Arc::new(PipeHub {
            clients: Mutex::new(Vec::new()),
            commands,
        });
        let accept_hub = hub.clone();
        std::thread::Builder::new()
            .name("control-pipe-accept".into())
            .spawn(move || accept_loop(accept_hub))
            .expect("spawn control pipe thread");
        hub
    }

    /// Send one JSON line to every connected client; prunes dead ones.
    pub fn broadcast(&self, line: &str) {
        let Ok(mut clients) = self.clients.lock() else { return };
        clients.retain_mut(|c| {
            c.write_all(line.as_bytes())
                .and_then(|_| c.write_all(b"\n"))
                .is_ok()
        });
    }

    /// Broadcast an event under a module namespace. `event_json` must be a
    /// complete JSON object (string-concatenated to avoid a parse round-trip).
    pub fn event(&self, module: &str, event_json: &str) {
        self.broadcast(&format!(r#"{{"mod":"{module}","event":{event_json}}}"#));
    }
}

fn accept_loop(hub: Arc<PipeHub>) {
    let name = match PIPE_NAME.to_ns_name::<GenericNamespaced>() {
        Ok(n) => n,
        Err(e) => {
            log!("control: pipe name invalid ({e}); control IPC disabled");
            return;
        }
    };
    let listener = match ListenerOptions::new().name(name).create_sync() {
        Ok(l) => l,
        Err(e) => {
            log!("control: pipe listen failed ({e}); control IPC disabled");
            return;
        }
    };
    log!("control: pipe listening (\\\\.\\pipe\\{PIPE_NAME})");

    loop {
        let stream: Stream = match listener.accept() {
            Ok(s) => s,
            Err(_) => continue,
        };
        let (reader, writer) = stream.split();
        {
            let Ok(mut clients) = hub.clients.lock() else { continue };
            clients.push(writer);
            log!("control: client connected ({} total)", clients.len());
        }
        hub.event(
            "hub",
            &format!(
                r#"{{"type":"hello","daemon":"recall-hdr","version":"{}"}}"#,
                env!("CARGO_PKG_VERSION")
            ),
        );

        let cmd_tx = hub.commands.clone();
        std::thread::Builder::new()
            .name("control-pipe-client".into())
            .spawn(move || {
                let reader = BufReader::new(reader);
                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    let line = line.trim().to_string();
                    if !line.is_empty() && cmd_tx.send(line).is_err() {
                        break;
                    }
                }
                // Writer half is pruned by the next broadcast after EOF.
            })
            .ok();
    }
}
