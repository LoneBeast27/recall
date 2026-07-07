#!/usr/bin/env bash
# lib.sh -- shared helpers for the recall-hdr T1-T6 live test harness.
#
# Sourced by run_t_tests.sh. Owns: process spawn/track/kill (BY PID, never image
# name), the two independent oracles (DXGI capture-engine + NvAPI --probe clamp
# state), log-tailing by byte offset, assertion primitives, and a cleanup trap
# that ALWAYS tears down every spawned pid + runs --restore + restores the user's
# original config.
#
# Windows note: we run under Git Bash. PIDs we track are the WINDOWS pids (via
# `//PID` for taskkill and `PID eq` for tasklist), obtained from a PowerShell
# Start-Process launcher so we get the real OS pid (a bash `&` job pid is the
# MSYS shim, not the Windows process taskkill needs).

set -u

# ---------------------------------------------------------------------------
# Paths (all absolute; harness owns only tools/e2e/)
# ---------------------------------------------------------------------------
RECALL_ROOT="L:/Projects/recall"
APP_EXE="$RECALL_ROOT/target/debug/recall-hdr.exe"
ENGINE_EXE="$RECALL_ROOT/target/release/recall.exe"
TESTCARD_EXE="$RECALL_ROOT/tools/hdr-testcard/target/debug/hdr-testcard.exe"

APPDATA_WIN="${APPDATA:-C:/Users/$USER/AppData/Roaming}"
HDR_DIR="$APPDATA_WIN/recall-hdr"
CONFIG_TOML="$HDR_DIR/config.toml"
STATE_JSON="$HDR_DIR/state.json"
LOG_FILE="$HDR_DIR/recall-hdr.log"

E2E_DIR="$RECALL_ROOT/tools/e2e"
WORK_DIR="$E2E_DIR/.work"          # scratch: dump PNGs, saved config, oracle out
CONFIG_BACKUP="$WORK_DIR/config.toml.orig"
CONFIG_EXISTED_MARKER="$WORK_DIR/config_existed"

mkdir -p "$WORK_DIR"

# ---------------------------------------------------------------------------
# Tracked spawned PIDs (Windows pids). Never kill by image name.
# ---------------------------------------------------------------------------
APP_PID=""
TESTCARD_PID=""
NOTEPAD_PID=""
# Any transient oracle recall.exe pids we launch get tracked here too.
declare -a EXTRA_PIDS=()

# ---------------------------------------------------------------------------
# Low-level: launch a Windows process, return its real OS pid on stdout.
# Uses PowerShell Start-Process -PassThru; PowerShell writes the Win32 pid to a
# pid-FILE (not stdout) so capture is robust across nested subshells / background
# tasks (a `$(...)`-captured stdout has proven unreliable under the harness).
# The child's stderr/stdout are redirected to a file the caller names. An extra
# RECALL_DUMP env can be passed via $SPAWN_ENV_RECALL_DUMP (Windows path).
# args: <label> <out_file> <exe> [arg...]
# ---------------------------------------------------------------------------
spawn_win() {
  local label="$1"; shift
  local outfile="$1"; shift
  local exe="$1"; shift
  local ps_args=""
  local a
  for a in "$@"; do
    a="${a//\'/\'\'}"   # single-quote-escape for PowerShell
    ps_args+="'$a',"
  done
  ps_args="${ps_args%,}"
  local win_exe; win_exe="$(cygpath -w "$exe" 2>/dev/null || echo "$exe")"
  local win_out; win_out="$(cygpath -w "$outfile" 2>/dev/null || echo "$outfile")"
  local pidfile="$WORK_DIR/spawn_pid_$$.txt"; : > "$pidfile"
  local win_pidfile; win_pidfile="$(cygpath -w "$pidfile")"
  local argclause=""
  [ -n "$ps_args" ] && argclause="-ArgumentList @($ps_args)"
  local envclause=""
  [ -n "${SPAWN_ENV_RECALL_DUMP:-}" ] && envclause="\$env:RECALL_DUMP='${SPAWN_ENV_RECALL_DUMP}'; "
  powershell.exe -NoProfile -Command \
    "${envclause}\$p = Start-Process -FilePath '$win_exe' $argclause -PassThru -RedirectStandardError '$win_out' -RedirectStandardOutput '$win_out.stdout' -WindowStyle Minimized; Set-Content -Path '$win_pidfile' -Value \$p.Id -NoNewline" \
    >/dev/null 2>&1
  local pid; pid="$(tr -d '\r\n ' < "$pidfile" 2>/dev/null)"
  rm -f "$pidfile"
  echo "$pid"
}

