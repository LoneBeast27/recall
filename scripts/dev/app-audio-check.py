"""Reads an app-audio check clip: track names in order, and how much of each test tone
(Tone A 440 Hz, Tone B 660 Hz) is in each track, per second. Exit 1 on a failed expectation.

    python scripts/dev/app-audio-check.py <clip.mp4>

Needs ffprobe/ffmpeg on PATH and numpy.
"""
import json
import subprocess
import sys

import numpy as np

RATE = 48_000


def tracks(clip):
    out = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "a", "-show_entries", "stream=index:stream_tags=handler_name,title",
                          "-of", "json", clip], capture_output=True, text=True, check=True).stdout
    return [(s["index"], s.get("tags", {}).get("title") or s.get("tags", {}).get("handler_name", "")) for s in json.loads(out)["streams"]]


def pcm(clip, ordinal):
    raw = subprocess.run(["ffmpeg", "-v", "error", "-i", clip, "-map", f"0:a:{ordinal}", "-ac", "1", "-ar", str(RATE), "-f", "f32le", "-"],
                         capture_output=True, check=True).stdout
    return np.frombuffer(raw, dtype=np.float32)


def strength(block, hz):
    t = np.arange(len(block)) / RATE
    return float(np.abs(np.dot(block, np.exp(-2j * np.pi * hz * t))) * 2 / max(len(block), 1))


def per_second(samples, hz):
    return [strength(samples[i:i + RATE], hz) for i in range(0, len(samples) - RATE + 1, RATE)]


def main(clip):
    found = tracks(clip)
    names = [name for _, name in found]
    print("tracks:", names)
    failures = []
    apps = names[-3:]
    if apps != ["Tone A", "Tone B", "Tone C"]:
        failures.append(f"app tracks must be the last three, in order: {names}")
    report = {}
    for ordinal, name in enumerate(names):
        samples = pcm(clip, ordinal)
        a, b = per_second(samples, 440), per_second(samples, 660)
        report[name] = (a, b)
        print(f"{name:>12}: {len(samples) / RATE:5.1f} s | 440 Hz on {sum(x > 0.02 for x in a):2d} s | 660 Hz on {sum(x > 0.02 for x in b):2d} s"
              f" | peak {float(np.abs(samples).max()) if len(samples) else 0:.3f}")
    if "Tone A" in report:
        a, b = report["Tone A"]
        if sum(x > 0.02 for x in a) < len(a) - 2:
            failures.append("Tone A must hold 440 Hz all through")
        if any(x > 0.02 for x in b):
            failures.append("Tone A must not hold Tone B's 660 Hz")
    if "Tone B" in report:
        a, b = report["Tone B"]
        on = [x > 0.02 for x in b]
        if not any(on) or all(on):
            failures.append("Tone B started late: its track must be silent first, then 660 Hz")
        elif sum(on[on.index(True):]) < len(on[on.index(True):]) - 2:
            failures.append("Tone B must hold 660 Hz from its start to the end of the clip")
        if any(x > 0.02 for x in a):
            failures.append("Tone B must not hold Tone A's 440 Hz")
    if "Tone C" in report:
        samples = pcm(clip, names.index("Tone C"))
        if len(samples) == 0 or float(np.abs(samples).max()) > 1e-4:
            failures.append("Tone C never ran: its track must be present and silent")
        lengths = {name: len(pcm(clip, i)) / RATE for i, name in enumerate(names)}
        if max(lengths.values()) - lengths["Tone C"] > 0.5:
            failures.append(f"Tone C must run the clip's whole length: {lengths}")
    for failure in failures:
        print("FAIL:", failure)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
