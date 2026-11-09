use super::*;

/// Pure conversion: level (1000 == 80 nits) -> nits, matching `get_sdr_white`.
fn level_to_nits(level: u32) -> u32 {
    level * 80 / SDR_WHITE_UNITS_PER_80_NITS
}
/// Inverse, matching `set_sdr_white`.
fn nits_to_level(nits: u32) -> u32 {
    nits * SDR_WHITE_UNITS_PER_80_NITS / 80
}

#[test]
fn nits_conversion_known_points() {
    // level (1000==80 nits) -> nits. 2500=200 / 3125=250 = the ABL-sane band.
    assert_eq!(level_to_nits(1000), 80);
    assert_eq!(level_to_nits(2500), 200);
    assert_eq!(level_to_nits(3125), 250);
    assert_eq!(level_to_nits(6000), 480); // panel peak-ish
}

#[test]
fn nits_roundtrip_is_stable_on_multiples_of_80() {
    for nits in [80u32, 160, 200, 240, 320, 400, 480] {
        assert_eq!(level_to_nits(nits_to_level(nits)), nits, "nits={nits}");
    }
}

#[test]
fn hand_rolled_type_constants() {
    // Guard the hand-rolled values against accidental edits (sources in the
    // module doc): 24H2 GET_ACI2=15, SET_HDR_STATE=16, undocumented white=0xEE.
    assert_eq!(GET_ADVANCED_COLOR_INFO_2.0, 15);
    assert_eq!(SET_HDR_STATE.0, 16);
    assert_eq!(SET_SDR_WHITE_LEVEL.0 as u32, 0xFFFF_FFEE);
}

// --- Live tests (authorized display-state testing; run explicitly) ---------
// Run: cargo test -p recall-hdr --bins -- --ignored --nocapture display::tests
// (this crate is bin-only, so `--bins` not `--lib`.)

const MANAGED: &str = ""; // the default: the primary display

#[test]
#[ignore = "live: touches real displays"]
fn live_enumerate() {
    if !crate::hw_gate::enabled("live_enumerate") {
        return;
    }
    let all = enumerate_targets().expect("enumerate_targets");
    println!("-- active displays --");
    for (name, t) in &all {
        println!(
            "  '{name}'  adapter=({},{}) id={}",
            t.adapter.LowPart, t.adapter.HighPart, t.id
        );
    }
    assert!(!all.is_empty(), "expected at least one active display");

    let managed = resolve_target(MANAGED).expect("resolve the primary display");
    println!("resolved managed target: '{}'", managed.name());
    assert!(all.iter().any(|(_, t)| *t == managed), "the primary is one of the active displays");

    // Every other display is a DIFFERENT target -- we must never touch it.
    let others: Vec<_> = all.iter().filter(|(_, t)| *t != managed).collect();
    for (n, t) in &others {
        println!("other (never touch): '{n}' id={}", t.id);
    }
}

/// Restores HDR (and SDR-white, if captured) to their originals on drop --
/// even if the test panics mid-assert. This is the mandatory reset guarantee
/// for live state-mutating tests.
struct Restore<'a> {
    t: &'a TargetId,
    hdr: bool,
    nits: Option<u32>,
}
impl Drop for Restore<'_> {
    fn drop(&mut self) {
        // White level first (may need HDR still on), then HDR.
        if let Some(n) = self.nits {
            let _ = set_sdr_white(self.t, n);
        }
        let _ = set_hdr_state(self.t, self.hdr);
        let now = get_hdr_state(self.t);
        println!(
            "[restore guard] hdr->{} (now {now:?}) white->{:?}",
            self.hdr, self.nits
        );
    }
}

#[test]
#[ignore = "live: flips HDR twice (two screen flashes, authorized)"]
fn live_hdr_roundtrip() {
    if !crate::hw_gate::enabled("live_hdr_roundtrip") {
        return;
    }
    let t = resolve_target(MANAGED).expect("resolve the primary display");
    let original = get_hdr_state(&t).expect("initial GET hdr");
    println!("HDR original = {original}");
    let _guard = Restore {
        t: &t,
        hdr: original,
        nits: None,
    };

    let target = !original;
    set_hdr_state(&t, target).expect("SET hdr to !original");
    std::thread::sleep(std::time::Duration::from_secs(2));
    let mid = get_hdr_state(&t).expect("GET hdr mid");
    println!("HDR after flip = {mid} (wanted {target})");
    assert_eq!(mid, target, "HDR did not flip to the requested state");

    // Explicit restore (the guard is the safety net, not the happy path).
    set_hdr_state(&t, original).expect("SET hdr back to original");
    let after = get_hdr_state(&t).expect("GET hdr after restore");
    println!("HDR after restore = {after} (wanted {original})");
    assert_eq!(after, original, "HDR was not restored to original");
}

#[test]
#[ignore = "live: reads real SDR white level"]
fn live_sdr_white_get() {
    if !crate::hw_gate::enabled("live_sdr_white_get") {
        return;
    }
    let t = resolve_target(MANAGED).expect("resolve the primary display");
    let nits = get_sdr_white(&t).expect("GET sdr white");
    println!("SDR white level = {nits} nits");
    assert!(
        (80..=480).contains(&nits),
        "SDR white {nits} outside 80..=480"
    );
}

/// Instrument-validity check for the UNDOCUMENTED `SET_SDR_WHITE_LEVEL`
/// (0xFFFFFFEE): enable HDR, set a different white value, GET-verify it took
/// (not just "returned Ok"); the guard restores white + HDR on any exit.
#[test]
#[ignore = "live: enables HDR + writes SDR white (screen flash, authorized)"]
fn live_sdr_white_set_roundtrip() {
    if !crate::hw_gate::enabled("live_sdr_white_set_roundtrip") {
        return;
    }
    let t = resolve_target(MANAGED).expect("resolve the primary display");
    let orig_hdr = get_hdr_state(&t).expect("GET hdr");
    // White level is only meaningful/settable while HDR is on.
    set_hdr_state(&t, true).expect("enable HDR for white-level test");
    std::thread::sleep(std::time::Duration::from_millis(500));
    let orig_nits = get_sdr_white(&t).expect("GET original white");
    println!("original: hdr={orig_hdr} white={orig_nits} nits");
    let _guard = Restore {
        t: &t,
        hdr: orig_hdr,
        nits: Some(orig_nits),
    };

    let want = if orig_nits == 200 { 240 } else { 200 }; // definitely different, ABL-sane
    set_sdr_white(&t, want).expect("SET sdr white (undocumented path)");
    std::thread::sleep(std::time::Duration::from_millis(300));
    let got = get_sdr_white(&t).expect("GET white after set");
    println!("set {want} nits -> GET reads {got} nits");
    assert!(
        (got as i64 - want as i64).abs() <= 1, // 1 quantization unit slack
        "SET_SDR_WHITE_LEVEL did not take: wanted {want}, got {got}"
    );
}
