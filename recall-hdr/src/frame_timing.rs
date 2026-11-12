//! The stats island's FPS source as the controller reports it (status
//! `fps_source`). FPS + 1% low come from Windows frame timing: the overlay
//! runs a real-time ETW session (`RecallFrameTiming`) on the D3D runtime's
//! and graphics kernel's own present events, the method Intel PresentMon and
//! NVIDIA FrameView use: no hooks, nothing injected into the game.
//!
//! Starting that session needs an elevated administrator or membership in
//! Performance Log Users (S-1-5-32-559, added once by `scripts/install.ps1`;
//! effective from the next sign-in). The overlay inherits this process's
//! token, so the controller's own token answers whether it will work.
//!
//! The controller also stops the session by name whenever its overlay is
//! killed or dies (a killed process cannot stop its own session).

use windows::core::{BOOL, PCWSTR};
use windows::Win32::Security::{
    CheckTokenMembership, CreateWellKnownSid, WinBuiltinAdministratorsSid, WinBuiltinPerfLoggingUsersSid, PSID,
    SECURITY_MAX_SID_SIZE, WELL_KNOWN_SID_TYPE,
};
use windows::Win32::System::Diagnostics::Etw::{
    ControlTraceW, CONTROLTRACE_HANDLE, EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_PROPERTIES, WNODE_FLAG_TRACED_GUID,
};

/// Must match `recall-overlay/src/stats/etw_session.rs` (`session_name`).
const SESSION_BASE: &str = "RecallFrameTiming";

/// The status `fps_source` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FpsSource {
    /// Stats are off.
    Off,
    /// Stats on and the token may start the session.
    Etw,
    /// Stats on, but neither an elevated administrator nor in Performance Log
    /// Users: FPS shows "—"; Settings offers "Set up FPS".
    NoPermission,
}

impl FpsSource {
    pub fn as_str(self) -> &'static str {
        match self {
            FpsSource::Off => "off",
            FpsSource::Etw => "etw",
            FpsSource::NoPermission => "no_permission",
        }
    }
}

pub fn fps_source(stats_on: bool, can_trace: bool) -> FpsSource {
    match (stats_on, can_trace) {
        (false, _) => FpsSource::Off,
        (true, true) => FpsSource::Etw,
        (true, false) => FpsSource::NoPermission,
    }
}

/// This process's token may start an ETW session: Performance Log Users or
/// an (enabled, i.e. elevated) Administrators group. A filtered admin token
/// has Administrators deny-only, which `CheckTokenMembership` reports as no.
pub fn can_trace() -> bool {
    in_group(WinBuiltinPerfLoggingUsersSid) || in_group(WinBuiltinAdministratorsSid)
}

fn in_group(kind: WELL_KNOWN_SID_TYPE) -> bool {
    let mut sid = [0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut size = SECURITY_MAX_SID_SIZE;
    let psid = PSID(sid.as_mut_ptr().cast());
    let mut member = BOOL(0);
    unsafe {
        CreateWellKnownSid(kind, None, Some(psid), &mut size).is_ok()
            && CheckTokenMembership(None, psid, &mut member).is_ok()
            && member.as_bool()
    }
}

/// `EVENT_TRACE_PROPERTIES` with room for the session name.
#[repr(C)]
struct Properties {
    props: EVENT_TRACE_PROPERTIES,
    name: [u16; 64],
}

/// The overlay's session name: `RecallFrameTiming`, or
/// `RecallFrameTiming.<tag>` under `RECALL_INSTANCE_TAG` (the overlay inherits
/// this process's environment), so a tagged controller never stops the
/// installed overlay's session. Same rule as the overlay's `session_name`.
fn session_name(tag: &str) -> String {
    let tag: String = tag.chars().take(32).collect();
    if tag.is_empty() {
        SESSION_BASE.to_owned()
    } else {
        format!("{SESSION_BASE}.{tag}")
    }
}

/// Stop the overlay's frame-timing session; harmless when none runs.
pub fn stop_session() {
    let name: Vec<u16> = session_name(&crate::single_instance::tag()).encode_utf16().chain([0]).collect();
    let mut p = Box::new(Properties { props: EVENT_TRACE_PROPERTIES::default(), name: [0; 64] });
    p.props.Wnode.BufferSize = std::mem::size_of::<Properties>() as u32;
    p.props.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
    p.props.LoggerNameOffset = std::mem::offset_of!(Properties, name) as u32;
    let none = CONTROLTRACE_HANDLE { Value: 0 };
    let status = unsafe { ControlTraceW(none, PCWSTR(name.as_ptr()), &mut p.props, EVENT_TRACE_CONTROL_STOP) };
    if status.0 == 0 {
        crate::log!("frame timing: stopped the overlay's ETW session");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_field_follows_the_toggle_and_the_permission() {
        assert_eq!(fps_source(false, true).as_str(), "off");
        assert_eq!(fps_source(false, false).as_str(), "off", "stats off: no permission line either");
        assert_eq!(fps_source(true, true).as_str(), "etw");
        assert_eq!(fps_source(true, false).as_str(), "no_permission");
    }

    #[test]
    fn the_token_check_is_read_only_and_the_name_matches_the_overlay() {
        // Whatever this machine answers, asking must not fail. (No stop here:
        // it would end a live overlay's session.)
        let _ = can_trace();
        assert_eq!(session_name(""), "RecallFrameTiming");
        assert_eq!(session_name("gate1"), "RecallFrameTiming.gate1", "a tagged controller stops only its own overlay's");
    }
}
