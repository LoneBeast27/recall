//! RØDECaster MIDI console: port listing, test send, inbound monitor log.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use midir::{MidiInput, MidiInputConnection, MidiOutput};

const RECENT_LIMIT: usize = 10;
const RETRY: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq)]
pub struct MidiMessage {
    /// Milliseconds since the monitor started.
    pub at_ms: u64,
    pub port: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MidiPorts {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

pub struct Midi {
    shared: Arc<Shared>,
}

struct Shared {
    started: Instant,
    ports: Mutex<MidiPorts>,
    monitored: Mutex<Option<String>>,
    recent: Mutex<VecDeque<MidiMessage>>,
    revision: AtomicU64,
    stop: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

impl Shared {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            ports: Mutex::new(MidiPorts::default()),
            monitored: Mutex::new(None),
            recent: Mutex::new(VecDeque::with_capacity(RECENT_LIMIT)),
            revision: AtomicU64::new(0),
            stop: AtomicBool::new(false),
        }
    }

    fn bump(&self) {
        self.revision.fetch_add(1, Ordering::AcqRel);
    }

    fn set_ports(&self, ports: MidiPorts) {
        let mut current = lock(&self.ports);
        if *current != ports {
            *current = ports;
            self.bump();
        }
    }

    fn set_monitored(&self, port: Option<String>) {
        let mut current = lock(&self.monitored);
        if *current != port {
            *current = port;
            self.bump();
        }
    }

    fn record(&self, port: &str, bytes: &[u8]) {
        let at_ms = self.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        push_recent(&mut lock(&self.recent), MidiMessage { at_ms, port: port.into(), bytes: bytes.to_vec() });
        self.bump();
    }
}

/// Newest first, capped at ten.
fn push_recent(recent: &mut VecDeque<MidiMessage>, message: MidiMessage) {
    recent.push_front(message);
    recent.truncate(RECENT_LIMIT);
}

impl Midi {
    /// Starts the inbound monitor on the RØDECaster port ("MIDI function" or a name
    /// containing rødecaster/rodecaster). No first-port fallback.
    pub fn spawn() -> Self {
        let shared = Arc::new(Shared::new());
        let worker = shared.clone();
        if let Err(error) = std::thread::Builder::new().name("midi-monitor".into()).spawn(move || monitor_loop(&worker)) {
            eprintln!("Recall native: MIDI monitor thread failed to start: {error}");
        }
        Self { shared }
    }
    pub fn ports(&self) -> MidiPorts {
        lock(&self.shared.ports).clone()
    }
    /// Name of the monitored input port, if found.
    pub fn monitored_port(&self) -> Option<String> {
        lock(&self.shared.monitored).clone()
    }
    /// Sends raw bytes to the first output whose name contains `port_match` (case-insensitive).
    /// Opens a fresh connection per call.
    pub fn send(&self, port_match: &str, bytes: &[u8]) -> Result<(), String> {
        if bytes.is_empty() {
            return Err("nothing to send".into());
        }
        let output = MidiOutput::new("recall-native-out").map_err(|error| error.to_string())?;
        let ports = output.ports();
        let names: Vec<String> = ports.iter().map(|port| output.port_name(port).unwrap_or_default()).collect();
        let index = pick_output(&names, port_match).ok_or_else(|| format!("no MIDI output matching '{port_match}'"))?;
        let mut connection = output.connect(&ports[index], "recall-native").map_err(|error| format!("open {}: {error}", names[index]))?;
        let sent = connection.send(bytes).map_err(|error| format!("send to {}: {error}", names[index]));
        connection.close();
        sent
    }
    /// Last 10 inbound messages, newest first.
    pub fn recent(&self) -> Vec<MidiMessage> {
        lock(&self.shared.recent).iter().cloned().collect()
    }
    pub fn revision(&self) -> u64 {
        self.shared.revision.load(Ordering::Acquire)
    }
}

impl Drop for Midi {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
    }
}

/// The monitor input: "MIDI function" (how the RØDECaster enumerates) or any name
/// containing rødecaster/rodecaster. Deliberately no fallback to another device.
fn pick_monitor(inputs: &[String]) -> Option<usize> {
    inputs.iter().position(|name| {
        let folded = fold(name);
        folded.contains("midi function") || folded.contains("rodecaster")
    })
}

/// First output containing `port_match` (case-insensitive), never the GS Wavetable synth.
fn pick_output(outputs: &[String], port_match: &str) -> Option<usize> {
    let wanted = fold(port_match);
    outputs.iter().position(|name| {
        let folded = fold(name);
        !folded.contains("wavetable") && folded.contains(&wanted)
    })
}

/// Lowercase with ø→o, so "RØDECaster" and "RODECaster" compare equal.
fn fold(text: &str) -> String {
    text.to_lowercase().chars().map(|c| if c == 'ø' { 'o' } else { c }).collect()
}

