#!/usr/bin/env python3
"""Make the updater's timing file (recall-native/src/services/update_timings.json).

The app's update progress bar is driven by real numbers where it can measure them (bytes
downloaded, requests finished) and uses this file only for the parts it cannot measure while
they run (waiting for GitHub to answer, checking the download, opening Setup). Setup's own
update time is shown as an estimate in the update card.

Sources, each optional, each kept to durations only (no paths, names, hosts or ids):
  --github          release build runs (tag pushes of release.yml) and Setup sizes, via `gh`.
  --setup-log FILE  Setup's log (%TEMP%\\RecallSetup.log): real updates, start to "Recall reopened".
                    Sandbox runs (test installs) are skipped.
  --app-log FILE    the app's own update timings (%LOCALAPPDATA%\\Recall\\updates\\timings.jsonl,
                    written by 0.1.7 and later): check requests, connect, download rate, verify, open.
  --measure N       time N real check requests and N Setup downloads from GitHub from this PC.

Run from the repo root:  python scripts/update-timings.py --github --setup-log <log> --measure 3
Python 3.9+ standard library only (plus the GitHub CLI for --github).
"""
import argparse
import datetime as dt
import hashlib
import json
import math
import re
import socket
import statistics
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

REPO = "LoneBeast27/recall"
OUT = Path(__file__).resolve().parent.parent / "recall-native" / "src" / "services" / "update_timings.json"
# Used only when a phase has no sample at all; marked "default" in the file.
DEFAULTS = {"check_request_s": 0.8, "connect_s": 1.0, "download_mbps": 20.0, "verify_s": 0.3, "open_setup_s": 0.6, "setup_update_s": 8.0}


def stats(values, unit):
    v = sorted(float(x) for x in values)
    if not v:
        return None

    def pct(p):
        # Linear interpolation between closest ranks.
        k = (len(v) - 1) * p / 100
        lo, hi = math.floor(k), math.ceil(k)
        return v[lo] + (v[hi] - v[lo]) * (k - lo)

    r = lambda x: round(x, 3)
    return {"unit": unit, "n": len(v), "mean": r(statistics.fmean(v)), "median": r(pct(50)), "p10": r(pct(10)), "p90": r(pct(90)),
            "p95": r(pct(95)), "stdev": r(statistics.stdev(v)) if len(v) > 1 else 0.0, "min": r(v[0]), "max": r(v[-1])}


def gh(path):
    out = subprocess.run(["gh", "api", "--paginate", path], check=True, capture_output=True, text=True).stdout
    # --paginate joins pages: one JSON value per page.
    dec, pos, pages = json.JSONDecoder(), 0, []
    while pos < len(out):
        while pos < len(out) and out[pos].isspace():
            pos += 1
        if pos >= len(out):
            break
        value, pos = dec.raw_decode(out, pos)
        pages.append(value)
    return pages


def when(text):
    return dt.datetime.fromisoformat(text.replace("Z", "+00:00"))


def github():
    builds, sizes = [], []
    for page in gh(f"repos/{REPO}/actions/workflows/release.yml/runs?event=push&status=success&per_page=100"):
        for run in page["workflow_runs"]:
            if re.fullmatch(r"v\d+\.\d+\.\d+", run["head_branch"] or ""):
                builds.append((when(run["updated_at"]) - when(run["run_started_at"])).total_seconds())
    for page in gh(f"repos/{REPO}/releases?per_page=100"):
        for rel in page:
            if rel["draft"] or rel["prerelease"]:
                continue
            sizes += [a["size"] / 1e6 for a in rel["assets"] if a["name"] == "RecallSetup.exe"]
    return {"release_build_s": stats(builds, "s"), "setup_size_mb": stats(sizes, "MB")}


LINE = re.compile(r"^(\d{4}-\d\d-\d\d \d\d:\d\d:\d\d) \[(\d+)\] (.*)$")


def setup_log(paths):
    """Per real update run (one Setup pid): start, settings backed up, files swapped, reopened."""
    runs = {}
    for path in paths:
        for raw in Path(path).read_text(encoding="utf-8", errors="replace").splitlines():
            m = LINE.match(raw)
            if not m:
                continue
            t, pid, text = dt.datetime.strptime(m[1], "%Y-%m-%d %H:%M:%S"), m[2], m[3]
            if re.match(r"Setup \S+ update ", text):
                runs[(path, pid, t)] = run = {"start": t, "sandbox": "(sandbox" in text}
                runs[(path, pid)] = run
                continue
            run = runs.get((path, pid))
            if run is None:
                continue
            for key, prefix in (("backup", "Settings backed up"), ("swapped", "Program files swapped in"), ("reopened", "Recall reopened")):
                if text.startswith(prefix) and key not in run:
                    run[key] = t
    real = [r for k, r in runs.items() if len(k) == 3 and not r["sandbox"] and "reopened" in r]
    span = lambda r, a, b: (r[b] - r[a]).total_seconds()
    return {
        "setup_update_s": stats([span(r, "start", "reopened") for r in real], "s"),
        "setup_quit_recorder_s": stats([span(r, "start", "backup") for r in real if "backup" in r], "s"),
        "setup_swap_s": stats([span(r, "backup", "swapped") for r in real if "backup" in r and "swapped" in r], "s"),
        "setup_reopen_s": stats([span(r, "swapped", "reopened") for r in real if "swapped" in r], "s"),
    }


