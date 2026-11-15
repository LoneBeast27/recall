# windows-capture 2.0.0, patched for Recall

Upstream: https://crates.io/crates/windows-capture 2.0.0 (MIT, see LICENCE). Two changes:

1. `src/graphics_capture_api.rs`: the frame pool has 2 buffers instead of 1 (`FRAME_BUFFERS`), in
   `Create` and `Recreate`.
2. `src/frame.rs`: `Frame::capture_frame()` returns the WinRT `Direct3D11CaptureFrame`.

Why: with NV12 recording frames (4:2:0, studio range), Recall keeps the newest captured frame alive
until the next one arrives, so a screenshot copies the exact BGRA picture from its surface only
when one is asked for, with no copy per frame. With one buffer, holding a frame would stop capture.
