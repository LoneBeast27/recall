"""Check a clip recorded from examples/present_target.rs (scripts/dev/engine-cost.ps1 -SaveSeconds N).

Reads, per decoded frame, the target's 24-bit frame counter (top strip) and the six colour
swatches (left column), and prints JSON: streams, frame count, counter continuity (no
repeats, never backwards, the step between captured frames), and the median decoded colour
of each swatch next to the colour the target drew.

With `--content W H` (a `--decorated` target: Recall records its W x H client area centred on
the canvas, black around it) the pattern is read at the content's place, and `letterbox`
reports the bars (every bar row's peak level, over every 10th frame) and the content's edges:
its first row must carry the frame counter (a title bar there would not), and its first
column the swatch colours (a border would not).

    python -I scripts/dev/check-clip.py <clip.mp4> [--content W H] [--ffmpeg <dir with ffmpeg.exe/ffprobe.exe>]
"""
import json
import os
import statistics
import subprocess
import sys

SWATCHES = [("red", (255, 0, 0)), ("green", (0, 255, 0)), ("blue", (0, 0, 255)),
            ("white", (255, 255, 255)), ("grey128", (128, 128, 128)), ("black", (0, 0, 0))]


def run(cmd):
    return subprocess.run(cmd, capture_output=True, check=True).stdout


def rows(ffmpeg, clip, w, h, x, y, every=1):
    """Decoded RGB24 rows of the w x h region at (x, y): a list of frames, each a list of rows."""
    if w <= 0 or h <= 0:
        return []
    vf = f"format=rgb24,crop={w}:{h}:{x}:{y}"
    if every > 1:
        vf = f"select=not(mod(n\\,{every})),{vf}"
    data = run([ffmpeg, "-v", "error", "-i", clip, "-map", "0:v:0", "-vf", vf,
                "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
    size, stride = w * h * 3, w * 3
    return [[data[f * size + r * stride:f * size + (r + 1) * stride] for r in range(h)]
            for f in range(len(data) // size)]


def counter_of(line, x0=0):
    value = 0
    for b in range(24):
        x = x0 + b * 48 + 24
        value = (value << 1) | (1 if line[x * 3] > 128 else 0)
    return value


def letterbox(ffmpeg, clip, canvas, content):
    (cw, ch), (x0, y0, w, h) = canvas, content
    regions = {"top": (cw, y0, 0, 0), "bottom": (cw, ch - y0 - h, 0, y0 + h),
               "left": (x0, h, 0, y0), "right": (cw - x0 - w, h, x0 + w, y0)}
    bars, peaks = {}, []
    for name, (rw, rh, rx, ry) in regions.items():
        frames = rows(ffmpeg, clip, rw, rh, rx, ry, every=10)
        row_peaks = [max(max(fr[r]) for fr in frames) for r in range(rh)] if frames else []
        peaks += row_peaks
        bars[name] = {"rows": rh, "peak": max(row_peaks, default=0),
                      "rows_exact_0": sum(1 for p in row_peaks if p == 0),
                      "rows_le_2": sum(1 for p in row_peaks if p <= 2),
                      "worst": sorted(((p, r) for r, p in enumerate(row_peaks)), reverse=True)[:3]}
    # First content row vs the counter row 24 px below: same 24 bits unless something else
    # (a title bar) sits on top of the client area.
    top = rows(ffmpeg, clip, 1152, 25, x0, y0)
    mismatch = sum(1 for fr in top if counter_of(fr[0]) != counter_of(fr[24]))
    # First content column at the swatch centres.
    col = rows(ffmpeg, clip, 1, 600, x0, y0 + 64)
    edge = 0
    for i, (_, want) in enumerate(SWATCHES):
        px = [fr[100 * i + 50] for fr in col]
        med = [int(statistics.median(p[c] for p in px)) for c in range(3)] if px else [999] * 3
        edge = max(edge, max(abs(a - b) for a, b in zip(med, want)))
    return {"canvas": f"{cw}x{ch}", "content": [x0, y0, w, h], "bars": bars,
            "bar_peak_level": max(peaks, default=0), "bar_rows": len(peaks),
            "bar_rows_exact_0": sum(1 for p in peaks if p == 0),
            "first_row_counter_mismatch_frames": mismatch, "first_row_frames": len(top),
            "first_column_swatch_max_diff": edge}


def main():
    args = sys.argv[1:]
    clip = args[0]
    tools = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "tools", "ffmpeg-dev",
                         "ffmpeg-7.1.1-gyan-full-shared", "bin")
    if "--ffmpeg" in args:
        tools = args[args.index("--ffmpeg") + 1]
    ffmpeg, ffprobe = os.path.join(tools, "ffmpeg.exe"), os.path.join(tools, "ffprobe.exe")
    probe = json.loads(run([ffprobe, "-v", "error", "-show_streams", "-show_format", "-of", "json", clip]))
    streams = [{"type": s["codec_type"], "codec": s["codec_name"],
                **({"size": f'{s["width"]}x{s["height"]}', "pix_fmt": s.get("pix_fmt"),
                    "color_space": s.get("color_space"), "color_range": s.get("color_range")}
                   if s["codec_type"] == "video" else {"channels": s.get("channels")})} for s in probe["streams"]]
    video = next(s for s in probe["streams"] if s["codec_type"] == "video")
    canvas = (video["width"], video["height"])
    x0 = y0 = 0
    content = None
    if "--content" in args:
        i = args.index("--content")
        w, h = int(args[i + 1]), int(args[i + 2])
        # framing.rs `fit` for content that fits: centred, even offsets.
        x0, y0 = ((canvas[0] - w) // 2) & ~1, ((canvas[1] - h) // 2) & ~1
        content = (x0, y0, w, h)

    # Counter row: y = 24, x = 0..1151 (24 blocks of 48 px), sampled at each block centre.
    counters = [counter_of(fr[0]) for fr in rows(ffmpeg, clip, 1152, 1, x0, y0 + 24)]
    steps = [b - a for a, b in zip(counters, counters[1:])]
    repeats = sum(1 for s in steps if s == 0)
    backwards = sum(1 for s in steps if s < 0)

    # Swatch column: x = 50, y = 64 + 100 * i + 50 (centres), every frame.
    col = rows(ffmpeg, clip, 1, 600, x0 + 50, y0 + 64)
    colours = []
    for i, (name, want) in enumerate(SWATCHES):
        px = [fr[100 * i + 50] for fr in col]
        med = tuple(int(statistics.median(p[c] for p in px)) for c in range(3)) if px else None
        colours.append({"swatch": name, "drawn": want, "decoded_median": med,
                        "max_abs_diff": max(abs(a - b) for a, b in zip(med, want)) if med else None})

    out = {
        "clip": clip,
        "duration_s": float(probe["format"].get("duration", 0)),
        "streams": streams,
        "frames": len(counters),
        "counter_first": counters[0] if counters else None,
        "counter_last": counters[-1] if counters else None,
        "repeats": repeats,
        "backwards": backwards,
        "step_median": statistics.median(steps) if steps else None,
        "step_max": max(steps) if steps else None,
        "colours": colours,
    }
    if content:
        out["letterbox"] = letterbox(ffmpeg, clip, canvas, content)
    print(json.dumps(out, indent=1))


if __name__ == "__main__":
    main()
