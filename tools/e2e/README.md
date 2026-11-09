# recall-hdr T1–T6 live test harness

Closed-loop, on-hardware verification of the recall-hdr display-manager against
the design's T1–T6. Drives the real tray app, a fake game (hdr-testcard), and
**two independent oracles** — the DXGI capture engine (panel HDR state + captured
`>1.0` fraction) and the NvAPI `--probe` (gamut-clamp state) — and asserts the
app's behaviour against both the oracles and its own timestamped log.

> **Display-state testing is authorized.** Screens flash on HDR flips — expected.
> The harness never kills by image name (other `recall*` processes may exist); it
> tracks the exact Windows pids it spawns. A cleanup trap **always** tears every
> spawned process down, runs `recall-hdr --restore`, and restores the user's
> `config.toml` byte-identical — even on failure.

## Prerequisites (built once)

```bash
# from the repo root
cargo build -p recall-hdr                         # target/debug/recall-hdr.exe
# capture-engine oracle (already built as release):  target/release/recall.exe
# fake game (standalone crate):
( cd tools/hdr-testcard && cargo build )           # target/debug/hdr-testcard.exe
```

## Run

```bash
# Git Bash. All tests:
export RECALL_E2E_MONITOR="<your HDR monitor's friendly name>"   # required
bash tools/e2e/run_t_tests.sh
# A subset:
bash tools/e2e/run_t_tests.sh T1 T5
```

Tests: `T1 T2 T3 T2b T5 T6`. Output is a per-test PASS/FAIL/PARTIAL/SKIPPED table
with log + oracle evidence lines, plus a final independent state verification
(probe + DXGI + tracked-pid liveness). Scratch (dump PNGs, oracle stderr, saved
config, per-run logs) lands in `tools/e2e/.work/` (gitignored).

## What each test proves

| Test | Feature | Evidence |
|------|---------|----------|
| **T1**  | HDR flip on fullscreen HDR game | log `set_hdr_state(…, true)` + `set_sdr_white`; DXGI verdict HDR; capture `>1.0` fraction; journal `hdr_touched=true` |
| **T2**  | revert to SDR on game exit | log `session pid … exited` + `set_hdr_state(…, false)`; DXGI SDR; journal cleared |
| **T3**  | alt-tab persistence | HDR stays on across a windowed non-game (no `DisableHdr`); then revert on game kill |
| **T2b** | SDR-game accurate clamp | no HDR flip; `NvApiClamp.apply(Accurate)`; `--probe` clamp active=true; clears on exit |
| **T5**  | crash-restore | hard-kill leaves display HDR + journal dirty; `--restore` → SDR + journal cleared |
| **T6**  | tray/config round-trip | HKCU Run mirror (read-only reg query); UI clicks SKIPPED(manual) |

## Files

- `run_t_tests.sh` — the test driver (T1–T6 + summary table).
- `lib.sh` — helpers: pid-tracked spawn/kill, the two oracles, log-tailing by
  byte offset, assertions, config backup/restore, and the cleanup trap.
