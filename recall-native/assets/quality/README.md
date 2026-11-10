# Quality comparison frames

Shown by the (i) beside Settings → Clip → Bitrate and Encoder effort
(`src/views/quality_preview.rs`), and on the setup page's quality step.

Every image is a real NVENC recording, not a mock-up: `tools/quality-previews/make-previews.ps1`
turns a still panorama into a lossless 1440p60 clip of a fast camera turn, records it with the
engine's NVENC settings once per format × bitrate × effort (36 recordings), and crops the same
spot of the same frame out of each. `<codec>-<mbps>-<effort>.png` is that crop, zoomed 2x;
`original.png` is the same crop of the lossless clip. The setup page's full frames
(`*-frame.png`, 640 × 360) are H.264 at Balanced. `previews.json` records the script's
arguments, and `quality_preview_tests.rs` checks that no two tabs share an image and that every
crop and full frame is the same frame.

Source picture: "Abandoned Hopper Terminal 04" by Poly Haven (tonemapped 8K JPG), <https://polyhaven.com/a/abandoned_hopper_terminal_04>, CC0 (Poly Haven). Not stored in the repo.
Arguments used: `-Yaw 110 -Pitch 5 -CropX 660 -CropY 600 -Size 136`.
