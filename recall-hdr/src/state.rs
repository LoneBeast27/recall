//! The reconciler state machine: desired-vs-actual display state, foreground
//! debounce, game-session tracking (stay-HDR while the game process lives).
//! Pure logic, no Win32.
//!
//! # Mental model
//! The app layer feeds this module three kinds of fact and asks it two kinds of
//! question; everything is driven by *injected* [`Instant`]s so it is fully
//! deterministic under test.
//!
//! Facts in:
//! - [`Reconciler::on_foreground`] -- "app `pid` classified as `kind` (with clamp
//!   `clamp` if it's an SDR game) just became the foreground window at `now`".
//! - [`Reconciler::on_session_exit`] -- "the process `pid` we were tracking as the
//!   live game session has exited" (liveness signal from the app layer, which is
//!   the only thing that can end a session besides a *different* game replacing it).
//! - [`Reconciler::confirm`] -- "the executor successfully applied this [`Action`]",
//!   which advances our model of the live (`actual`) display so the next diff is
//!   correct.
//!
//! Questions out:
//! - [`Reconciler::poll`] -- "given `now`, is the debounce deadline reached, and if
//!   so what [`Action`]s move `actual` toward the desired state?" (empty otherwise).
//! - [`Reconciler::next_deadline`] -- "when should the message loop wake me?".
//!
//! # The two-level design
//! There are deliberately two layers of state, and conflating them is the classic
//! bug this module exists to prevent:
//!
//! 1. **Session** (`session`): survives alt-tab. A game AppKind settling as
//!    foreground *starts* a session; the session lives until the process exits
//!    ([`on_session_exit`]) or a **different** game settles (replacement). A
//!    transient [`AppKind::NonGame`] blip (alt-tab to the desktop/browser) does
//!    NOT touch the session -- HDR stays on, no flip-storm. This is the whole
//!    point of the module.
//! 2. **Debounce** (`pending`): raw foreground events are noisy (a splash screen,
//!    then the game). A foreground event stores a *pending candidate* and a
//!    deadline `now + debounce`; a newer event within the window *replaces* it. We
//!    only fold the candidate into session state (and emit actions) when
//!    [`poll`] is called at/after the deadline. So a 150ms splash followed by the
//!    real game coalesces into ONE transition with zero intermediate actions.
//!
//! # Desired-vs-actual reconcile (idempotence + ordering)
//! `desired()` is a pure function of the current session. [`poll`] diffs it against
//! `actual` (our model of the live display, advanced only by [`confirm`]) and emits
//! the minimal ordered [`Action`] list. Re-deriving the same desired state produces
//! ZERO actions the second time -- the no-double-flash guarantee. Two hardware
//! ordering constraints are honored:
//! - **Into HDR:** [`Action::ClearClamp`] (if a clamp is live) is emitted BEFORE
//!   [`Action::EnableHdr`] -- the NvAPI clamp is rejected while HDR is on, so we
//!   drop it first.
//! - **Out of HDR into an SDR-game clamp:** [`Action::DisableHdr`] is emitted
//!   BEFORE [`Action::SetClamp`] -- same constraint, other direction.
//!
//! # Retry semantics (documented design choice)
//! [`poll`] diffs desired against `actual`, and `actual` advances ONLY on
//! [`confirm`]. Therefore an action the executor emitted but did not confirm (it
//! failed, or the app layer never called `confirm`) is **re-emitted on the next
//! `poll`**. This is intentional: the reconciler is a convergence loop, not a
//! fire-and-forget dispatcher. The executor is responsible for calling `confirm`
//! only on genuine success; anything else self-heals on the next reconcile tick.

use std::time::{Duration, Instant};

use crate::types::{Action, AppKind, ClampMode, DesiredDisplay};

/// A live game session. Survives alt-tab; ends on process exit or replacement by
/// a different game. `clamp` is the per-app clamp mode captured at session start
/// (only meaningful for [`AppKind::SdrGame`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Session {
    kind: AppKind,
    pid: u32,
    clamp: ClampMode,
}

/// A debounced, not-yet-committed foreground candidate.
#[derive(Debug, Clone, Copy)]
struct Pending {
    kind: AppKind,
    pid: u32,
    clamp: ClampMode,
    /// The wall time at/after which [`Reconciler::poll`] should commit this.
    deadline: Instant,
}

