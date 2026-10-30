"""HUD scan over the clip library: where each game's HUD sits along the top edge,
for the Recall Island's shortcut-hint card (the `hud` entries of
recall-overlay/assets/hud-spots.json). The island's own spots and scale stay
on always-on HUD (the first scan's table): the user's ruling, 2026-10-30, was
"Only for the hint card".

HUD that comes and goes counts (kill feeds, alerts), not only HUD that is always
there. For each game, short bursts of 3 frames 0.2 s apart are sampled across
many clips (matches, maps). Only bursts in which the world clearly moved count
(the camera turned): in those, a pixel that keeps an edge in every frame at an
unchanged brightness is HUD on screen at that moment. A pixel is occupied when
that holds in at least SHARE of the game's moving bursts (and in at least
MIN_BURSTS of them) - a kill feed is up for a good part of active play - or
when the first scan's rule (an edge in the median of every sampled frame:
HUD that is always there) marks it, so the free depth can only shrink.
(Counting every burst is not enough: the camera is still often enough, in buy
phases and while aiming, that world edges pass 5% almost everywhere.) Last,
specks are dropped: an occupied pixel stays only where its 9x9 neighbourhood
holds at least DENSE occupied pixels (HUD text, banners and panels do; a few
stray edges along the top do not).

Each game's raw counts are saved (<key>.npz), so `--reprocess` can redo the
rule without reading the clips again.

Then the HUD grid: for every 32 px column at 1440p, one 64-bit number whose
bit r says the 8 px rows [r, r + 1) x 8 hold an occupied pixel (the top 512
rows; no card reaches deeper). The overlay adds up the cells under each hint
card to pick the one covering the least HUD. This grid picks the same card as
the full mask in all 14 table entries (checked 2026-10-30).

Gentle by design (the user's PC, and the clips sit on an HDD): run it through
nice-build.ps1 (background cores, BelowNormal); each ffmpeg child also gets very
low I/O priority and 2 threads; it never writes next to the clips; and it waits
while a game is running or a fullscreen app is in front.

    powershell -NoProfile -File ~/.claude/tools/nice-build.ps1 python scripts/dev/hud-scan.py "V:/ALL RECORDINGS" OUT_DIR [--old OLD_RESULTS.json]
"""
import argparse
import ctypes
import json
import math
import random
import shutil
import subprocess
import time
from ctypes import wintypes
from pathlib import Path

import numpy as np

W, H, SCALE = 640, 360, 4  # analysis size; results reported at 1440p
EDGE_THRESHOLD = 9.0  # median-image gradient (as the first scan)
SHARE = 0.05
MIN_BURSTS = 3
MOTION = 6.0  # mean grey change between a burst's first and last frame: the world moved
STEADY = 16.0  # a HUD pixel's brightness stays within this through a burst
DENSE = 12  # occupied pixels in a pixel's 9x9 neighbourhood for it to count (drops specks)
BURST_FRAMES, BURST_STEP = 3, 12  # every 12th frame at 60 fps: 0.2 s apart
FOOTPRINT = 400  # reported depth: under a 400 px wide box at each anchor, 12 px in from the edge
BIN = 32  # skyline and HUD grid column width at 1440p
HUD_ROW, HUD_ROWS = 8, 64  # the HUD grid's rows: 8 px at 1440p, 64 bits per column
GAMES = {  # game key -> exe names that mean "the user is playing" (any of these pauses the scan)
    "valorant": "VALORANT-Win64-Shipping.exe", "rocketleague": "RocketLeague.exe", "apexlegends": "r5apex.exe",
    "overwatch": "Overwatch.exe", "overwatch2": "Overwatch.exe", "marvelrivals": "Marvel-Win64-Shipping.exe",
    "genshinimpact": "GenshinImpact.exe", "battlefield6": "bf6.exe", "counterstrike2": "cs2.exe",
    "tomclancysrainbowsixsiege": "RainbowSix.exe", "counterstrikeglobaloffensive": "csgo.exe",
    "forzahorizon6": "ForzaHorizon6.exe", "lethalcompany": "Lethal Company.exe",
}
NO_WINDOW, BELOW_NORMAL = 0x08000000, 0x00004000