fn list_ports() -> MidiPorts {
    let inputs = MidiInput::new("recall-native-list-in")
        .map(|midi| midi.ports().iter().filter_map(|port| midi.port_name(port).ok()).collect())
        .unwrap_or_default();
    let outputs = MidiOutput::new("recall-native-list-out")
        .map(|midi| midi.ports().iter().filter_map(|port| midi.port_name(port).ok()).collect())
        .unwrap_or_default();
    MidiPorts { inputs, outputs }
}

/// Every 3 s: refresh the port list, drop a vanished monitor port, (re)open the
/// RØDECaster input when it is present and not yet open.
fn monitor_loop(shared: &Arc<Shared>) {
    let mut open: Option<(String, MidiInputConnection<()>)> = None;
    while !shared.stop.load(Ordering::Acquire) {
        let ports = list_ports();
        if open.as_ref().is_some_and(|(name, _)| !ports.inputs.contains(name)) {
            if let Some((_, connection)) = open.take() {
                connection.close();
            }
            shared.set_monitored(None);
        }
        if open.is_none() {
            open = pick_monitor(&ports.inputs).and_then(|index| connect_monitor(shared, &ports.inputs[index]));
            shared.set_monitored(open.as_ref().map(|(name, _)| name.clone()));
        }
        shared.set_ports(ports);
        let resume = Instant::now() + RETRY;
        while Instant::now() < resume && !shared.stop.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    if let Some((_, connection)) = open {
        connection.close();
    }
}

fn connect_monitor(shared: &Arc<Shared>, name: &str) -> Option<(String, MidiInputConnection<()>)> {
    let input = MidiInput::new("recall-native-in").ok()?;
    let port = input.ports().into_iter().find(|port| input.port_name(port).is_ok_and(|n| n == name))?;
    let sink = shared.clone();
    let label = name.to_owned();
    let connection = input.connect(&port, "recall-native-monitor", move |_, bytes, _| sink.record(&label, bytes), ()).ok()?;
    Some((name.to_owned(), connection))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn monitor_port_prefers_rodecaster_names_and_never_falls_back() {
        assert_eq!(pick_monitor(&names(&["Launchpad", "MIDI function"])), Some(1));
        assert_eq!(pick_monitor(&names(&["midi FUNCTION 2"])), Some(0));
        assert_eq!(pick_monitor(&names(&["Keyboard", "RØDECaster Pro II"])), Some(1));
        assert_eq!(pick_monitor(&names(&["RODECaster Duo"])), Some(0));
        assert_eq!(pick_monitor(&names(&["Launchpad", "Keyboard"])), None);
        assert_eq!(pick_monitor(&[]), None);
    }

    #[test]
    fn output_match_is_case_insensitive_and_skips_wavetable() {
        let outputs = names(&["Microsoft GS Wavetable Synth", "MIDI function", "RØDECaster"]);
        assert_eq!(pick_output(&outputs, "midi"), Some(1));
        assert_eq!(pick_output(&outputs, "rodecaster"), Some(2));
        assert_eq!(pick_output(&outputs, "RØDECASTER"), Some(2));
        assert_eq!(pick_output(&outputs, "wavetable"), None);
        assert_eq!(pick_output(&outputs, ""), Some(1), "empty match = first non-wavetable");
        assert_eq!(pick_output(&outputs, "launchpad"), None);
    }

    #[test]
    fn recent_keeps_ten_newest_first() {
        let shared = Shared::new();
        for i in 0..12u8 {
            shared.record("MIDI function", &[0x90, i, 0x7f]);
        }
        let recent: Vec<_> = lock(&shared.recent).iter().cloned().collect();
        assert_eq!(recent.len(), 10);
        assert_eq!(recent[0].bytes, vec![0x90, 11, 0x7f]);
        assert_eq!(recent[9].bytes, vec![0x90, 2, 0x7f]);
        assert!(recent.windows(2).all(|pair| pair[0].at_ms >= pair[1].at_ms));
        assert_eq!(shared.revision.load(Ordering::Acquire), 12);
    }

    #[test]
    fn port_and_monitor_changes_bump_revision_once() {
        let shared = Shared::new();
        let ports = MidiPorts { inputs: names(&["MIDI function"]), outputs: names(&["MIDI function"]) };
        shared.set_ports(ports.clone());
        shared.set_ports(ports);
        shared.set_monitored(Some("MIDI function".into()));
        shared.set_monitored(Some("MIDI function".into()));
        assert_eq!(shared.revision.load(Ordering::Acquire), 2);
    }

    #[test]
    #[ignore = "live hardware: lists MIDI ports and opens the RØDECaster input (never sends)"]
    fn live_midi_lists_ports_and_monitors() {
        let midi = Midi::spawn();
        let deadline = Instant::now() + Duration::from_secs(5);
        while midi.revision() == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        eprintln!("ports {:?}\nmonitored {:?}", midi.ports(), midi.monitored_port());
        assert!(midi.revision() > 0, "monitor thread published ports");
    }

    #[test]
    fn empty_send_is_rejected_before_touching_devices() {
        let midi = Midi { shared: Arc::new(Shared::new()) };
        assert!(midi.send("midi function", &[]).is_err());
    }
}
