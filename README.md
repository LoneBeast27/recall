# Recall

Lean, HDR-aware multi-track clip recorder + live HDR→SDR source for Windows, in Rust.
A nod to (and the opposite of) Windows Recall — this one captures the gameplay you *want* to keep.

Full spec: `Agents/Claude/PRDs/recall-hdr-clip-tool-PRD.md` (in the Docker Speed Drive workspace).

## Phase 0 (current focus)
A live **HDR→SDR output window** titled `<Game> SDR` that **Discord screen-share** and **OBS window-capture**
can pick up correctly (no washed-out HDR). Video only; replay buffer / multi-track encode / gallery come later.

Stack: `windows-capture` (WGC) · `wgpu` (HDR→SDR tonemap) · `winit` (output window).

## Workspace layout
- `recall/` — capture core (this app).
- `recall-hdr/` — HDR display-manager companion utility, built in a separate session
  (see `recall-hdr-manager-build-brief.md`). Adds itself to this workspace's members.

## Roadmap
- **P0** live SDR source window (Discord/OBS pickup)
- **P1** RAM replay buffer → CFR NVENC clip → Moments DB inject + instant feedback overlay
- **P2** multi-track audio (endpoint/cable model)
- **P3** auto-detect/triggers, buffer-config
- **P4** own gallery; virtual-cam/NDI; streaming