/// The reconciler state machine. Pure logic; drive it with injected `Instant`s.
pub struct Reconciler {
    debounce: Duration,
    sdr_white: u32,

    /// The committed live game session, if any. `None` == on the desktop baseline.
    session: Option<Session>,

    /// A user's manual HDR pin (tray "Toggle HDR now" / hotkey), `None` == no pin.
    /// A desired-state override, NOT an executor bypass: it feeds [`desired`] so the
    /// reconcile/journal/confirm machinery applies it and, crucially, does NOT undo
    /// it on the next tick (the self-cancel bug this field fixes). A **game session
    /// outranks the pin** so the game experience is never held hostage by a stale
    /// pin; committing a new session clears it (see [`commit`]). Normalized so
    /// `Some(false)` with no session collapses to `None` (a pin equal to the HDR-off
    /// baseline is not a pin) -- see [`toggle_manual_hdr`].
    manual_hdr: Option<bool>,

    /// A foreground event awaiting its debounce deadline. Replaced by any newer
    /// event; consumed (committed or discarded) by [`poll`] once the deadline
    /// passes.
    pending: Option<Pending>,

    /// Our model of the live display, advanced only by [`confirm`]. The diff
    /// target for reconcile. Starts at the desktop baseline (HDR off, no clamp).
    actual: DesiredDisplay,
}

impl Reconciler {
    /// Construct a reconciler. `debounce` is the settle window for foreground
    /// events; `sdr_white` is the SDR-content white level (nits) requested while
    /// HDR is on. The live model starts at the desktop baseline: HDR off, clamp
    /// off (native gamut). If the real display differs at startup, the first
    /// `poll` reconciles it.
    ///
    /// `actual.sdr_white` starts at `0` -- the **"not asserted"** sentinel. The
    /// SDR-content white level is a property of HDR mode only (see `SDR_WHITE_UNSET`
    /// on [`desired`]); it is unknown/irrelevant on the SDR desktop and is not
    /// asserted onto the panel until HDR turns on. Seeding it to the config value
    /// would be a lie (the live display's white level is whatever Windows last
    /// used), so we treat it as unset and always assert it when entering HDR.
    pub fn new(debounce: Duration, sdr_white: u32) -> Self {
        Reconciler {
            debounce,
            sdr_white,
            session: None,
            manual_hdr: None,
            pending: None,
            actual: DesiredDisplay {
                hdr: false,
                clamp: ClampMode::Off,
                sdr_white: Self::SDR_WHITE_UNSET,
            },
        }
    }

    /// Sentinel for "SDR white level is not currently asserted" (HDR off / unknown
    /// live value). Kept out of band from any real nits value so a non-HDR desired
    /// and a non-HDR actual compare equal (no spurious `SetSdrWhite` while HDR is
    /// off) and entering HDR always emits one explicit assertion.
    const SDR_WHITE_UNSET: u32 = 0;

    /// A foreground app was observed. Stores it as the pending candidate with a
    /// fresh deadline `now + debounce`, replacing any earlier candidate still in
    /// the window. Does NOT act immediately -- [`poll`] commits it once the
    /// deadline passes. `clamp` is this app's configured clamp mode (used only if
    /// it's an [`AppKind::SdrGame`]).
    pub fn on_foreground(&mut self, kind: AppKind, pid: u32, clamp: ClampMode, now: Instant) {
        self.pending = Some(Pending {
            kind,
            pid,
            clamp,
            deadline: now + self.debounce,
        });
    }

    /// The app layer reports the tracked session process exited. If `pid` matches
    /// the live session, the session ends (desired reverts to baseline); the next
    /// [`poll`] emits the revert actions. A stale/non-matching pid is ignored. A
    /// pending candidate for the same dead pid is also cleared so we don't commit
    /// a session for a process we already know is gone.
    pub fn on_session_exit(&mut self, pid: u32, _now: Instant) {
        if self.session.map(|s| s.pid) == Some(pid) {
            self.session = None;
        }
        if self.pending.map(|p| p.pid) == Some(pid) {
            self.pending = None;
        }
    }

    /// The pid the app layer should watch for liveness: the live session's pid, or
    /// `None` when on the baseline. (A pending-but-uncommitted candidate is NOT
    /// yet the session and is not returned here.)
    pub fn session_pid(&self) -> Option<u32> {
        self.session.map(|s| s.pid)
    }

