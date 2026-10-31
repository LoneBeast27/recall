# VALORANT frame times, Recall ON vs OFF, zero-copy build (2026-11-01)

Installed build: recall-hdr.exe and recall-overlay.exe dated 2026-10-31 22:23 IST, recall.exe dated 2026-10-30 11:24 IST
(contains the GPU path; master 2bdaf74). Same VALORANT process (pid 46816) for both runs. Only change between runs:
the three Recall processes were stopped at 02:12:55 IST. Afterburner, RTSS and Process Lasso were running in both.
Method as in `.planning/BUG_valorant-freezes_2026-10-26.md`: PresentMon, 300 s each, first 25 s skipped.

## Result

All frames, the same report as 26 Oct:

| | 1 Nov ON | 1 Nov OFF | 26 Oct ON | 26 Oct OFF |
|---|---|---|---|---|
| frames over 50 ms | 15 (3.3/min) | 11 (2.4/min) | 61 (13.3/min) | 2 (0.4/min) |
| frames over 20 ms | 17 | 14 | 69 | 3 |
| 1% low | 563 fps | 565 fps | 312 fps | 694 fps |
| 0.1% low | 296 fps | 245 fps | 168 fps | 305 fps |
| worst frame | 741.7 ms | 20,607 ms | 775 ms | 112 ms |
| median | 1.13 ms | 1.17 ms | 1.16 ms | 1.03 ms |
| flip mode of the frames over 50 ms | 15 Composed: Flip | 10 Composed: Flip, 1 Independent Flip | 58 of 60 Independent Flip (BUG doc) | 2 Composed: Flip |

Full-screen play only (`Hardware: Independent Flip` frames), which is where the 26 Oct freezes happened:

| | 1 Nov ON | 1 Nov OFF | 26 Oct ON | 26 Oct OFF |
|---|---|---|---|---|
| seconds of full-screen play | 231 | 236 | 275 | 275 |
| frames over 50 ms | **0** | 1 (0.3/min) | **59 (12.9/min)** | 0 |
| frames over 20 ms | 0 | 4 | 67 | 1 |
| 1% low | 570 fps | 566 fps | 313 fps | 695 fps |
| 0.1% low | 294 fps | 248 fps | 170 fps | 306 fps |
| worst frame | 13.1 ms | 69.3 ms | 775.4 ms | 21.1 ms |

Reference, as given in the brief, not recomputed: 26 Oct 07:26-07:54, about 9 frames at or over 50 ms per minute, 1% low about 240.

## Reading

- The 26 Oct freeze (long `MsCPUBusy` during Independent Flip) did not appear with Recall ON: 0 in 231 s against 12.9/min.
- The all-frames count of 3.3/min is not the same freeze. All 15 frames are `Composed: Flip`, at switches in and out of
  composition. The 26 Oct Recall OFF run also shows Composed stretches with 102-112 ms frames.
- Composed stretches, ON: 02:08:27-02:09:01 and 02:11:26-02:12:03 (39,652 frames in total, game still at about 800-1000 fps).
  The second stretch contains Rohan's "ok" message, sent 02:11:57 IST. Inferred, not observed: Rohan had left the game window to reply.
- Composed stretches, OFF: 02:14:50-02:14:53, 02:15:46-02:16:13 and 02:16:18-02:16:26 (17 frames, game almost stopped
  presenting; this is the 20.6 s worst frame). Cause unknown. No message was sent then.
- The one full-screen spike over 50 ms was 69 ms at 02:17:45, with Recall OFF.
- Today's 1% low with Recall OFF (566) is lower than on 26 Oct (695), in both full-screen and all-frames numbers. Today's
  ON/OFF pair matches, so Recall is not the cause of that gap; the cause is not measured.

## Recall logs (`%APPDATA%\recall-hdr\recall-hdr.log`)

- GPU zero-copy: `capture: zero-copy GPU path (D3D11 tonemap -> NVENC)` at engine start (01:37:23 IST, pid 21924).
- CPU fallback lines (`encoder takes no GPU frames`) since that engine start: 0.
- `capture resolution changed; reopening NVENC` during the ON run: 0.
- Earlier today in Counter-Strike 2 the same reopen line did occur, with a failed encoder init followed by recovery. Not part of this test.

## Limits

- 1 pair of 5-minute runs in 1 game session, not a repeat series.
- PresentMon ran unelevated, as a Performance Log Users member, by pid. The Recall ON run was restarted once at 02:07:51
  because a 0-byte file size was misread as a failed capture. The discarded 34 s run is `on\frames-on-020716.csv`.
- The in-game situation in each run (round phase, map, deaths) was not logged.

## State left behind

- Recorder restarted at 02:18:18 IST: recall-hdr pid 35604, recall.exe pid 4824, recall-overlay pid 43932. It picked up
  VALORANT at 02:19:18 on the zero-copy path.
- Autostart unchanged: off (StartupApproved first byte 3).

## Data

`L:\General\.tmp\valorant-ab-2026-11-01\on\frames-on-020750.csv`, `...\off\frames-off-021307.csv`, plus `run.log` in each folder.