APP_FIELDS = ("check_request_s", "connect_s", "download_mbps", "verify_s", "open_setup_s")


def app_log(paths):
    got = {k: [] for k in APP_FIELDS}
    for path in paths:
        for line in Path(path).read_text(encoding="utf-8", errors="replace").splitlines():
            try:
                row = json.loads(line)
            except ValueError:
                continue
            for k in APP_FIELDS:
                values = row.get(k) if isinstance(row.get(k), list) else [row.get(k)]
                got[k] += [v for v in values if isinstance(v, (int, float)) and v >= 0]
    return got


def measure(n):
    """Real requests from this PC: the check's API and checksum requests, then Setup downloads."""
    ua = {"User-Agent": "Recall-updater-timings"}
    latest = json.load(urllib.request.urlopen(urllib.request.Request(f"https://api.github.com/repos/{REPO}/releases/latest", headers=ua), timeout=30))
    tag = latest["tag_name"]
    urls = [f"https://api.github.com/repos/{REPO}/releases/latest", f"https://api.github.com/repos/{REPO}/compare/{tag}...main?per_page=1&page=2",
            f"https://github.com/{REPO}/releases/download/{tag}/SHA256SUMS.txt"]
    got = {k: [] for k in APP_FIELDS}
    # Connect time per address of each host: a host with one dead address of 4 shows up as
    # 1 slow sample in 4, the chance a check meets it first (Windows waits ~21 s on it).
    for _ in range(n):
        for host in ("api.github.com", "github.com", "release-assets.githubusercontent.com"):
            for ip in sorted({a[4][0] for a in socket.getaddrinfo(host, 443, socket.AF_INET, socket.SOCK_STREAM)}):
                t = time.perf_counter()
                try:
                    socket.create_connection((ip, 443), timeout=25).close()
                except OSError:
                    pass
                got["connect_s"].append(time.perf_counter() - t)
    for _ in range(n):
        for url in urls:
            t = time.perf_counter()
            urllib.request.urlopen(urllib.request.Request(url, headers=ua), timeout=30).read()
            got["check_request_s"].append(time.perf_counter() - t)
        t = time.perf_counter()
        reply = urllib.request.urlopen(urllib.request.Request(f"https://github.com/{REPO}/releases/download/{tag}/RecallSetup.exe", headers=ua), timeout=60)
        first = reply.read(1)  # the rate counts from the first byte
        t_first = time.perf_counter()
        body = first + reply.read()
        t_end = time.perf_counter()
        got["download_mbps"].append(len(body) * 8 / 1e6 / max(t_end - t_first, 1e-6))
        # Python's SHA-256 stands in for the app's; same file, same order of cost.
        t = time.perf_counter()
        hashlib.sha256(body).hexdigest()
        got["verify_s"].append(time.perf_counter() - t)
    return got


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--github", action="store_true")
    ap.add_argument("--setup-log", action="append", default=[])
    ap.add_argument("--app-log", action="append", default=[])
    ap.add_argument("--measure", type=int, default=0)
    ap.add_argument("--out", type=Path, default=OUT)
    a = ap.parse_args()

    phases, sources = {}, []
    if a.github:
        phases.update(github())
        sources.append("github release runs and assets")
    if a.setup_log:
        phases.update(setup_log(a.setup_log))
        sources.append(f"{len(a.setup_log)} Setup log(s)")
    samples = {k: [] for k in APP_FIELDS}
    if a.app_log:
        for k, v in app_log(a.app_log).items():
            samples[k] += v
        sources.append(f"{len(a.app_log)} app timing log(s)")
    if a.measure:
        for k, v in measure(a.measure).items():
            samples[k] += v
        sources.append(f"{a.measure} measured round(s) from one PC")
    for k, v in samples.items():
        phases[k] = stats(v, "Mbit/s" if k == "download_mbps" else "s")
    for k, value in DEFAULTS.items():
        if not phases.get(k):
            phases[k] = {"unit": "Mbit/s" if k == "download_mbps" else "s", "n": 0, "default": value, "median": value, "p90": value * 2}
    phases = {k: v for k, v in sorted(phases.items()) if v}
    doc = {
        "about": "Update timings for the progress bar: only unmeasurable waits use these. Made by scripts/update-timings.py; do not edit by hand.",
        "generated": dt.date.today().isoformat(),
        "sources": sources,
        "phases": phases,
    }
    a.out.write_text(json.dumps(doc, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(f"wrote {a.out}")
    for k, v in phases.items():
        print(f"  {k:24} n={v['n']:<3} median={v['median']} p90={v.get('p90')} {v['unit']}")


if __name__ == "__main__":
    sys.exit(main())
