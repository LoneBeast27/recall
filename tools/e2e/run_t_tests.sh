#!/usr/bin/env bash
# run_t_tests.sh -- recall-hdr T1..T6 live closed-loop test harness.
#
# Drives the real tray app (recall-hdr.exe), a fake game (hdr-testcard.exe), and
# TWO independent oracles (DXGI capture-engine HDR verdict via recall.exe, and
# NvAPI clamp state via `recall-hdr --probe`) to verify the design's T1..T6.
#
# Display-state testing is AUTHORIZED (screens flash on HDR flips -- expected).
# The harness NEVER kills by image name (other recall processes may exist), only
# by the exact Windows pids it spawned. A cleanup trap ALWAYS tears everything
# down + runs --restore + restores the user's config byte-identical, even on
# failure.
#
# Usage:  bash tools/e2e/run_t_tests.sh [T1 T2 ...]   (default: all)
# Output: per-test PASS/FAIL with log + oracle evidence; a summary table at end.
#
# Preconditions built/checked by the harness:
#   - target/debug/recall-hdr.exe   (cargo build -p recall-hdr)
#   - target/release/recall.exe     (the capture-engine oracle)
#   - tools/hdr-testcard/target/debug/hdr-testcard.exe

set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
source "$HERE/lib.sh"

trap cleanup EXIT

# --- results table ---------------------------------------------------------
declare -A RESULT
declare -A NOTE
record() { RESULT["$1"]="$2"; NOTE["$1"]="${3:-}"; }

# Establish a clean baseline before a test: SDR + no clamp + no journal.
# (App must be stopped when called, so --restore can act and probe is honest.)
ensure_clean_baseline() {
  local why="$1"
  echo "  -- baseline check ($why) --"
  # Make sure no game / notepad is holding a session.
  stop_testcard; stop_notepad
  # Repair any leftover display state, then verify.
  "$APP_EXE" --restore >/dev/null 2>&1 || true
  local clamp dxgi jrnl
  clamp="$(oracle_clamp_active)"
  oracle_dxgi_run "baseline_$why"; dxgi="$(dxgi_verdict "baseline_$why")"
  jrnl="$(journal_any_touched)"
  echo "     clamp=$clamp  dxgi=$dxgi  journal_touched=$jrnl"
  [ "$clamp" = "false" ] || echo "     [warn] baseline clamp not clear"
  [ "$dxgi" = "SDR" ] || echo "     [warn] baseline not SDR (dxgi=$dxgi)"
}

banner() { echo ""; echo "============================================================"; echo "  $1"; echo "============================================================"; }

# ===========================================================================
# T1 -- HDR flip on game start
# ===========================================================================
run_T1() {
  banner "T1  HDR flip on fullscreen HDR game"
  ASSERT_FAILS=0; EVIDENCE=""
  write_config_override hdr_game
  ensure_clean_baseline T1
  start_app || { record T1 FAIL "app did not start"; return; }
  local mark; mark="$(log_mark)"
  start_testcard 2.0,2.0,2.0 || { record T1 FAIL "testcard did not start"; return; }
  sleep 3.5   # debounce (400ms) + reconcile + HDR set + margin
  local logtxt; logtxt="$(log_since "$mark")"
  echo "  --- app log (T1 window) ---"; echo "$logtxt" | sed 's/^/    | /' | tail -25

  assert_matches "log: HDR enable (set_hdr_state true)" "$logtxt" "set_hdr_state\(PG27AQDP, true\)"
  assert_matches "log: SDR-white asserted"              "$logtxt" "set_sdr_white\(PG27AQDP"
  foreground_testcard   # give the capture engine the testcard, not a stray window
  oracle_dxgi_run T1; local hdr; hdr="$(dxgi_verdict T1)"; local pct; pct="$(dxgi_over_one T1)"
  echo "  DXGI oracle verdict: $hdr ; >1.0 pixels: ${pct}%"
  # PRIMARY HDR-on proof = the DXGI panel-state verdict (independent of what the
  # capture engine happens to be pointed at). The >1.0 capture fraction is a
  # SECONDARY confirmation: reported, and asserted only when the engine actually
  # captured the testcard (high fraction). A low fraction here means the engine
  # captured a non-testcard window, NOT that HDR is off (verdict already proves).
  assert_eq "DXGI oracle: HDR on (panel state)" "$hdr" "HDR"
  if awk "BEGIN{exit !($pct > 10)}"; then
    ev "PASS capture >1.0 fraction large (${pct}%) -- testcard captured in HDR"
  else
    ev "INFO capture >1.0 fraction ${pct}% (engine captured a non-testcard window this pass; panel-state verdict is the HDR-on proof)"
  fi
  assert_true "journal hdr_touched=true" "$(journal_hdr_touched)"

  if [ "$ASSERT_FAILS" -eq 0 ]; then record T1 PASS; else record T1 FAIL "$ASSERT_FAILS assertion(s)"; fi
}

