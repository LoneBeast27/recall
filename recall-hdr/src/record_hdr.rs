//! Optional HDR management inside the unified record mode.
//!
//! The record mode never changes a display setting unless
//! `behavior.hdr_auto = true`. Then this module runs the legacy machinery
//! unchanged -- `Reconciler` + journalled [`HdrExecutor`] + the restore spine +
//! startup repair -- fed by the same foreground watcher as the capture tracker.
//!
//! - The managed monitor is resolved read-only at startup. Absent -> HDR is
//!   reported unavailable and every HDR command is refused; recording is
//!   unaffected.
//! - Activation is lazy (startup with `hdr_auto`, or `set_config` turning it
//!   on). The ctrl-handler half of the spine can be installed once per
//!   process, so the spine is built on first activation and kept.
//! - Turning `hdr_auto` off restores the journalled baseline (a repeatable
//!   restore, like the legacy tray's RestoreNow) and stops acting.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::display::{self, TargetId};
use crate::guard::{OnceRestore, RestoreGuard};
use crate::hdr_exec::{self, HdrExecutor};
use crate::liveness::ProcessWatch;
use crate::log;
use crate::rules;
use crate::system_notice::{self, Origin, Step};
use crate::state::Reconciler;
use crate::watcher::WatchEvent;

/// HDR state for the session status `hdr` object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HdrSnapshot {
    pub available: bool,
    pub display: Option<String>,
    pub hdr_on: bool,
    pub manual_pin: bool,
    pub auto: bool,
    pub session_pid: Option<u32>,
}

struct Active {
    exec: HdrExecutor,
    reconciler: Reconciler,
    liveness: ProcessWatch,
}

pub struct HdrManager {
    /// The managed display, resolved read-only at startup.
    target: Option<TargetId>,
    active: Option<Active>,
    /// Last read-only HDR probe while inactive.
    live_hdr: bool,
    spine: Option<(RestoreGuard, Arc<OnceRestore>)>,
    /// The per-machine `system_notice` marker (`None`: shelved, or no LOCALAPPDATA).
    ack_file: Option<std::path::PathBuf>,
    /// An automatic start waiting for the first-change notice, since when.
    notice_due: Option<Instant>,
}

impl HdrManager {
    /// Resolve the managed display (read-only; no display change). While HDR is shelved
    /// ([`crate::hdr_gate`]) the monitor is not even probed: HDR reads as unavailable,
    /// so every HDR command is refused and the 5 s live refresh does nothing.
    pub fn probe(cfg: &Config) -> Self {
        if !crate::hdr_gate::enabled() {
            // An older build may have left the display switched; put it back once.
            hdr_exec::repair_if_dirty(cfg);
            return Self::shelved();
        }
        let target = match display::resolve_target(&cfg.display.target_friendly_name) {
            Ok(t) => {
                log!("record-hdr: managed display {} found", t.name());
                Some(t)
            }
            Err(e) => {
                log!("record-hdr: managed display '{}' not found ({e}); HDR features unavailable", cfg.display.target_friendly_name);
                None
            }
        };
        let live_hdr = target.as_ref().and_then(|t| display::get_hdr_state(t).ok()).unwrap_or(false);
        HdrManager { target, active: None, live_hdr, spine: None, ack_file: system_notice::ack_file(), notice_due: None }
    }

    /// The manager while HDR is shelved: no managed display, never activates.
    pub(crate) fn shelved() -> Self {
        HdrManager { target: None, active: None, live_hdr: false, spine: None, ack_file: None, notice_due: None }
    }

    pub fn available(&self) -> bool {
        self.target.is_some()
    }

    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// Start managing HDR (journal repair, spine, reconciler). Idempotent. Before the first
    /// display change on this machine an automatic start only marks the notice due
    /// ([`system_notice`]); the controller shows it, then calls [`notice_shown`](Self::notice_shown).
    pub fn activate(&mut self, cfg: &Config, origin: Origin) -> Result<(), String> {
        if self.active.is_some() {
            return Ok(());
        }
        if !crate::hdr_gate::enabled() {
            return Err(crate::hdr_gate::SHELVED_MSG.into());
        }
        let Some(target) = self.target.clone() else {
            return Err(format!("managed display '{}' not found", cfg.display.target_friendly_name));
        };
        let acknowledged = self.ack_file.as_deref().is_some_and(system_notice::acknowledged);
        match system_notice::decide(acknowledged, origin) {
            Step::Proceed => {}
            Step::RememberAndProceed => self.remember(),
            Step::NoticeFirst => {
                self.notice_due.get_or_insert_with(Instant::now);
                log!("record-hdr: the first display change on this machine waits for its notice");
                return Ok(());
            }
        }
        self.notice_due = None;
        hdr_exec::repair_if_dirty(cfg);
        if self.spine.is_none() {
            self.spine = Some(hdr_exec::install_restore_spine(cfg)?);
        }
        self.active = Some(Active {
            exec: HdrExecutor::new(target),
            reconciler: Reconciler::new(Duration::from_millis(cfg.behavior.debounce_ms as u64), cfg.behavior.sdr_white_nits),
            liveness: ProcessWatch::new("record-hdr: session"),
        });
        log!("record-hdr: HDR management ON");
        Ok(())
    }