def game_key(name):
    return "".join(c.lower() for c in name if c.isascii() and c.isalnum())


def very_low_io(pid_handle):
    """ProcessIoPriority (33) = 0: very low I/O priority for an ffmpeg child."""
    value = ctypes.c_ulong(0)
    ctypes.windll.ntdll.NtSetInformationProcess(wintypes.HANDLE(int(pid_handle)), 33, ctypes.byref(value), 4)


def fullscreen_app():
    """The foreground window covers its whole monitor (and is not the desktop)."""
    user32 = ctypes.windll.user32
    hwnd = user32.GetForegroundWindow()
    if not hwnd or hwnd in (user32.GetShellWindow(), user32.GetDesktopWindow()):
        return None
    rect = wintypes.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(rect))

    class MONITORINFO(ctypes.Structure):
        _fields_ = [("cbSize", wintypes.DWORD), ("rcMonitor", wintypes.RECT), ("rcWork", wintypes.RECT), ("dwFlags", wintypes.DWORD)]

    info = MONITORINFO(cbSize=ctypes.sizeof(MONITORINFO))
    user32.GetMonitorInfoW(user32.MonitorFromWindow(hwnd, 2), ctypes.byref(info))
    m = info.rcMonitor
    if (rect.left, rect.top, rect.right, rect.bottom) != (m.left, m.top, m.right, m.bottom):
        return None
    name = ctypes.create_unicode_buffer(256)
    user32.GetClassNameW(hwnd, name, 256)
    return None if name.value in ("Progman", "WorkerW") else name.value


def playing():
    """Why to wait now, or None."""
    out = subprocess.run(["tasklist", "/FO", "CSV", "/NH"], capture_output=True, text=True, creationflags=NO_WINDOW).stdout
    running = {line.split(",")[0].strip('"').lower() for line in out.splitlines() if line}
    games = {exe for exe in GAMES.values() if exe.lower() in running}
    if games:
        return "game running: " + ", ".join(sorted(games))
    full = fullscreen_app()
    return f"fullscreen app in front ({full})" if full else None


def wait_while_playing(log):
    reason = playing()
    while reason:
        log(f"paused: {reason}")
        time.sleep(15)
        reason = playing()


def duration(ffprobe, path):
    out = subprocess.run([ffprobe, "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", str(path)],
                         capture_output=True, text=True, creationflags=NO_WINDOW)
    try:
        return float(out.stdout.strip())
    except ValueError:
        return 0.0


def burst(ffmpeg, path, at):
    """3 grey frames 0.2 s apart from `at`, or None."""
    cmd = [ffmpeg, "-v", "error", "-threads", "2", "-ss", f"{at:.2f}", "-i", str(path), "-frames:v", str(BURST_FRAMES),
           "-vf", f"select='not(mod(n\\,{BURST_STEP}))',scale={W}:{H}", "-fps_mode", "passthrough",
           "-pix_fmt", "gray", "-f", "rawvideo", "-"]
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, creationflags=NO_WINDOW | BELOW_NORMAL)
    very_low_io(proc._handle)
    data, _ = proc.communicate()
    if len(data) != W * H * BURST_FRAMES:
        return None
    return np.frombuffer(data, dtype=np.uint8).reshape(BURST_FRAMES, H, W).astype(np.float32)


def tidy(mask):
    """Drop isolated pixels (keep 3+ of 8 neighbours) and the frame's top row."""
    padded = np.pad(mask, 1).astype(np.int8)
    neighbours = sum(np.roll(np.roll(padded, dy, 0), dx, 1) for dy in (-1, 0, 1) for dx in (-1, 0, 1)) - padded
    mask = mask & (neighbours[1:-1, 1:-1] >= 3)
    mask[0, :] = False  # one-sided gradient noise at the frame edge
    return mask


def edges(image):
    gy, gx = np.gradient(image)
    return np.hypot(gx, gy)


def steady_edges(frames):
    """While the world moved: edges in every frame at an unchanged brightness
    (HUD on screen at that moment). None when the world held still."""
    if np.abs(frames[-1] - frames[0]).mean() < MOTION:
        return None
    each = np.min(np.stack([edges(f) for f in frames]), axis=0) > EDGE_THRESHOLD
    steady = (frames.max(axis=0) - frames.min(axis=0)) < STEADY
    return tidy(each & steady)


