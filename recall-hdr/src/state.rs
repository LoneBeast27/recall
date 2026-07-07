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
mod tests {
    use super::*;

    /// A fixed clock origin so tests can add deterministic offsets.
    fn t0() -> Instant {
        Instant::now()
    }

    const DEBOUNCE: Duration = Duration::from_millis(200);
    const SDR_WHITE: u32 = 200;

    /// Build a reconciler with the standard test debounce/white.
    fn rec() -> Reconciler {
        Reconciler::new(DEBOUNCE, SDR_WHITE)
    }

    /// Drive a full commit: fire the candidate, poll past its deadline, return the
    /// emitted actions. Applies `confirm` for each so `actual` advances (mirrors
    /// the executor happy path).
    fn settle(r: &mut Reconciler, kind: AppKind, pid: u32, clamp: ClampMode, base: Instant) -> Vec<Action> {
        r.on_foreground(kind, pid, clamp, base);
        let actions = r.poll(base + DEBOUNCE);
        for a in &actions {
            r.confirm(*a);
        }
        actions
    }

    // -- debounce coalescing -------------------------------------------------

    #[test]
    fn debounce_coalesces_splash_then_game_into_one_transition() {
        let mut r = rec();
        let base = t0();

        // Splash screen (NonGame) at t=0, then the real HDR game at t=150ms,
        // both inside the 200ms window.
        r.on_foreground(AppKind::NonGame, 100, ClampMode::Off, base);
        // A poll mid-window (before deadline) must do NOTHING.
        assert_eq!(r.poll(base + Duration::from_millis(150)), vec![]);

        r.on_foreground(AppKind::HdrGame, 200, ClampMode::Off, base + Duration::from_millis(150));
        // Still before the (replaced) deadline: nothing.
        assert_eq!(r.poll(base + Duration::from_millis(200)), vec![]);

        // Past the new deadline: exactly ONE transition (enable HDR + set white),
        // zero intermediate actions for the discarded splash.
        let actions = r.poll(base + Duration::from_millis(400));
        assert_eq!(actions, vec![Action::EnableHdr, Action::SetSdrWhite(SDR_WHITE)]);
        assert_eq!(r.session_pid(), Some(200), "the game, not the splash, owns the session");
    }

    #[test]
    fn newer_event_replaces_candidate_within_window() {
        let mut r = rec();
        let base = t0();
        r.on_foreground(AppKind::HdrGame, 1, ClampMode::Off, base);
        // Replace with an SDR game before the first deadline.
        r.on_foreground(AppKind::SdrGame, 2, ClampMode::Accurate, base + Duration::from_millis(100));
        let actions = r.poll(base + Duration::from_millis(400));
        // Only the SDR game commits: no HDR was ever enabled.
        assert_eq!(actions, vec![Action::SetClamp(ClampMode::Accurate)]);
        assert_eq!(r.session_pid(), Some(2));
    }

    // -- idempotence ---------------------------------------------------------

    #[test]
    fn idempotent_same_state_twice_is_empty_second_time() {
        let mut r = rec();
        let base = t0();
        let first = settle(&mut r, AppKind::HdrGame, 1, ClampMode::Off, base);
        assert_eq!(first, vec![Action::EnableHdr, Action::SetSdrWhite(SDR_WHITE)]);

        // Re-deriving the same desired state (a bare re-poll) yields ZERO actions
        // -- the no-double-flash guarantee.
        assert_eq!(r.poll(base + Duration::from_secs(1)), vec![]);
    }

    // -- alt-tab persistence -------------------------------------------------

