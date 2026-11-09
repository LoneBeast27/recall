//! Which present providers the frame-timing session enables, and with which
//! filter. Pure: `etw_session` passes the real `EnableTraceEx2` calls in
//! through [`Enabler`], the tests pass a fake.
//!
//! The user-mode providers (DXGI, D3D9) are asked with a PID filter first,
//! every time the session is aimed at a game ([`enable_for`] at start,
//! [`retarget`] on a switch). A refused filter is retried unfiltered (the
//! event callback filters on the pid anyway), and the user-mode providers
//! enabled filtered before the refusal are re-enabled unfiltered, so a later
//! switch never leaves one of them on the old pid. Two kinds of refusal:
//!
//! - `ERROR_ACCESS_DENIED`: this game refuses the filter while the same
//!   session enables fine for every other game. Apex Legends (Easy
//!   Anti-Cheat) does this; it is not about the user's permission. The
//!   providers go unfiltered for this game only, and the next switch asks
//!   filtered again.
//! - any other error: the PID filter itself is not available (as before this
//!   module): unfiltered for the rest of the session.
//!
//! The kernel provider (DxgKrnl) is never PID-filtered.

use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SUCCESS, WIN32_ERROR};

use super::etw_session::StartError;
use super::present_events::{ProviderSpec, DXGI, DXGKRNL};

/// The session's provider calls.
pub trait Enabler {
    /// `EnableTraceEx2(ENABLE_PROVIDER)`, PID-filtered for `Some(pid)`.
    fn enable(&mut self, spec: &ProviderSpec, pid: Option<u32>) -> WIN32_ERROR;
    /// `EnableTraceEx2(DISABLE_PROVIDER)`.
    fn disable(&mut self, spec: &ProviderSpec);
}

/// How the session's providers ended up for one target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aimed {
    /// DXGI's final status (`ERROR_SUCCESS` when DXGI was not asked).
    pub dxgi: WIN32_ERROR,
    /// DxgKrnl's final status (`ERROR_SUCCESS` when DxgKrnl was not asked).
    pub kernel: WIN32_ERROR,
    /// This target refused the PID filter (access denied): the provider that
    /// said so. The user-mode providers run unfiltered for this target.
    pub refused_by: Option<&'static str>,
    /// The PID filter is available; false after any other refusal, for good.
    pub filter_ok: bool,
}

/// Enables `specs` (in order) for `pid`; PID-filtered where the spec allows
/// it, while `filter_ok`.
pub fn enable_for(specs: &[&ProviderSpec], pid: u32, filter_ok: bool, calls: &mut impl Enabler) -> Aimed {
    let mut aimed = Aimed { dxgi: ERROR_SUCCESS, kernel: ERROR_SUCCESS, refused_by: None, filter_ok };
    let mut status = vec![ERROR_SUCCESS; specs.len()];
    for (i, spec) in specs.iter().enumerate() {
        let filtered = spec.pid_filter && aimed.filter_ok && aimed.refused_by.is_none();
        let mut now = calls.enable(spec, filtered.then_some(pid));
        if now != ERROR_SUCCESS && filtered {
            if now == ERROR_ACCESS_DENIED {
                aimed.refused_by = Some(spec.name);
            } else {
                aimed.filter_ok = false;
            }
            for (earlier, was) in specs[..i].iter().zip(status.iter_mut()) {
                if earlier.pid_filter && *was == ERROR_SUCCESS {
                    calls.disable(earlier);
                    *was = calls.enable(earlier, None);
                }
            }
            now = calls.enable(spec, None);
        }
        status[i] = now;
    }
    for (spec, now) in specs.iter().zip(status) {
        if spec.guid == DXGI {
            aimed.dxgi = now;
        } else if spec.guid == DXGKRNL {
            aimed.kernel = now;
        }
    }
    aimed
}

/// Aims the user-mode providers of `specs` at a new `pid`: filtered again
/// (a refusal by the last game does not carry over), unless the filter is
/// not available at all (then nothing changes: unfiltered covers every pid).
/// `None` when nothing was re-enabled.
pub fn retarget(specs: &[&ProviderSpec], pid: u32, filter_ok: bool, calls: &mut impl Enabler) -> Option<Aimed> {
    if !filter_ok {
        return None;
    }
    let user: Vec<&ProviderSpec> = specs.iter().copied().filter(|spec| spec.pid_filter).collect();
    for spec in &user {
        calls.disable(spec);
    }
    Some(enable_for(&user, pid, filter_ok, calls))
}

/// The session counts frames when DXGI or the DxgKrnl fallback is on. With
/// neither, a refusal of DXGI (asked unfiltered by then) means no permission.
pub fn outcome(aimed: &Aimed) -> Result<(), StartError> {
    match (aimed.dxgi, aimed.kernel) {
        (ERROR_SUCCESS, _) | (_, ERROR_SUCCESS) => Ok(()),
        (ERROR_ACCESS_DENIED, _) => Err(StartError::Denied),
        (other, _) => Err(StartError::Failed(format!("EnableTraceEx2 DXGI: error {}", other.0))),
    }
}

/// The trace note's tail for how the user-mode providers run.
pub fn filter_note(aimed: &Aimed, forced_off: bool) -> String {
    match (aimed.refused_by, aimed.filter_ok) {
        _ if forced_off => " (unfiltered: RECALL_ETW_NO_PID_FILTER=1)".into(),
        (Some(name), _) => format!(" (unfiltered: {name} refused the PID filter for this game)"),
        (None, false) => " (unfiltered: no PID filter available)".into(),
        (None, true) => String::new(),
    }
}

#[cfg(test)]
#[path = "enable_plan_tests.rs"]
mod tests;