    /// Manual HDR toggle (tray "Toggle HDR now" / hotkey). Flips the pin relative to
    /// the CURRENT effective desired HDR (`new = !desired().hdr`), stores it, and
    /// returns the new HDR intent for the caller to log / surface in the status line.
    ///
    /// This is a **desired-state** override, not an executor bypass: the pin feeds
    /// [`desired`], so the very next [`poll`] emits the converging actions through
    /// the normal journal/confirm path and -- because `desired()` now reflects the
    /// pin -- the reconciler will NOT diff it back out on the following tick (the
    /// self-cancel bug). It acts immediately on the next poll; no debounce (that is a
    /// noise filter for foreground events, not deliberate user intent).
    ///
    /// Precedence: a live game session outranks the pin (see [`desired`]), so
    /// toggling while a game holds the display flips relative to the game's desired
    /// HDR but is inert until the session ends; the common case (on the desktop) is
    /// the whole point -- a manual HDR-on that STAYS on.
    ///
    /// Normalization: a pin equal to the HDR-off baseline (`Some(false)` with no
    /// session) is stored as `None` -- it is not a pin, just baseline. `pin on ->
    /// off` therefore returns `false` and clears the pin.
    pub fn toggle_manual_hdr(&mut self) -> bool {
        let new_pin = !self.desired().hdr;
        // Some(false) == the no-session baseline; normalize to None (not a pin).
        self.manual_hdr = if new_pin { Some(true) } else { None };
        new_pin
    }

    /// Whether a manual HDR pin is currently in effect (introspection for the status
    /// line, so the user sees *why* HDR is on -- "HDR (manual)").
    pub fn manual_pinned(&self) -> bool {
        self.manual_hdr.is_some()
    }

    /// Drop any manual HDR pin (RestoreNow / shutdown, so the restore isn't
    /// immediately re-asserted by a stale pin). The next [`poll`] reconciles from the
    /// session-derived desired state alone.
    pub fn clear_manual(&mut self) {
        self.manual_hdr = None;
    }