    #[test]
    fn alt_tab_blip_to_nongame_keeps_session_and_emits_nothing() {
        let mut r = rec();
        let base = t0();
        settle(&mut r, AppKind::HdrGame, 42, ClampMode::Off, base);
        assert_eq!(r.session_pid(), Some(42));
        assert!(r.actual().hdr, "HDR is on for the game");

        // Alt-tab to the desktop: a NonGame foreground blip settles.
        let blip = settle(&mut r, AppKind::NonGame, 999, ClampMode::Off, base + Duration::from_secs(5));
        assert_eq!(blip, vec![], "alt-tab to desktop emits NO actions");
        assert_eq!(r.session_pid(), Some(42), "session survives the blip");
        assert!(r.actual().hdr, "HDR stays on -- no flip-storm");
    }

    #[test]
    fn alt_tab_back_to_game_still_no_actions() {
        // Alt-tab away then back: same game re-settles, still nothing changes.
        let mut r = rec();
        let base = t0();
        settle(&mut r, AppKind::HdrGame, 42, ClampMode::Off, base);
        settle(&mut r, AppKind::NonGame, 999, ClampMode::Off, base + Duration::from_secs(2));
        let back = settle(&mut r, AppKind::HdrGame, 42, ClampMode::Off, base + Duration::from_secs(4));
        assert_eq!(back, vec![], "re-focusing the same game emits nothing");
        assert!(r.actual().hdr);
    }

    // -- session exit reverts ------------------------------------------------

    #[test]
    fn session_exit_reverts_hdr_to_baseline() {
        let mut r = rec();
        let base = t0();
        settle(&mut r, AppKind::HdrGame, 42, ClampMode::Off, base);
        assert!(r.actual().hdr);

        // The process dies.
        r.on_session_exit(42, base + Duration::from_secs(10));
        assert_eq!(r.session_pid(), None, "session ended");

        // Next poll reverts to baseline (HDR off).
        let actions = r.poll(base + Duration::from_secs(10));
        assert_eq!(actions, vec![Action::DisableHdr]);
        for a in &actions {
            r.confirm(*a);
        }
        assert!(!r.actual().hdr);
    }

    #[test]
    fn sdr_game_exit_clears_clamp_to_baseline() {
        let mut r = rec();
        let base = t0();
        settle(&mut r, AppKind::SdrGame, 7, ClampMode::Accurate, base);
        assert_eq!(r.actual().clamp, ClampMode::Accurate);

        r.on_session_exit(7, base + Duration::from_secs(3));
        let actions = r.poll(base + Duration::from_secs(3));
        assert_eq!(actions, vec![Action::ClearClamp]);
    }

    #[test]
    fn stale_exit_pid_is_ignored() {
        let mut r = rec();
        let base = t0();
        settle(&mut r, AppKind::HdrGame, 42, ClampMode::Off, base);
        // A different (already-dead / unrelated) pid must not end the session.
        r.on_session_exit(999, base + Duration::from_secs(1));
        assert_eq!(r.session_pid(), Some(42), "unrelated exit ignored");
        assert_eq!(r.poll(base + Duration::from_secs(2)), vec![]);
    }

    // -- replacement ordering ------------------------------------------------

    #[test]
    fn hdr_game_replaced_by_sdr_game_disables_hdr_before_setclamp() {
        let mut r = rec();
        let base = t0();
        settle(&mut r, AppKind::HdrGame, 1, ClampMode::Off, base);
        assert!(r.actual().hdr);

        // A DIFFERENT game (SDR) settles -> session replacement.
        let actions = settle(&mut r, AppKind::SdrGame, 2, ClampMode::Accurate, base + Duration::from_secs(5));
        // Ordering constraint: DisableHdr MUST precede SetClamp.
        assert_eq!(actions, vec![Action::DisableHdr, Action::SetClamp(ClampMode::Accurate)]);
        assert_eq!(r.session_pid(), Some(2), "new game owns the session");
        assert!(!r.actual().hdr);
        assert_eq!(r.actual().clamp, ClampMode::Accurate);
    }