def dense(mask, radius=4):
    """Keep occupied pixels with at least DENSE occupied pixels in their (2r+1)^2 box."""
    integral = np.pad(mask.astype(np.int32).cumsum(0).cumsum(1), ((1, 0), (1, 0)))
    y0 = np.clip(np.arange(H) - radius, 0, H)
    y1 = np.clip(np.arange(H) + radius + 1, 0, H)
    x0 = np.clip(np.arange(W) - radius, 0, W)
    x1 = np.clip(np.arange(W) + radius + 1, 0, W)
    box = integral[y1][:, x1] - integral[y0][:, x1] - integral[y1][:, x0] + integral[y0][:, x0]
    return mask & (box >= DENSE)


def skyline(mask):
    """Free rows at 1440p per BIN-px column: down to the first occupied pixel."""
    first = np.where(mask.any(axis=0), mask.argmax(axis=0), H) * SCALE  # per analysis column
    per_bin = BIN // SCALE
    return [int(first[i:i + per_bin].min()) for i in range(0, W, per_bin)]


def hud_grid(mask):
    """Per BIN-px column at 1440p, a number: bit r = rows [r, r + 1) x HUD_ROW hold an occupied pixel."""
    rows, columns = HUD_ROW // SCALE, BIN // SCALE

    def bits(x):
        return sum(1 << r for r in range(HUD_ROWS) if mask[r * rows:(r + 1) * rows, x:x + columns].any())

    return [bits(x) for x in range(0, W, columns)]