    fn remember(&self) {
        if let Some(file) = &self.ack_file {
            if let Err(e) = system_notice::remember(file) {
                log!("record-hdr: could not record the display-change notice: {e}");
            }
        }
    }

    /// The first-change notice to show now (title, detail), if one is due and can be seen.
    pub fn notice_ready(&self, now: Instant, viewers: usize) -> Option<(&'static str, String)> {
        self.notice_due.filter(|&due| system_notice::notice_ready(due, now, viewers))?;
        let display = self.target.as_ref().map_or("your display", |t| t.name());
        Some((system_notice::TITLE, system_notice::detail(display)))
    }

    /// The notice was shown: remember it for this machine and start managing HDR.
    pub fn notice_shown(&mut self, cfg: &Config) -> Result<(), String> {
        self.remember();
        self.notice_due = None;
        self.activate(cfg, Origin::UserAction)
    }

    /// Stop managing HDR and put the display back to the journalled baseline.
    pub fn deactivate(&mut self, cfg: &Config) {
        self.notice_due = None;
        if self.active.take().is_some() {
            hdr_exec::restore_now(cfg);
            self.refresh_live();
            log!("record-hdr: HDR management OFF (display restored to baseline)");
        }
    }

    /// Feed a foreground change to the reconciler (only while active).
    pub fn on_foreground(&mut self, ev: &WatchEvent, cfg: &Config, now: Instant) {
        let Some(a) = self.active.as_mut() else { return };
        let fg = crate::app::foreground_of(ev);
        let kind = rules::classify(&fg, cfg);
        let clamp = cfg.rule_for(&fg.exe_stem).1;
        a.reconciler.on_foreground(kind, ev.pid, clamp, now);
    }