    #[test]
    fn sdr_game_replaced_by_hdr_game_clears_clamp_before_enablehdr() {
        let mut r = rec();
        let base = t0();
        settle(&mut r, AppKind::SdrGame, 1, ClampMode::Accurate, base);
        assert_eq!(r.actual().clamp, ClampMode::Accurate);

        // A DIFFERENT game (HDR) settles.
        let actions = settle(&mut r, AppKind::HdrGame, 2, ClampMode::Off, base + Duration::from_secs(5));
        // Ordering constraint: ClearClamp MUST precede EnableHdr; then SDR white.
        assert_eq!(
            actions,
            vec![Action::ClearClamp, Action::EnableHdr, Action::SetSdrWhite(SDR_WHITE)]
        );
        assert!(r.actual().hdr);
        assert_eq!(r.actual().clamp, ClampMode::Off);
    }

    // -- confirm advances actual --------------------------------------------

    #[test]
    fn confirm_advances_actual_so_later_reconciles_diff_correctly() {
        let mut r = rec();
        let base = t0();
        r.on_foreground(AppKind::HdrGame, 1, ClampMode::Off, base);
        let actions = r.poll(base + DEBOUNCE);
        assert_eq!(actions, vec![Action::EnableHdr, Action::SetSdrWhite(SDR_WHITE)]);
        // Before confirm, actual is still baseline.
        assert!(!r.actual().hdr);
        for a in &actions {
            r.confirm(*a);
        }
        assert!(r.actual().hdr, "confirm advanced actual");
        // Now the state matches; a re-poll is empty.
        assert_eq!(r.poll(base + Duration::from_secs(1)), vec![]);
    }

    // -- retry semantics (documented choice) --------------------------------

    #[test]
    fn unconfirmed_action_is_reemitted_on_next_poll() {
        let mut r = rec();
        let base = t0();
        r.on_foreground(AppKind::HdrGame, 1, ClampMode::Off, base);
        let first = r.poll(base + DEBOUNCE);
        assert_eq!(first, vec![Action::EnableHdr, Action::SetSdrWhite(SDR_WHITE)]);

        // The executor did NOT confirm (it failed). Actual is unchanged, so the
        // next poll RE-EMITS the same converging actions (retry-by-convergence).
        let retry = r.poll(base + Duration::from_secs(1));
        assert_eq!(retry, first, "unconfirmed action re-emitted next poll");

        // Partial confirm: only HDR took. Next poll emits just the remaining step.
        r.confirm(Action::EnableHdr);
        let remaining = r.poll(base + Duration::from_secs(2));
        assert_eq!(remaining, vec![Action::SetSdrWhite(SDR_WHITE)]);
    }

    // -- misc invariants -----------------------------------------------------

    #[test]
    fn next_deadline_tracks_pending_only() {
        let mut r = rec();
        let base = t0();
        assert_eq!(r.next_deadline(), None, "nothing pending at start");
        r.on_foreground(AppKind::HdrGame, 1, ClampMode::Off, base);
        assert_eq!(r.next_deadline(), Some(base + DEBOUNCE));
        // Committing clears the pending deadline.
        r.poll(base + DEBOUNCE);
        assert_eq!(r.next_deadline(), None, "deadline cleared after commit");
    }

    #[test]
    fn exit_of_pending_but_uncommitted_game_discards_the_candidate() {
        // A game becomes foreground but dies before its debounce deadline. The
        // exit signal for that pid must discard the candidate so we never commit a
        // session for a dead process.
        let mut r = rec();
        let base = t0();
        r.on_foreground(AppKind::HdrGame, 55, ClampMode::Off, base);
        r.on_session_exit(55, base + Duration::from_millis(50));
        assert_eq!(r.poll(base + Duration::from_secs(1)), vec![], "dead candidate never commits");
        assert_eq!(r.session_pid(), None);
    }