    /// When the message loop should next wake to call [`poll`]: the pending
    /// candidate's deadline, or `None` if nothing is pending.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending.map(|p| p.deadline)
    }

    /// Debounce-gated reconcile. If a candidate's deadline has passed, fold it into
    /// session state, then diff the resulting desired state against `actual` and
    /// return the ordered actions to converge. Returns empty when the deadline has
    /// not yet passed AND the state already matches (the common idle poll).
    ///
    /// Note: `poll` is also the retry tick -- even with no pending candidate, if
    /// `actual` has drifted from desired (e.g. a prior action was never confirmed)
    /// it re-emits the converging actions.
    pub fn poll(&mut self, now: Instant) -> Vec<Action> {
        if let Some(p) = self.pending {
            if now >= p.deadline {
                self.pending = None;
                self.commit(p);
            }
        }
        self.reconcile()
    }

    /// The executor confirmed an action succeeded; advance the live model so the
    /// next diff reflects reality. Unknown/`Noop` actions are ignored.
    pub fn confirm(&mut self, action: Action) {
        match action {
            Action::EnableHdr => self.actual.hdr = true,
            // Leaving HDR un-asserts the SDR white level (it's an HDR-mode
            // property); reset the sentinel so re-entering HDR asserts it afresh.
            Action::DisableHdr => {
                self.actual.hdr = false;
                self.actual.sdr_white = Self::SDR_WHITE_UNSET;
            }
            Action::SetClamp(m) => self.actual.clamp = m,
            Action::ClearClamp => self.actual.clamp = ClampMode::Off,
            Action::SetSdrWhite(w) => self.actual.sdr_white = w,
            Action::Noop => {}
        }
    }

    /// The current live model (introspection / logging).
    pub fn actual(&self) -> DesiredDisplay {
        self.actual
    }

    // -- internals ----------------------------------------------------------

    /// Fold a committed candidate into session state. A game candidate starts or
    /// replaces the session; a [`AppKind::NonGame`] candidate is a transient blip
    /// that leaves an existing session untouched (alt-tab persistence) and only
    /// matters when there is no session (stays on baseline).
    fn commit(&mut self, p: Pending) {
        match p.kind {
            AppKind::HdrGame | AppKind::SdrGame => {
                // Start or replace the session. Replacement (a *different* game
                // settling) transitions old->new directly; the reconcile diff
                // produces the correct ordered actions.
                self.session = Some(Session {
                    kind: p.kind,
                    pid: p.pid,
                    clamp: p.clamp,
                });
                // A new game takes over the display: drop any manual pin so that on
                // this session's exit we return to the TRUE HDR-off baseline, not a
                // stale pre-game pin. (Session exit does NOT resurrect the pin --
                // it was cleared here, at session start.)
                self.manual_hdr = None;
            }
            AppKind::NonGame => {
                // Alt-tab blip. If a session is live it PERSISTS (HDR stays on, no
                // actions). With no session we're already on baseline. Either way,
                // desired state is unchanged -- nothing to do here.
            }
        }
    }

    /// The desired display state derived purely from the current session.
    fn desired(&self) -> DesiredDisplay {
        match self.session {
            Some(Session {
                kind: AppKind::HdrGame,
                ..
            }) => DesiredDisplay {
                hdr: true,
                clamp: ClampMode::Off,
                sdr_white: self.sdr_white,
            },
            Some(Session {
                kind: AppKind::SdrGame,
                clamp,
                ..
            }) => DesiredDisplay {
                hdr: false,
                clamp,
                // HDR off -> white level not asserted (sentinel), so it never
                // spuriously diffs against `actual` while HDR is off.
                sdr_white: Self::SDR_WHITE_UNSET,
            },
            // A committed NonGame session is never stored, so this arm is the
            // no-session baseline -- where a manual HDR pin (if any) applies. The
            // pin outranks nothing here (there is no session), and a game session in
            // the arms above outranks it: the game experience is never held hostage
            // by a stale pin. The pin only affects HDR (+ SDR-white asserted when HDR
            // is on, exactly like an HdrGame session); it never touches the clamp.
            // `manual_hdr` is normalized so only `Some(true)` reaches here as a real
            // override (`Some(false)`/`None` == the HDR-off baseline).
            _ => match self.manual_hdr {
                Some(true) => DesiredDisplay {
                    hdr: true,
                    clamp: ClampMode::Off,
                    sdr_white: self.sdr_white,
                },
                _ => DesiredDisplay {
                    hdr: false,
                    clamp: ClampMode::Off,
                    sdr_white: Self::SDR_WHITE_UNSET,
                },
            },
        }
    }

    /// Diff `desired()` against `actual` into the minimal ordered action list.
    /// Idempotent: when they already agree the result is empty. Honors the two
    /// hardware ordering constraints (clamp cleared before HDR on; HDR off before
    /// an SDR clamp is set).
    fn reconcile(&self) -> Vec<Action> {
        let desired = self.desired();
        let actual = self.actual;
        let mut actions = Vec::new();

        if desired == actual {
            return actions;
        }

        if desired.hdr && !actual.hdr {
            // Transitioning INTO HDR. The NvAPI clamp is rejected while HDR is on,
            // so drop any live clamp FIRST, then enable HDR, then set SDR white.
            if actual.clamp != ClampMode::Off {
                actions.push(Action::ClearClamp);
            }
            actions.push(Action::EnableHdr);
            if desired.sdr_white != actual.sdr_white {
                actions.push(Action::SetSdrWhite(desired.sdr_white));
            }
        } else if !desired.hdr && actual.hdr {
            // Leaving HDR. Disable HDR FIRST, then apply the target clamp (a clamp
            // SET is rejected while HDR is still on). `Off` means clear any clamp.
            actions.push(Action::DisableHdr);
            if desired.clamp != actual.clamp {
                match desired.clamp {
                    ClampMode::Off => actions.push(Action::ClearClamp),
                    m => actions.push(Action::SetClamp(m)),
                }
            }
        } else {
            // HDR state already matches; only the clamp (and/or white level) differ.
            if desired.clamp != actual.clamp {
                match desired.clamp {
                    ClampMode::Off => actions.push(Action::ClearClamp),
                    m => actions.push(Action::SetClamp(m)),
                }
            }
            // SDR white only matters while HDR is on; emit if it drifted.
            if desired.hdr && desired.sdr_white != actual.sdr_white {
                actions.push(Action::SetSdrWhite(desired.sdr_white));
            }
        }

        actions
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
