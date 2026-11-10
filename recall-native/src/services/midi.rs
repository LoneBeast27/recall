//! MIDI console: port listing, test send, inbound monitor log on the input chosen in
//! Settings → General → MIDI device (`midi.input`; "" = off, nothing is opened).
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
    /// The configured input port name (never empty: an off setting spawns no monitor).
    wanted: String,
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
    fn new(wanted: String) -> Self {
        Self {
            wanted,
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
    /// Starts the inbound monitor on the input port named `port` (the setting). It opens
    /// that port only, never another device.
    pub fn spawn(port: String) -> Self {
        let shared = Arc::new(Shared::new(port));
        let worker = shared.clone();
        if let Err(error) = std::thread::Builder::new().name("midi-monitor".into()).spawn(move || monitor_loop(&worker)) {
            eprintln!("Recall native: MIDI monitor thread failed to start: {error}");
        }
        Self { shared }
    }
    /// The port this monitor was started for.
    pub fn wanted(&self) -> &str {
        &self.shared.wanted
    }
    pub fn ports(&self) -> MidiPorts {
        lock(&self.shared.ports).clone()
    }
    /// Name of the monitored input port, if found.
    pub fn monitored_port(&self) -> Option<String> {
        lock(&self.shared.monitored).clone()
    }
    /// Last 10 inbound messages, newest first.
    pub fn recent(&self) -> Vec<MidiMessage> {
        lock(&self.shared.recent).iter().cloned().collect()
    }
    pub fn revision(&self) -> u64 {
        self.shared.revision.load(Ordering::Acquire)
    }
}

/// Sends raw bytes to the first output whose name contains `port_match` (case-insensitive), on a
/// fresh connection. Blocks while the port opens: call it off the UI thread.
pub fn send_to(port_match: &str, bytes: &[u8]) -> Result<(), String> {
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

/// "Send test note": middle C on, 120 ms, off, to the output `port_match` names. Blocks for the
/// note and the two port opens: run it on a background task.
pub fn send_test_note(port_match: &str) -> Result<(), String> {
    send_to(port_match, &[0x90, 60, 127])?;
    std::thread::sleep(Duration::from_millis(120));
    send_to(port_match, &[0x80, 60, 0])
}

impl Drop for Midi {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
    }
}

/// The monitor input: the port named `wanted` (case-insensitive). Deliberately no fallback
/// to another device.
fn pick_monitor(inputs: &[String], wanted: &str) -> Option<usize> {
    let wanted = fold(wanted.trim());
    if wanted.is_empty() {
        return None;
    }
    inputs.iter().position(|name| fold(name.trim()) == wanted)
}

/// First output containing `port_match` (case-insensitive), never the GS Wavetable synth.
fn pick_output(outputs: &[String], port_match: &str) -> Option<usize> {
    let wanted = fold(port_match);
    outputs.iter().position(|name| {
        let folded = fold(name);
        !folded.contains("wavetable") && folded.contains(&wanted)
    })
}

/// Lowercase with ø→o, so a name typed without the ø still matches.
fn fold(text: &str) -> String {
    text.to_lowercase().chars().map(|c| if c == 'ø' { 'o' } else { c }).collect()
}

/// MIDI input port names, for the Settings picker (call off the UI thread).
pub fn input_names() -> Vec<String> {
    list_ports().inputs
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
/// chosen input when it is present and not yet open.
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
            open = pick_monitor(&ports.inputs, &shared.wanted).and_then(|index| connect_monitor(shared, &ports.inputs[index]));
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
    fn monitor_port_is_only_the_chosen_one() {
        let inputs = names(&["MIDI Controller 1", "Keyboard 2", "Keyboard"]);
        assert_eq!(pick_monitor(&inputs, "keyboard"), Some(2), "whole name, not a prefix");
        assert_eq!(pick_monitor(&inputs, " MIDI Controller 1 "), Some(0));
        assert_eq!(pick_monitor(&inputs, "Controller"), None, "never another device");
        assert_eq!(pick_monitor(&inputs, ""), None, "off");
        assert_eq!(pick_monitor(&[], "Keyboard"), None);
    }

    #[test]
    fn output_match_is_case_insensitive_and_skips_wavetable() {
        let outputs = names(&["Microsoft GS Wavetable Synth", "USB MIDI Interface", "MIDI Controller 1"]);
        assert_eq!(pick_output(&outputs, "midi"), Some(1));
        assert_eq!(pick_output(&outputs, "controller"), Some(2));
        assert_eq!(pick_output(&outputs, "MIDI CONTROLLER 1"), Some(2));
        assert_eq!(pick_output(&outputs, "wavetable"), None);
        assert_eq!(pick_output(&outputs, ""), Some(1), "empty match = first non-wavetable");
        assert_eq!(pick_output(&outputs, "keyboard"), None);
    }

    #[test]
    fn recent_keeps_ten_newest_first() {
        let shared = Shared::new("MIDI Controller 1".into());
        for i in 0..12u8 {
            shared.record("MIDI Controller 1", &[0x90, i, 0x7f]);
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
        let shared = Shared::new("MIDI Controller 1".into());
        let ports = MidiPorts { inputs: names(&["MIDI Controller 1"]), outputs: names(&["MIDI Controller 1"]) };
        shared.set_ports(ports.clone());
        shared.set_ports(ports);
        shared.set_monitored(Some("MIDI Controller 1".into()));
        shared.set_monitored(Some("MIDI Controller 1".into()));
        assert_eq!(shared.revision.load(Ordering::Acquire), 2);
    }

    #[test]
    #[ignore = "live hardware: lists MIDI ports and opens the first input (never sends)"]
    fn live_midi_lists_ports_and_monitors() {
        if !crate::hw_gate::enabled("live_midi_lists_ports_and_monitors") {
            return;
        }
        let midi = Midi::spawn(input_names().into_iter().next().unwrap_or_default());
        let deadline = Instant::now() + Duration::from_secs(5);
        while midi.revision() == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        eprintln!("ports {:?}\nmonitored {:?}", midi.ports(), midi.monitored_port());
        assert!(midi.revision() > 0, "monitor thread published ports");
    }

    #[test]
    fn empty_send_is_rejected_before_touching_devices() {
        assert!(send_to("MIDI Controller 1", &[]).is_err());
    }
}