    #[test]
    fn sdr_game_punchy_clamp_is_respected() {
        // The per-app clamp mode flows through: a Punchy SDR game clamps Punchy.
        let mut r = rec();
        let base = t0();
        let actions = settle(&mut r, AppKind::SdrGame, 9, ClampMode::Punchy, base);
        assert_eq!(actions, vec![Action::SetClamp(ClampMode::Punchy)]);
        assert_eq!(r.actual().clamp, ClampMode::Punchy);
    }

    #[test]
    fn hdr_game_from_active_desktop_clamp_clears_first() {
        // Startup edge: the live display already has a clamp (e.g. from a prior
        // desktop clamp the app layer confirmed). An HDR game must clear it BEFORE
        // enabling HDR.
        let mut r = rec();
        let base = t0();
        r.confirm(Action::SetClamp(ClampMode::Accurate)); // seed live clamp
        let actions = settle(&mut r, AppKind::HdrGame, 1, ClampMode::Off, base);
        assert_eq!(
            actions,
            vec![Action::ClearClamp, Action::EnableHdr, Action::SetSdrWhite(SDR_WHITE)]
        );
    }

    // -- manual HDR pin (tray "Toggle HDR now" / hotkey) ---------------------

    #[test]
    fn manual_pin_on_emits_hdr_and_persists_across_polls_no_self_cancel() {
        // The core regression: a manual toggle-on must ENABLE HDR and STAY on --
        // no DisableHdr on any subsequent poll (the ~290ms self-cancel bug).
        let mut r = rec();
        let base = t0();

        let on = r.toggle_manual_hdr();
        assert!(on, "toggle from baseline pins HDR on");
        assert!(r.manual_pinned());

        // First poll applies the pin: EnableHdr + SetSdrWhite (same shape as a game).
        let first = r.poll(base);
        assert_eq!(first, vec![Action::EnableHdr, Action::SetSdrWhite(SDR_WHITE)]);
        for a in &first {
            r.confirm(*a);
        }
        assert!(r.actual().hdr, "HDR is on after the pin applied");

        // Multiple later polls must be EMPTY -- the pin holds, nothing undoes it.
        for k in 1..=5 {
            let later = r.poll(base + Duration::from_millis(300 * k));
            assert_eq!(later, vec![], "poll #{k} after pin must NOT self-cancel HDR");
        }
        assert!(r.actual().hdr, "HDR stayed on the whole time");
    }

    #[test]
    fn manual_pin_toggle_again_disables_hdr() {
        // Pin on, then toggle again -> HDR back off, pin cleared (normalized).
        let mut r = rec();
        let base = t0();
        assert!(r.toggle_manual_hdr());
        for a in &r.poll(base) {
            r.confirm(*a);
        }
        assert!(r.actual().hdr);

        let on = r.toggle_manual_hdr();
        assert!(!on, "second toggle intends HDR off");
        assert!(!r.manual_pinned(), "Some(false) normalizes to None -- not a pin");
        let off = r.poll(base + Duration::from_millis(100));
        assert_eq!(off, vec![Action::DisableHdr]);
        for a in &off {
            r.confirm(*a);
        }
        assert!(!r.actual().hdr, "HDR is back off");
    }

    #[test]
    fn game_session_commit_clears_manual_pin_and_drives_its_own_class() {
        // A pin is live (HDR on) on the desktop; then an SDR game settles. The game
        // OUTRANKS the pin: it clears the pin and drives its own class (HDR off +
        // clamp). The pin must not survive to fight the game.
        let mut r = rec();
        let base = t0();
        assert!(r.toggle_manual_hdr());
        for a in &r.poll(base) {
            r.confirm(*a);
        }
        assert!(r.actual().hdr && r.manual_pinned());

        // An SDR game settles -> replacement of the pin by a real session.
        let actions = settle(&mut r, AppKind::SdrGame, 7, ClampMode::Accurate, base + Duration::from_secs(2));
        assert!(!r.manual_pinned(), "committing a game clears the pin");
        // Leaving HDR (from the pin) before setting the SDR clamp -- ordering honored.
        assert_eq!(actions, vec![Action::DisableHdr, Action::SetClamp(ClampMode::Accurate)]);
        assert!(!r.actual().hdr);
        assert_eq!(r.actual().clamp, ClampMode::Accurate);
    }

