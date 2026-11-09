//! Tests that open real audio or MIDI devices, spawn the real engine (which opens the
//! microphone), read or change real displays (HDR, SDR white), capture the screen, or run the
//! render probe run only with RECALL_HW_TESTS=1 (P2-d): `--include-ignored` alone never
//! touches hardware. Each such test starts with `if !crate::hw_gate::enabled("<name>") { return; }`.

/// The env var that lets hardware tests run.
pub(crate) const ENV: &str = "RECALL_HW_TESTS";

/// True only when RECALL_HW_TESTS=1; otherwise prints that `test` was skipped.
pub(crate) fn enabled(test: &str) -> bool {
    let on = std::env::var(ENV).is_ok_and(|v| v.trim() == "1");
    if !on {
        eprintln!("skipped {test}: opens real hardware; set {ENV}=1 to run it");
    }
    on
}