# Is a Windows pid alive? (tasklist filtered by PID)
pid_alive() {
  local pid="$1"
  [ -z "$pid" ] && return 1
  local out
  out="$(tasklist //FI "PID eq $pid" //NH 2>/dev/null | tr -d '\r')"
  echo "$out" | grep -q "$pid"
}

# Kill a single Windows pid TREE (child python/engine etc.) + verify gone.
kill_pid_tree() {
  local pid="$1"
  [ -z "$pid" ] && return 0
  if pid_alive "$pid"; then
    taskkill //PID "$pid" //T //F >/dev/null 2>&1 || true
  fi
  # verify
  local i
  for i in 1 2 3 4 5 6 7 8 9 10; do
    pid_alive "$pid" || return 0
    sleep 0.3
  done
  pid_alive "$pid" && return 1 || return 0
}

# ---------------------------------------------------------------------------
# App lifecycle
# ---------------------------------------------------------------------------
start_app() {
  local out="$WORK_DIR/app.out"
  APP_PID="$(spawn_win recall-hdr "$out" "$APP_EXE")"
  sleep 2.0   # let it resolve display, install watcher, start engine, enter pump
  if ! pid_alive "$APP_PID"; then
    echo "  [!] app failed to stay alive (pid=$APP_PID); see $out" >&2
    APP_PID=""
    return 1
  fi
  echo "  app started (pid $APP_PID)"
  return 0
}

stop_app() {
  if [ -n "$APP_PID" ]; then
    kill_pid_tree "$APP_PID" || echo "  [!] app pid $APP_PID did not die" >&2
    APP_PID=""
  fi
}

# ---------------------------------------------------------------------------
# Testcard (fake game). --solid values may exceed 1.0 for HDR-range.
# args: <r,g,b>  (e.g. 2.0,2.0,2.0)
# ---------------------------------------------------------------------------
start_testcard() {
  local solid="${1:-2.0,2.0,2.0}"
  local out="$WORK_DIR/testcard.out"
  TESTCARD_PID="$(spawn_win hdr-testcard "$out" "$TESTCARD_EXE" --fullscreen --solid "$solid")"
  sleep 2.5   # window create + fullscreen + first present + foreground event
  if ! pid_alive "$TESTCARD_PID"; then
    echo "  [!] testcard failed to stay alive (pid=$TESTCARD_PID); see $out" >&2
    TESTCARD_PID=""
    return 1
  fi
  echo "  testcard started (pid $TESTCARD_PID, solid $solid)"
  return 0
}

stop_testcard() {
  if [ -n "$TESTCARD_PID" ]; then
    kill_pid_tree "$TESTCARD_PID" || echo "  [!] testcard pid $TESTCARD_PID did not die" >&2
    TESTCARD_PID=""
  fi
}

# Re-bring the testcard to the foreground (its fullscreen window can lose focus
# to a stray app / our own oracle window). Best-effort; used before a capture so
# the DXGI oracle is more likely to capture the testcard's >1.0 pixels.
foreground_testcard() {
  [ -n "$TESTCARD_PID" ] || return 0
  powershell.exe -NoProfile -Command \
    "\$sig='[DllImport(\"user32.dll\")]public static extern bool SetForegroundWindow(IntPtr h);'; \
     Add-Type -MemberDefinition \$sig -Name Fg -Namespace T -ErrorAction SilentlyContinue; \
     \$p=Get-Process -Id $TESTCARD_PID -ErrorAction SilentlyContinue; \
     if(\$p -and \$p.MainWindowHandle -ne 0){ [T.Fg]::SetForegroundWindow(\$p.MainWindowHandle)|Out-Null }" \
    >/dev/null 2>&1 || true
  sleep 0.6
}