    #[test]
    fn session_exit_returns_to_baseline_not_the_stale_pin() {
        // Pin HDR on, a game commits (clearing the pin) and its own HDR settles,
        // then the game exits. We must land on the TRUE baseline (HDR off), NOT
        // resurrect the pre-game pin.
        let mut r = rec();
        let base = t0();
        assert!(r.toggle_manual_hdr());
        for a in &r.poll(base) {
            r.confirm(*a);
        }
        // HDR game settles (pin cleared; game's own HDR persists -> no visible flip).
        let commit = settle(&mut r, AppKind::HdrGame, 9, ClampMode::Off, base + Duration::from_secs(2));
        assert_eq!(commit, vec![], "pin-on HDR == game HDR: nothing to change on commit");
        assert!(!r.manual_pinned());

        // Game exits -> revert to baseline HDR-off (the stale pin is NOT resurrected).
        r.on_session_exit(9, base + Duration::from_secs(5));
        let revert = r.poll(base + Duration::from_secs(5));
        assert_eq!(revert, vec![Action::DisableHdr], "exit reverts to baseline, not the pin");
        for a in &revert {
            r.confirm(*a);
        }
        assert!(!r.actual().hdr);
    }

    #[test]
    fn clear_manual_reverts_the_pin() {
        // clear_manual (RestoreNow / shutdown) drops the pin so the next poll
        // reconciles back to baseline.
        let mut r = rec();
        let base = t0();
        assert!(r.toggle_manual_hdr());
        for a in &r.poll(base) {
            r.confirm(*a);
        }
        assert!(r.actual().hdr && r.manual_pinned());

        r.clear_manual();
        assert!(!r.manual_pinned());
        let revert = r.poll(base + Duration::from_millis(100));
        assert_eq!(revert, vec![Action::DisableHdr], "cleared pin reverts to baseline");
    }

    #[test]
    fn pin_acts_immediately_no_debounce_needed() {
        // A foreground debounce is pending (a game candidate not yet at deadline).
        // A manual pin must still apply on the very next poll -- it is deliberate
        // user intent, not a noisy foreground event, so it bypasses the debounce
        // window entirely (the pin lives outside `pending`).
        let mut r = rec();
        let base = t0();
        r.on_foreground(AppKind::SdrGame, 3, ClampMode::Accurate, base); // deadline = base + DEBOUNCE
        assert!(r.toggle_manual_hdr());
        // Poll BEFORE the debounce deadline: the pending game hasn't committed, but
        // the pin is already effective -> HDR on now, no waiting.
        let acted = r.poll(base + Duration::from_millis(10));
        assert_eq!(acted, vec![Action::EnableHdr, Action::SetSdrWhite(SDR_WHITE)]);
        assert_eq!(r.session_pid(), None, "the game candidate has NOT committed yet");
    }

    #[test]
    fn pin_off_while_game_session_holds_flips_relative_to_game_but_is_inert() {
        // Toggling while a game holds the display: the pin flips relative to the
        // game's desired HDR, but a session OUTRANKS the pin, so nothing changes on
        // the display until the session ends.
        let mut r = rec();
        let base = t0();
        settle(&mut r, AppKind::HdrGame, 1, ClampMode::Off, base);
        assert!(r.actual().hdr, "game drove HDR on");

        // Toggle: desired().hdr is currently true (the game), so new pin intends off
        // -> normalizes to None. Either way, the session still wins: no actions.
        let _ = r.toggle_manual_hdr();
        assert_eq!(r.poll(base + Duration::from_secs(1)), vec![], "session outranks the pin");
        assert!(r.actual().hdr, "game HDR is untouched by the pin");
    }
}
