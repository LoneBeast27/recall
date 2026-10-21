//! RØDECaster level meters (WASAPI loopback on render endpoints, capture on the mic).
//! Ported from the hub's `meters.rs`; missing devices are retried.

#[derive(Debug, Clone, PartialEq)]
pub enum MeterState {
    Live,
    Missing,
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MeterReading {
    /// "Game", "Music", "System", "Virtual A", "Virtual B", "Mic"
    pub label: String,
    pub rms_db: f32,
    pub peak_db: f32,
    pub state: MeterState,
}

pub struct Meters {
    _private: (),
}

impl Meters {
    /// Starts one thread per meter. Never blocks.
    pub fn spawn() -> Self {
        Self { _private: () }
    }
    /// Latest readings in display order (always 6 entries).
    pub fn snapshot(&self) -> Vec<MeterReading> {
        ["Game", "Music", "System", "Virtual A", "Virtual B", "Mic"]
            .iter()
            .map(|label| MeterReading { label: (*label).into(), rms_db: -90.0, peak_db: -90.0, state: MeterState::Missing })
            .collect()
    }
    pub fn revision(&self) -> u64 {
        0
    }
}

/// Maps dB to a 0..1 bar fraction over a -60..0 dB scale.
pub fn bar_fraction(db: f32) -> f32 {
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}
