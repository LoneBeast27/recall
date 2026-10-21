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