# ===========================================================================
# T2 -- revert on game exit
# ===========================================================================
run_T2() {
  banner "T2  revert to SDR on game exit"
  ASSERT_FAILS=0; EVIDENCE=""
  # Continue from T1's live HDR session if present; else set one up.
  if [ -z "$APP_PID" ] || ! pid_alive "$APP_PID"; then
    write_config_override hdr_game
    ensure_clean_baseline T2
    start_app || { record T2 FAIL "app did not start"; return; }
    start_testcard 2.0,2.0,2.0 || { record T2 FAIL "testcard did not start"; return; }
    sleep 3.5
  fi
  local mark; mark="$(log_mark)"
  echo "  killing testcard (pid $TESTCARD_PID) to end the session"
  local dead_pid="$TESTCARD_PID"
  stop_testcard
  sleep 5.0   # liveness tick + DisableHdr
  local logtxt; logtxt="$(log_since "$mark")"
  echo "  --- app log (T2 window) ---"; echo "$logtxt" | sed 's/^/    | /' | tail -20

  assert_matches "log: session pid exited" "$logtxt" "session pid $dead_pid exited"
  assert_matches "log: HDR disable (set_hdr_state false)" "$logtxt" "set_hdr_state\(PG27AQDP, false\)"
  oracle_dxgi_run T2; local hdr; hdr="$(dxgi_verdict T2)"
  echo "  DXGI oracle verdict: $hdr"
  assert_eq "DXGI oracle: SDR" "$hdr" "SDR"
  # Journal should be cleared (no touched flags / file gone).
  assert_false "journal cleared (no touched flags)" "$(journal_any_touched)"
  if [ "$ASSERT_FAILS" -gt 0 ]; then
    ev "DIAGNOSIS: live session-exit detection does not fire. check_session_liveness"
    ev "  opens the game handle with PROCESS_QUERY_LIMITED_INFORMATION only, which"
    ev "  LACKS SYNCHRONIZE -> WaitForSingleObject returns WAIT_FAILED (ERROR_ACCESS_"
    ev "  DENIED 5), never WAIT_OBJECT_0, so the exit branch never runs. Display is"
    ev "  only reverted via --restore (next launch), never live. FIX: OpenProcess("
    ev "  PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, ...) in app.rs."
  fi

  if [ "$ASSERT_FAILS" -eq 0 ]; then record T2 PASS; else record T2 FAIL "$ASSERT_FAILS assertion(s)"; fi
}