    /// Liveness + reconcile + execute (the legacy pump's steps f/g).
    pub fn tick(&mut self, now: Instant) {
        let Some(a) = self.active.as_mut() else { return };
        if let Some(pid) = a.liveness.poll(a.reconciler.session_pid()) {
            a.reconciler.on_session_exit(pid, now);
        }
        for action in a.reconciler.poll(now) {
            a.exec.execute(action, &mut a.reconciler);
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.active.as_ref().and_then(|a| a.reconciler.next_deadline())
    }

    /// Manual HDR pin flip; refused unless HDR management is on.
    pub fn toggle(&mut self) -> Result<bool, String> {
        self.ready()?;
        let a = self.active.as_mut().ok_or("HDR management is off")?;
        let on = a.reconciler.toggle_manual_hdr();
        log!("record-hdr: manual HDR pin -> {on}");
        Ok(on)
    }

    /// Set the Advanced Color default profile (call [`ready`](Self::ready)
    /// first; this is the WCS write whose failure is an `ac_profile` event).
    pub fn set_ac_profile(&self, cfg: &Config, profile: &'static str) -> Result<&'static str, String> {
        crate::wcs::set_advanced_color_default(&cfg.display.target_friendly_name, profile)?;
        log!("record-hdr: set_ac_profile -> {profile} (next HDR entry)");
        Ok(profile)
    }

    /// `Ok` when HDR commands may change the display: the managed monitor is
    /// present AND `behavior.hdr_auto` is on.
    pub fn ready(&self) -> Result<(), String> {
        if !crate::hdr_gate::enabled() {
            return Err(crate::hdr_gate::SHELVED_MSG.into());
        }
        if self.target.is_none() {
            return Err("HDR is unavailable: the managed display was not found".into());
        }
        if self.active.is_none() {
            return Err("HDR management is off; set behavior.hdr_auto = true first".into());
        }
        Ok(())
    }

    /// Re-read the live HDR state (read-only) for the inactive snapshot.
    pub fn refresh_live(&mut self) {
        if let Some(t) = &self.target {
            self.live_hdr = display::get_hdr_state(t).unwrap_or(self.live_hdr);
        }
    }

    pub fn snapshot(&self, cfg: &Config) -> HdrSnapshot {
        let (hdr_on, manual_pin, session_pid) = match &self.active {
            Some(a) => (a.reconciler.actual().hdr, a.reconciler.manual_pinned(), a.reconciler.session_pid()),
            None => (self.live_hdr, false, None),
        };
        HdrSnapshot {
            available: self.target.is_some(),
            display: self.target.as_ref().map(|t| t.name().to_string()),
            hdr_on,
            manual_pin,
            auto: cfg.behavior.hdr_auto && crate::hdr_gate::enabled(),
            session_pid,
        }
    }

    /// Clean shutdown: restore through the spine (once), clear the journal,
    /// disarm the guard. A no-op when HDR was never managed.
    pub fn shutdown(&mut self) {
        let Some((mut guard, once)) = self.spine.take() else { return };
        once.run();
        // Same ordering as the legacy shutdown: restore, then clear a journal
        // the restore left dirty, then disarm.
        if crate::journal::Journal::is_dirty() {
            let cleared = match self.active.as_mut() {
                Some(a) => a.exec.clear_journal(),
                None => crate::journal::Journal::open().clear(),
            };
            if let Err(e) = cleared {
                log!("record-hdr: journal clear on shutdown failed: {e}");
            }
        }
        self.active = None;
        guard.disarm();
    }
}

impl crate::record_app::Controller {
    /// HDR's tick, after the one-time first-change notice when one is due: shown on the
    /// island (forced, like "Overlay off") once the island runs and is connected.
    pub(crate) fn hdr_tick(&mut self, now: Instant) {
        let viewers = if self.overlay.running() { self.hub.client_count() } else { 0 };
        if let Some((title, detail)) = self.hdr.notice_ready(now, viewers) {
            self.hub.event("session", &crate::record_notice::shortcut_notice_json(title, &detail, false, true));
            log!("record-hdr: first display-change notice shown");
            let cfg = self.cfg.clone();
            if let Err(e) = self.hdr.notice_shown(&cfg) {
                log!("record-hdr: HDR management could not start after the notice: {e}");
            }
        }
        self.hdr.tick(now);
    }
}

impl HdrSnapshot {
    /// The session status `hdr` object.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "available": self.available,
            "display": self.display,
            "hdr_on": self.hdr_on,
            "manual_pin": self.manual_pin,
            "auto": self.auto,
            "shelved": !crate::hdr_gate::enabled(),
        })
    }

    /// The legacy `mod:"hdr"` status event (kept for the Svelte hub).
    pub fn legacy_event(&self, engine_health: &str) -> String {
        serde_json::json!({
            "type": "status",
            "target": self.display.clone().unwrap_or_default(),
            "hdr": self.hdr_on,
            "manual_pin": self.manual_pin,
            "session_pid": self.session_pid,
            "auto": self.auto,
            "engine_health": engine_health,
        })
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests run without RECALL_EXPERIMENTAL_HDR, so this is the shipped (shelved) state.
    #[test]
    fn shelved_hdr_refuses_every_display_change() {
        assert!(!crate::hdr_gate::enabled(), "tests must see the shipped, shelved state");
        let mut cfg = Config::default();
        cfg.behavior.hdr_auto = true; // an old config that still asks for switching
        let mut hdr = HdrManager::shelved();
        assert!(!hdr.available());
        assert_eq!(hdr.activate(&cfg, Origin::Automatic), Err(crate::hdr_gate::SHELVED_MSG.to_string()));
        assert!(!hdr.is_active(), "hdr_auto in config does not start HDR management");
        assert_eq!(hdr.activate(&cfg, Origin::UserAction), Err(crate::hdr_gate::SHELVED_MSG.to_string()));
        assert_eq!(hdr.notice_ready(Instant::now() + Duration::from_secs(60), 3), None, "shelved: no notice is ever due");
        assert!(hdr.notice_shown(&cfg).is_err(), "nor does a shown notice start anything");
        assert_eq!(hdr.ready(), Err(crate::hdr_gate::SHELVED_MSG.to_string()));
        assert!(hdr.toggle().is_err(), "the manual HDR toggle is refused");
        let snap = hdr.snapshot(&cfg);
        assert!(!snap.available && !snap.auto && !snap.hdr_on, "status reads as off: {snap:?}");
        assert_eq!(snap.to_json()["shelved"], serde_json::json!(true));
        hdr.shutdown(); // no spine, no display restore: a no-op
    }
}
