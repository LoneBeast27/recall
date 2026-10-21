//! RØDECaster MIDI console: port listing, test send, inbound monitor log.

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
    _private: (),
}

impl Midi {
    /// Starts the inbound monitor on the RØDECaster port ("MIDI function" or a name
    /// containing rødecaster/rodecaster). No first-port fallback.
    pub fn spawn() -> Self {
        Self { _private: () }
    }
    pub fn ports(&self) -> MidiPorts {
        MidiPorts::default()
    }
    /// Name of the monitored input port, if found.
    pub fn monitored_port(&self) -> Option<String> {
        None
    }
    /// Sends raw bytes to the first output whose name contains `port_match` (case-insensitive).
    /// Opens a fresh connection per call.
    pub fn send(&self, _port_match: &str, _bytes: &[u8]) -> Result<(), String> {
        Err("MIDI not implemented".into())
    }
    /// Last 10 inbound messages, newest first.
    pub fn recent(&self) -> Vec<MidiMessage> {
        Vec::new()
    }
    pub fn revision(&self) -> u64 {
        0
    }
}