# ===========================================================================
# T3 -- alt-tab persistence
# ===========================================================================
run_T3() {
  banner "T3  alt-tab persistence (HDR stays on across a non-game foreground)"
  ASSERT_FAILS=0; EVIDENCE=""
  write_config_override hdr_game
  ensure_clean_baseline T3
  start_app || { record T3 FAIL "app did not start"; return; }
  start_testcard 2.0,2.0,2.0 || { record T3 FAIL "testcard did not start"; return; }
  sleep 3.5
  oracle_dxgi_run T3_pre; local hdr0; hdr0="$(dxgi_verdict T3_pre)"
  assert_eq "HDR on before alt-tab" "$hdr0" "HDR"

  local mark; mark="$(log_mark)"
  echo "  foregrounding notepad (non-game) for >2s ..."
  start_notepad || { record T3 FAIL "notepad did not start"; return; }
  sleep 3.0
  local logtxt; logtxt="$(log_since "$mark")"
  echo "  --- app log (alt-tab window) ---"; echo "$logtxt" | sed 's/^/    | /' | tail -20
  # CORE T3 feature: NO DisableHdr while a non-game is foreground (persistence).
  local pre_fails="$ASSERT_FAILS"
  assert_not_matches "no DisableHdr during alt-tab" "$logtxt" "set_hdr_state\(PG27AQDP, false\)"
  oracle_dxgi_run T3_mid; local hdrmid; hdrmid="$(dxgi_verdict T3_mid)"
  echo "  DXGI oracle (notepad foreground): $hdrmid"
  assert_eq "HDR REMAINS on during alt-tab" "$hdrmid" "HDR"
  local persistence_fails=$((ASSERT_FAILS - pre_fails))

  # Teardown revert -- shares the T2 session-exit bug; scored separately so the
  # persistence result (the actual T3 feature) isn't masked by the known defect.
  local mark2; mark2="$(log_mark)"
  stop_notepad
  local dead="$TESTCARD_PID"
  stop_testcard
  sleep 5.0
  local logtxt2; logtxt2="$(log_since "$mark2")"
  local pre2="$ASSERT_FAILS"
  assert_matches "log: session exit after game killed" "$logtxt2" "session pid $dead exited"
  assert_matches "log: HDR disable on revert" "$logtxt2" "set_hdr_state\(PG27AQDP, false\)"
  oracle_dxgi_run T3_post; local hdrend; hdrend="$(dxgi_verdict T3_post)"
  echo "  DXGI oracle (after teardown): $hdrend"
  assert_eq "HDR reverted to SDR" "$hdrend" "SDR"
  local revert_fails=$((ASSERT_FAILS - pre2))

  if [ "$revert_fails" -gt 0 ]; then
    ev "NOTE teardown-revert assertions ($revert_fails) fail for the SAME session-exit"
    ev "  bug as T2 (PROCESS_QUERY_LIMITED_INFORMATION lacks SYNCHRONIZE)."
  fi
  # T3 verdict is about PERSISTENCE (its named feature). Report both.
  if [ "$persistence_fails" -eq 0 ] && [ "$revert_fails" -eq 0 ]; then
    record T3 PASS
  elif [ "$persistence_fails" -eq 0 ]; then
    record T3 "PARTIAL" "persistence PASS; teardown-revert blocked by T2 session-exit bug"
  else
    record T3 FAIL "$persistence_fails persistence assertion(s)"
  fi
}

