use std::process;

use windows::Win32::System::Diagnostics::Etw::EVENT_TRACE_CONTROL_QUERY;

use super::*;

/// Session `name` runs (any process's).
fn exists(name: &str) -> bool {
    let mut p = Properties::new();
    let name = wide(name);
    let none = CONTROLTRACE_HANDLE { Value: 0 };
    unsafe { ControlTraceW(none, PCWSTR(name.as_ptr()), &mut p.props, EVENT_TRACE_CONTROL_QUERY) == ERROR_SUCCESS }
}

/// Leaves a session `name` running with no owner lock: what a killed
/// overlay leaves behind. False when this account may not start sessions.
fn start_stale(name: &str) -> bool {
    let mut p = Properties::new();
    let mut control = CONTROLTRACE_HANDLE { Value: 0 };
    start_trace(&wide(name), &mut p, &mut control) == ERROR_SUCCESS
}

fn tagged(test: &str) -> String {
    session_name(Kind::Overlay, &format!("test-{}-{test}", process::id()))
}

#[test]
fn names_follow_the_instance_tag() {
    assert_eq!(session_name(Kind::Overlay, ""), "RecallFrameTiming", "recall-hdr stops it by this name");
    assert_eq!(session_name(Kind::Overlay, "gate1"), "RecallFrameTiming.gate1");
    assert_eq!(session_name(Kind::Probe, ""), "RecallFrameTimingProbe");
    assert_eq!(session_name(Kind::Probe, "gate1"), "RecallFrameTimingProbe.gate1");
    assert!(session_name(Kind::Probe, &"x".repeat(200)).len() < 64, "fits the name buffer");
    assert_eq!(owner_lock("RecallFrameTiming.gate1"), "Global\\RecallFrameTiming.gate1.owner");
}

#[test]
fn a_tagged_overlay_replaces_its_own_stale_session_and_never_the_plain_one() {
    let name = tagged("stale");
    if !start_stale(&name) {
        eprintln!("skipped: this account may not start ETW sessions");
        return;
    }
    let plain = session_name(Kind::Overlay, "");
    let plain_before = exists(&plain);
    let session = EtwSession::start_named(process::id(), name.clone()).expect("a dead owner's session is replaced");
    assert!(exists(&name));
    if plain_before {
        assert!(exists(&plain), "the installed overlay's session is never stopped");
    }
    drop(session);
    assert!(!exists(&name), "stopped with the session");
}

#[test]
fn a_session_with_a_live_owner_is_left_alone() {
    let name = tagged("owned");
    if !start_stale(&name) {
        eprintln!("skipped: this account may not start ETW sessions");
        return;
    }
    // A live owner (another user's lock answers access denied: `Claim::Taken` too).
    let Claim::First(live) = single_instance::claim(&owner_lock(&name)) else { panic!("a fresh lock") };
    let Err(StartError::Failed(why)) = EtwSession::start_named(process::id(), name.clone()) else {
        panic!("must back off while the owner lives");
    };
    assert!(why.contains("live owner"), "{why}");
    assert!(exists(&name), "the owner's session keeps running");
    drop(live);
    let _ = stop_by_name(&name);
    assert!(!exists(&name));
}