def under(sky, x0, x1):
    """The skyline's free rows under [x0, x1) at 1440p."""
    return min(sky[int(x0 // BIN):int(-(-x1 // BIN))])


def footprints(sky):
    width = W * SCALE
    return {"left": under(sky, 12, 12 + FOOTPRINT), "centre": under(sky, (width - FOOTPRINT) / 2, (width + FOOTPRINT) / 2),
            "right": under(sky, width - 12 - FOOTPRINT, width - 12)}


def clip_date(clip):
    parts = clip.name.split("__")
    return parts[1] if len(parts) > 1 else ""


def pick(clips, clips_per_game, since):
    """Clips from `since` on (today's HUD and overlays, not an old layout or
    an old on-screen display), spread over that span; the newest ones when a
    game has too few recent clips."""
    clips = sorted(clips, key=clip_date)
    recent = [c for c in clips if clip_date(c) >= since]
    pool = recent if len(recent) >= clips_per_game else clips[-clips_per_game:]
    step = max(1, len(pool) // clips_per_game)
    return pool[::step][:clips_per_game]


def scan_game(ffmpeg, ffprobe, clips, log):
    """(steady-edge counts over moving bursts, moving bursts, every burst's first frame)."""
    count, moving, firsts = np.zeros((H, W), np.int32), 0, []
    for clip, bursts_per_clip in clips:
        d = duration(ffprobe, clip)
        if d < 6:
            continue
        for k in range(bursts_per_clip):
            wait_while_playing(log)
            frames = burst(ffmpeg, clip, d * (k + 0.5) / bursts_per_clip)
            if frames is None:
                continue
            firsts.append(frames[0])
            mask = steady_edges(frames)
            if mask is not None:
                count += mask
                moving += 1
    return count, moving, firsts


def write_ppm(path, rgb):
    with open(path, "wb") as f:
        f.write(f"P6 {rgb.shape[1]} {rgb.shape[0]} 255\n".encode())
        f.write(rgb.tobytes())


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("clips_dir")
    parser.add_argument("out_dir")
    parser.add_argument("--clips", type=int, default=10)
    parser.add_argument("--bursts", type=int, default=8)
    parser.add_argument("--old", help="the first scan's results.json, for the comparison")
    parser.add_argument("--only", help="scan this game key only (a smoke run)")
    parser.add_argument("--reprocess", action="store_true", help="redo the rule from the saved <key>.npz counts")
    parser.add_argument("--since", default=time.strftime("%Y-%m-%d", time.localtime(time.time() - 365 * 86400)),
                        help="sample clips recorded from this date on (default: the last 12 months)")
    args = parser.parse_args()
    ffmpeg, ffprobe = shutil.which("ffmpeg"), shutil.which("ffprobe")
    out = Path(args.out_dir)
    out.mkdir(parents=True, exist_ok=True)
    log_file = (out / "scan.log").open("a", encoding="utf-8")

    def log(message):
        line = f"{time.strftime('%H:%M:%S')} {message}"
        print(line, flush=True)
        log_file.write(line + "\n")
        log_file.flush()

    old = json.loads(Path(args.old).read_text(encoding="utf-8")) if args.old else {}
    games = {}
    for clip in Path(args.clips_dir).glob("*.mp4"):
        key = game_key(clip.name.split("__")[0])
        if key in GAMES and (not args.only or key == args.only) and clip.stat().st_size > 20_000_000:
            games.setdefault(key, []).append(clip)
    random.seed(7)
    results = {}
    for key, clips in sorted(games.items(), key=lambda kv: -len(kv[1])):
        started = time.time()
        raw = out / f"{key}.npz"
        if args.reprocess:
            if not raw.exists():
                continue
            saved = np.load(raw)
            count, n, median, picked = saved["count"], int(saved["moving"]), saved["median"], [Path(c) for c in saved["sampled"]]
            frames = int(saved["frames"])
        else:
            picked = pick(clips, args.clips, args.since)
            count, n, firsts = scan_game(ffmpeg, ffprobe, [(c, args.bursts) for c in picked], log)
            if n < (2 if args.only else 10):
                log(f"{key}: only {n} moving bursts, skipped")
                continue
            median, frames = np.median(np.stack(firsts), axis=0), len(firsts)
            np.savez_compressed(raw, count=count, moving=n, median=median.astype(np.float32), frames=frames,
                                sampled=np.array([c.name for c in picked]))
        need = max(MIN_BURSTS, math.ceil(SHARE * n))
        median = median.astype(np.float32)
        always = dense(tidy(edges(median) > EDGE_THRESHOLD))  # the first scan's rule
        mask = always | dense(count >= need)
        sky = skyline(mask)
        free = footprints(sky)
        before = next((v for name, v in old.items() if game_key(name) == key), None)
        results[key] = {"clips": len(clips), "sampled": [c.name for c in picked], "frames": frames, "moving_bursts": n, "occupied_if_in_bursts": need,
                        "free_1440p": free,
                        "skyline_1440p": sky,
                        "hud_1440p": hud_grid(mask),
                        "first_scan_free_1440p": before and {"centre": before["free_top_island_px_1440p"],
                                                             "left": before["free_top_left_px_1440p"],
                                                             "right": before["free_top_right_px_1440p"]}}
        rgb = np.stack([median] * 3, axis=-1)
        share = count / n
        rgb[mask] = [255, 40, 40]  # comes-and-goes HUD in red
        rgb[always] = [255, 200, 40]  # always-there HUD (the first scan's rule) in amber
        del share
        write_ppm(out / f"{key}.ppm", rgb.astype(np.uint8))
        log(f"{key}: {n} moving bursts in {time.time() - started:.0f}s, free {free}")
        (out / "results.json").write_text(json.dumps(results, indent=1), encoding="utf-8")
    grids = {key: {"hud": r["hud_1440p"]} for key, r in results.items()}
    if "tomclancysrainbowsixsiege" in grids:
        grids["rainbowsixsiege"] = grids["tomclancysrainbowsixsiege"]
    rule = (f"an edge in the median of all sampled frames, or steady edges through 3-frame bursts in >= {SHARE:.0%} "
            f"(and >= {MIN_BURSTS}) of a game's bursts in which the world moved")
    source = (f"hud grid: scripts/dev/hud-scan.py over V:\\ALL RECORDINGS, {time.strftime('%Y-%m-%d')}; per {BIN} px column at 1440p, bit r = {HUD_ROW} px row r holds HUD, "
              f"occupied = {rule} (HUD that comes and goes counts); used only by the shortcut hint card")
    (out / "hud-spots.json").write_text(json.dumps({"_source": source, "spots": grids}, indent=1), encoding="utf-8")
    log("done")


if __name__ == "__main__":
    main()