# ===========================================================================
# T2b -- SDR-game clamp (accurate) -- no HDR flip, clamp engages, clears on exit
# ===========================================================================
run_T2b() {
  banner "T2b  SDR-game accurate clamp (punchy/accurate feature)"
  ASSERT_FAILS=0; EVIDENCE=""
  write_config_override sdr_game accurate
  ensure_clean_baseline T2b
  start_app || { record T2b FAIL "app did not start"; return; }
  local mark; mark="$(log_mark)"
  start_testcard 0.5,0.25,0.1 || { record T2b FAIL "testcard did not start"; return; }
  sleep 3.5
  local logtxt; logtxt="$(log_since "$mark")"
  echo "  --- app log (T2b window) ---"; echo "$logtxt" | sed 's/^/    | /' | tail -20

  # ENGAGE half (the punchy/accurate feature itself): no HDR flip + clamp on.
  local pre="$ASSERT_FAILS"
  assert_not_matches "no HDR enable for SDR game" "$logtxt" "set_hdr_state\(PG27AQDP, true\)"
  assert_matches "log: accurate sRGB clamp engaged" "$logtxt" "NvApiClamp.apply\(Accurate\)"
  local clamp; clamp="$(oracle_clamp_active)"    # probe oracle WHILE app live
  echo "  probe oracle: clamp active=$clamp"
  assert_true "probe: clamp active during SDR game" "$clamp"
  oracle_dxgi_run T2b; local hdr; hdr="$(dxgi_verdict T2b)"
  assert_eq "DXGI oracle: still SDR (no flip)" "$hdr" "SDR"
  local engage_fails=$((ASSERT_FAILS - pre))

  # CLEAR-on-exit half: shares the T2 session-exit bug -- scored separately.
  local mark2; mark2="$(log_mark)"
  stop_testcard
  sleep 5.0
  local logtxt2; logtxt2="$(log_since "$mark2")"
  local pre2="$ASSERT_FAILS"
  assert_matches "log: clamp cleared on exit" "$logtxt2" "NvApiClamp.reset\(\)"
  local clamp2; clamp2="$(oracle_clamp_active)"
  echo "  probe oracle after exit: clamp active=$clamp2"
  assert_false "probe: clamp cleared after game exit" "$clamp2"
  local clear_fails=$((ASSERT_FAILS - pre2))

  if [ "$clear_fails" -gt 0 ]; then
    ev "NOTE clamp-clear-on-exit ($clear_fails) fails for the SAME session-exit bug"
    ev "  as T2 (SYNCHRONIZE missing on the liveness handle). Clamp IS cleared on"
    ev "  the next --restore -- verified in final-state (clamp active=false)."
  fi
  if [ "$engage_fails" -eq 0 ] && [ "$clear_fails" -eq 0 ]; then
    record T2b PASS
  elif [ "$engage_fails" -eq 0 ]; then
    record T2b "PARTIAL" "clamp ENGAGE PASS; clear-on-exit blocked by T2 session-exit bug"
  else
    record T2b FAIL "$engage_fails engage assertion(s)"
  fi
}

# ===========================================================================
# T5 -- crash-restore (hard kill leaves display dirty; --restore repairs)
# ===========================================================================
run_T5() {
  banner "T5  crash-restore (hard-kill app mid-HDR, --restore repairs)"
  ASSERT_FAILS=0; EVIDENCE=""
  write_config_override hdr_game
  ensure_clean_baseline T5
  start_app || { record T5 FAIL "app did not start"; return; }
  start_testcard 2.0,2.0,2.0 || { record T5 FAIL "testcard did not start"; return; }
  sleep 3.5
  oracle_dxgi_run T5_pre; local hdr0; hdr0="$(dxgi_verdict T5_pre)"
  assert_eq "HDR on before crash" "$hdr0" "HDR"
  assert_true "journal hdr_touched before crash" "$(journal_hdr_touched)"

  echo "  HARD-killing app tree (pid $APP_PID) mid-session ..."
  # taskkill /F /T the app -- its RAII/ctrl guards never run -> display left dirty.
  taskkill //PID "$APP_PID" //T //F >/dev/null 2>&1 || true
  local ap="$APP_PID"; APP_PID=""
  sleep 1.5
  # Display should still be HDR (dirty) and journal dirty.
  oracle_dxgi_run T5_dirty; local hdrdirty; hdrdirty="$(dxgi_verdict T5_dirty)"
  echo "  DXGI oracle after hard-kill (expect still HDR/dirty): $hdrdirty"
  assert_eq "display left HDR after hard kill (dirty)" "$hdrdirty" "HDR"
  assert_true "journal still hdr_touched (dirty)" "$(journal_hdr_touched)"

  echo "  running recall-hdr --restore ..."
  local rout; rout="$("$APP_EXE" --restore 2>&1)"
  echo "$rout" | sed 's/^/    | /' | tail -12
  assert_matches "restore log: baseline reapplied / journal cleared" "$rout" "restore complete|journal cleared|nothing to restore"
  oracle_dxgi_run T5_post; local hdrrest; hdrrest="$(dxgi_verdict T5_post)"
  echo "  DXGI oracle after --restore: $hdrrest"
  assert_eq "DXGI oracle: SDR after restore" "$hdrrest" "SDR"
  assert_false "journal cleared after restore" "$(journal_any_touched)"

  stop_testcard
  if [ "$ASSERT_FAILS" -eq 0 ]; then record T5 PASS; else record T5 FAIL "$ASSERT_FAILS assertion(s)"; fi
}