# Start a NORMAL-window notepad and force it to the foreground. The app only
# needs a foreground change to a *non-game* (windowed is fine -- a windowed
# unknown classifies NonGame). We launch Normal (not Minimized) so it can be
# foregrounded, then SetForegroundWindow. Retries handle notepad's slow window.
start_notepad() {
  local out="$WORK_DIR/notepad.out"
  local pidfile="$WORK_DIR/notepad_pid.txt"; : > "$pidfile"
  local win_pidfile; win_pidfile="$(cygpath -w "$pidfile")"
  # Launch + foreground in one PowerShell so the window handle is fresh.
  powershell.exe -NoProfile -Command \
    "\$sig='[DllImport(\"user32.dll\")]public static extern bool SetForegroundWindow(IntPtr h);[DllImport(\"user32.dll\")]public static extern bool ShowWindow(IntPtr h,int c);'; \
     Add-Type -MemberDefinition \$sig -Name Fg -Namespace E -ErrorAction SilentlyContinue; \
     \$p = Start-Process -FilePath 'C:\\Windows\\System32\\notepad.exe' -PassThru -WindowStyle Normal; \
     Set-Content -Path '$win_pidfile' -Value \$p.Id -NoNewline; \
     for(\$i=0;\$i -lt 20;\$i++){ \$p.Refresh(); if(\$p.MainWindowHandle -ne 0){ [E.Fg]::ShowWindow(\$p.MainWindowHandle,5)|Out-Null; [E.Fg]::SetForegroundWindow(\$p.MainWindowHandle)|Out-Null; break }; Start-Sleep -Milliseconds 150 }" \
    >/dev/null 2>&1
  NOTEPAD_PID="$(tr -d '\r\n ' < "$pidfile" 2>/dev/null)"
  rm -f "$pidfile"
  sleep 1.0
  if [ -n "$NOTEPAD_PID" ] && pid_alive "$NOTEPAD_PID"; then
    echo "  notepad started + foregrounded (pid $NOTEPAD_PID)"; return 0
  fi
  echo "  [!] notepad failed to start (pid='$NOTEPAD_PID')" >&2
  NOTEPAD_PID=""; return 1
}

stop_notepad() {
  if [ -n "$NOTEPAD_PID" ]; then
    kill_pid_tree "$NOTEPAD_PID" || echo "  [!] notepad pid $NOTEPAD_PID did not die" >&2
    NOTEPAD_PID=""
  fi
}

# ---------------------------------------------------------------------------
# Oracle 1: DXGI capture-engine HDR verdict + capture >1.0 fraction.
# Launches a SHORT-lived recall.exe with RECALL_DUMP; it prints to stderr:
#   [recall] display: HDR|SDR (max N nits) -> tonemap|passthrough
#   [recall dump] ... pixels >1.0 (HDR-range): X%  =>  source looks HDR|SDR-range
# We wait for the dump line (frame 30, ~<1s), then kill by pid and VERIFY dead.
#
# Subshell-safety: this fn is meant to be called as `$(oracle_dxgi tag)`, so any
# `EXTRA_PIDS+=` here would not survive to the parent. We therefore also append
# every oracle pid to a persistent file (ORACLE_PID_FILE) the cleanup trap reads,
# so a kill that (somehow) slips is still swept at teardown. The kill+verify is
# done here inline regardless.
# ---------------------------------------------------------------------------
ORACLE_PID_FILE="$WORK_DIR/oracle_pids.txt"
: > "$ORACLE_PID_FILE"

# Return the .out path for a given oracle tag (where its stderr was captured).
oracle_file() { echo "$WORK_DIR/${1:-oracle}_engine.out"; }

