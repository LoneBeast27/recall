"""Check a clip recorded from examples/present_target.rs (scripts/dev/engine-cost.ps1 -SaveSeconds N).

Reads, per decoded frame, the target's 24-bit frame counter (top strip) and the six colour
swatches (left column), and prints JSON: streams, frame count, counter continuity (no
repeats, never backwards, the step between captured frames), and the median decoded colour
of each swatch next to the colour the target drew.

    python -I scripts/dev/check-clip.py <clip.mp4> [--ffmpeg <dir with ffmpeg.exe/ffprobe.exe>]
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

    # Counter row: y = 24, x = 0..1151 (24 blocks of 48 px), sampled at each block centre.
    row = run([ffmpeg, "-v", "error", "-i", clip, "-map", "0:v:0", "-vf", "format=rgb24,crop=1152:1:0:24",
               "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
    width = 1152 * 3
    counters = []
    for f in range(len(row) // width):
        line = row[f * width:(f + 1) * width]
        value = 0
        for b in range(24):
            x = b * 48 + 24
            value = (value << 1) | (1 if line[x * 3] > 128 else 0)
        counters.append(value)
    steps = [b - a for a, b in zip(counters, counters[1:])]
    repeats = sum(1 for s in steps if s == 0)
    backwards = sum(1 for s in steps if s < 0)

    # Swatch column: x = 50, y = 64 + 100 * i + 50 (centres), every frame.
    col = run([ffmpeg, "-v", "error", "-i", clip, "-map", "0:v:0", "-vf", "format=rgb24,crop=1:600:50:64",
               "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
    fsize = 600 * 3
    colours = []
    for i, (name, want) in enumerate(SWATCHES):
        y = 100 * i + 50
        px = [col[f * fsize + y * 3:f * fsize + y * 3 + 3] for f in range(len(col) // fsize)]
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
    print(json.dumps(out, indent=1))


if __name__ == "__main__":
    main()