# ===========================================================================
# T6 -- tray round-trip (config side only; UI clicks SKIPPED(manual))
# ===========================================================================
run_T6() {
  banner "T6  tray/config round-trip (autostart HKCU Run mirror)"
  ASSERT_FAILS=0; EVIDENCE=""
  # Config default has autostart=true. Start the app; Config isn't saved on plain
  # start (only tray edits call save), so the Run key is only written when the
  # config is saved. We test the automatable half: the app's Config::save mirror.
  # Approach: read the HKCU Run key state, and verify the tray build path leaves
  # it consistent with config.autostart after a start.  The reg is READ-ONLY here.
  echo "  reading HKCU Run key (read-only) ..."
  local runkey
  runkey="$(reg query 'HKCU\Software\Microsoft\Windows\CurrentVersion\Run' //v recall-hdr 2>&1 | tr -d '\r')"
  echo "$runkey" | sed 's/^/    | /'
  local present="false"
  echo "$runkey" | grep -qi "recall-hdr" && present="true"
  ev "INFO HKCU Run 'recall-hdr' present=$present (config default autostart=true; only WRITTEN on a tray/config save, not on plain start)"
  # This is honest reporting: a plain `recall-hdr.exe` start does NOT save config,
  # so it does NOT write the Run key. The mirror is exercised by Config::save,
  # covered by the crate's live registry unit test (autostart_write_readback_
  # delete_live, #[ignore]). We surface that fact rather than fake a click.
  ev "SKIPPED(manual) tray menu clicks: Toggle HDR, Auto-by-game, Set default clamp, Autostart, Open config/log, Restore Now -- require a human on the tray icon"
  ev "NOTE Config::save round-trip + HKCU mirror is unit-tested: cargo test -p recall-hdr --bins -- --ignored config::tests::autostart_write_readback_delete_live"
  record T6 "SKIPPED(manual)" "config-save mirror covered by ignored live unit test; UI clicks need a human"
}

# ===========================================================================
# Driver
# ===========================================================================
main() {
  echo "recall-hdr T1..T6 live harness  ($(date))"
  echo "app     : $APP_EXE"
  echo "engine  : $ENGINE_EXE"
  echo "testcard: $TESTCARD_EXE"
  # Preflight: binaries present?
  for f in "$APP_EXE" "$ENGINE_EXE" "$TESTCARD_EXE"; do
    [ -f "$f" ] || { echo "MISSING binary: $f"; exit 2; }
  done
  backup_config

  local tests=("$@")
  [ "${#tests[@]}" -eq 0 ] && tests=(T1 T2 T3 T2b T5 T6)

  for t in "${tests[@]}"; do
    case "$t" in
      T1)  run_T1 ;;
      T2)  run_T2 ;;
      T3)  run_T3 ;;
      T2b) run_T2b ;;
      T5)  run_T5 ;;
      T6)  run_T6 ;;
      *) echo "unknown test: $t" ;;
    esac
    # Between tests, drop the app so the next baseline check is honest.
    stop_app
  done

  # --- summary table ---
  echo ""
  echo "============================================================"
  echo "  RESULTS"
  echo "============================================================"
  printf "  %-6s  %-16s  %s\n" "TEST" "RESULT" "NOTE"
  printf "  %-6s  %-16s  %s\n" "----" "------" "----"
  for t in T1 T2 T3 T2b T5 T6; do
    [ -n "${RESULT[$t]:-}" ] && printf "  %-6s  %-16s  %s\n" "$t" "${RESULT[$t]}" "${NOTE[$t]:-}"
  done
  echo "============================================================"
}

main "$@"