# Run the DXGI oracle for <tag>. Writes captured engine stderr to the .out file
# (read it via `oracle_file <tag>` or the convenience `oracle_dxgi <tag>`). Does
# NOT rely on command-substitution stdout for the payload -- that has proven
# lossy under nested subshells / background tasks. Returns 0 if a dump was seen.
oracle_dxgi_run() {
  local tag="${1:-oracle}"
  local dump_win="$WORK_DIR/${tag}.png"
  local out; out="$(oracle_file "$tag")"
  : > "$out"
  SPAWN_ENV_RECALL_DUMP="$(cygpath -w "$dump_win" 2>/dev/null || echo "$dump_win")"
  local pid; pid="$(spawn_win recall-oracle "$out" "$ENGINE_EXE")"
  unset SPAWN_ENV_RECALL_DUMP
  [ -n "$pid" ] && echo "$pid" >> "$ORACLE_PID_FILE"
  # Wait up to ~12s for the dump line (frame 30 warmup) to land in stderr.
  local seen=1 i
  for i in $(seq 1 60); do
    if grep -q "recall dump" "$out" 2>/dev/null; then seen=0; break; fi
    [ -n "$pid" ] && ! pid_alive "$pid" && break
    sleep 0.2
  done
  sleep 0.4   # let the trailing [recall] display: line flush after the dump
  [ -n "$pid" ] && { kill_pid_tree "$pid" >/dev/null 2>&1 || taskkill //PID "$pid" //T //F >/dev/null 2>&1 || true; }
  return $seen
}

# Convenience: run + echo the file's contents (safe because we cat the on-disk
# file the parent shell can see; parsers below re-read the file, not this echo).
oracle_dxgi() {
  local tag="${1:-oracle}"
  oracle_dxgi_run "$tag" >/dev/null 2>&1
  cat "$(oracle_file "$tag")" 2>/dev/null
}

# Sweep any oracle recall.exe pids recorded in ORACLE_PID_FILE (cleanup safety).
sweep_oracle_pids() {
  [ -f "$ORACLE_PID_FILE" ] || return 0
  local p
  while read -r p; do
    [ -n "$p" ] && pid_alive "$p" && { taskkill //PID "$p" //T //F >/dev/null 2>&1 || true; }
  done < "$ORACLE_PID_FILE"
  : > "$ORACLE_PID_FILE"
}

# Parse the DXGI verdict (HDR|SDR) from an oracle_dxgi capture (stdin).
# stdout: "HDR" | "SDR" | "UNKNOWN"
parse_dxgi_hdr() {
  local txt; txt="$(cat)"
  if echo "$txt" | grep -qE "\[recall\] display: HDR"; then echo "HDR"
  elif echo "$txt" | grep -qE "\[recall\] display: SDR"; then echo "SDR"
  else echo "UNKNOWN"; fi
}

# Parse the >1.0 HDR-range percentage from stdin. stdout: float (or -1 on miss).
# NB: extract the number immediately before '%' -- the label itself contains
# ">1.0", so a naive "first float" grep wrongly returns 1.0.
parse_over_one_pct() {
  local txt; txt="$(cat)"
  local v
  v="$(echo "$txt" | grep -oE "\(HDR-range\): [0-9]+\.[0-9]+%" \
        | grep -oE "[0-9]+\.[0-9]+" | head -1)"
  echo "${v:--1}"
}

# File-based variants (robust: read the on-disk .out for <tag>, never a captured
# stdout that a background/subshell context may have truncated).
dxgi_verdict() { parse_dxgi_hdr    < "$(oracle_file "${1:-oracle}")"; }
dxgi_over_one(){ parse_over_one_pct < "$(oracle_file "${1:-oracle}")"; }

# ---------------------------------------------------------------------------
# Oracle 2: NvAPI clamp state via `recall-hdr --probe` (read-only).
# Prints "CSC GET: ... active=true|false". stdout: "true" | "false" | "unknown"
# ---------------------------------------------------------------------------
oracle_clamp_active() {
  local out
  out="$("$APP_EXE" --probe 2>&1)"
  echo "$out" > "$WORK_DIR/last_probe.out"
  if echo "$out" | grep -qE "CSC GET:.*active=true"; then echo "true"
  elif echo "$out" | grep -qE "CSC GET:.*active=false"; then echo "false"
  else echo "unknown"; fi
}

# ---------------------------------------------------------------------------
# Journal (state.json) inspection
# ---------------------------------------------------------------------------
journal_exists()   { [ -f "$STATE_JSON" ]; }
journal_hdr_touched() {
  [ -f "$STATE_JSON" ] || { echo "false"; return; }
  if grep -qE '"hdr_touched":[[:space:]]*true' "$STATE_JSON"; then echo "true"; else echo "false"; fi
}
journal_any_touched() {
  [ -f "$STATE_JSON" ] || { echo "false"; return; }
  if grep -qE '"(hdr|clamp|sdr_white)_touched":[[:space:]]*true' "$STATE_JSON"; then echo "true"; else echo "false"; fi
}

# ---------------------------------------------------------------------------
# Log tailing by byte offset -- read only lines produced since a mark.
# ---------------------------------------------------------------------------
log_mark() { wc -c < "$LOG_FILE" 2>/dev/null | tr -d ' ' || echo 0; }
log_since() {
  local off="${1:-0}"
  [ -f "$LOG_FILE" ] || return 0
  tail -c "+$((off + 1))" "$LOG_FILE" 2>/dev/null
}

# ---------------------------------------------------------------------------
# Assertions -- each records PASS/FAIL into the running evidence buffer.
# ---------------------------------------------------------------------------
ASSERT_FAILS=0
EVIDENCE=""
ev() { EVIDENCE+="$1"$'\n'; echo "    $1"; }

assert_eq() {  # <desc> <got> <want>
  if [ "$2" = "$3" ]; then ev "PASS $1 (=$2)"; else ev "FAIL $1 (got '$2' want '$3')"; ASSERT_FAILS=$((ASSERT_FAILS+1)); fi
}
assert_contains() {  # <desc> <haystack> <needle>
  if echo "$2" | grep -qF "$3"; then ev "PASS $1"; else ev "FAIL $1 (missing: $3)"; ASSERT_FAILS=$((ASSERT_FAILS+1)); fi
}
assert_matches() {  # <desc> <haystack> <regex>
  if echo "$2" | grep -qE "$3"; then ev "PASS $1"; else ev "FAIL $1 (no match: $3)"; ASSERT_FAILS=$((ASSERT_FAILS+1)); fi
}
assert_not_matches() {  # <desc> <haystack> <regex>
  if echo "$2" | grep -qE "$3"; then ev "FAIL $1 (unexpected match: $3)"; ASSERT_FAILS=$((ASSERT_FAILS+1)); else ev "PASS $1"; fi
}
assert_true()  { if [ "$2" = "true" ];  then ev "PASS $1"; else ev "FAIL $1 (got '$2')"; ASSERT_FAILS=$((ASSERT_FAILS+1)); fi; }
assert_false() { if [ "$2" = "false" ]; then ev "PASS $1"; else ev "FAIL $1 (got '$2')"; ASSERT_FAILS=$((ASSERT_FAILS+1)); fi; }

# ---------------------------------------------------------------------------
# Config backup / restore (byte-identical). Records whether one existed.
# ---------------------------------------------------------------------------
backup_config() {
  if [ -f "$CONFIG_TOML" ]; then
    cp -f "$CONFIG_TOML" "$CONFIG_BACKUP"
    touch "$CONFIG_EXISTED_MARKER"
    echo "  backed up existing config.toml"
  else
    rm -f "$CONFIG_EXISTED_MARKER"
    echo "  no pre-existing config.toml (app was running on defaults)"
  fi
}
restore_config() {
  if [ -f "$CONFIG_EXISTED_MARKER" ]; then
    cp -f "$CONFIG_BACKUP" "$CONFIG_TOML"
    echo "  restored original config.toml (byte-identical)"
  else
    rm -f "$CONFIG_TOML"
    echo "  removed harness config.toml (none existed originally)"
  fi
}

# Write a config with a single override for hdr-testcard.
# args: <rule_kind: hdr_game|sdr_game|none> [clamp: punchy|accurate|off]
write_config_override() {
  local kind="$1"; local clamp="${2:-}"
  {
    echo "version = 1"
    echo ""
    echo "[display]"
    echo 'target_friendly_name = "PG27AQDP"'
    echo ""
    echo "[behavior]"
    echo 'default_game_class = "sdr_game"'
    echo 'default_clamp = "punchy"'
    echo "sdr_white_nits = 220"
    echo "debounce_ms = 400"
    echo ""
    echo "[hotkey]"
    echo 'toggle_hdr = "Ctrl+Alt+H"'
    echo ""
    echo "[startup]"
    echo "autostart = true"
    echo ""
    echo "[[overrides]]"
    echo 'exe = "hdr-testcard"'
    echo "kind = \"$kind\""
    [ -n "$clamp" ] && echo "clamp = \"$clamp\""
  } > "$CONFIG_TOML"
}

# ---------------------------------------------------------------------------
# Master cleanup -- ALWAYS run on EXIT. Kills every tracked pid, runs --restore,
# restores the user's config, and reports the final verified state.
# ---------------------------------------------------------------------------
final_state_report() {
  echo ""
  echo "=== FINAL STATE VERIFICATION ==="
  local clamp; clamp="$(oracle_clamp_active)"
  echo "  clamp active (probe oracle) : $clamp"
  oracle_dxgi_run finalstate; local dxgi; dxgi="$(dxgi_verdict finalstate)"
  echo "  DXGI HDR verdict            : $dxgi"
  echo "  journal state.json present  : $(journal_exists && echo yes || echo no)"
  echo -n "  tracked pids still alive    :"
  local anyleft=0 p
  for p in "$APP_PID" "$TESTCARD_PID" "$NOTEPAD_PID" "${EXTRA_PIDS[@]}"; do
    if [ -n "$p" ] && pid_alive "$p"; then echo -n " $p"; anyleft=1; fi
  done
  [ "$anyleft" = 0 ] && echo " none" || echo " <-- LEAK"
  echo "================================"
}

cleanup() {
  echo ""
  echo ">>> CLEANUP (trap): tearing down all spawned processes + restoring state"
  stop_testcard
  stop_notepad
  stop_app
  # Kill any transient oracle recall.exe pids we launched (subshell-safe sweep).
  sweep_oracle_pids
  local p
  for p in "${EXTRA_PIDS[@]:-}"; do
    [ -n "$p" ] && pid_alive "$p" && kill_pid_tree "$p" >/dev/null 2>&1 || true
  done
  # Journal-backed repair: guarantee the display is off HDR / no clamp.
  echo "  running recall-hdr --restore ..."
  "$APP_EXE" --restore >/dev/null 2>&1 || true
  # Restore the user's original config byte-identical.
  restore_config
  final_state_report
}
