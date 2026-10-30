"""Install a hud-scan.py result as recall-overlay/assets/hud-spots.json and print
the change per game (old free depth -> new, anchor).

    python -I scripts/dev/hud-spots-update.py SCAN_OUT/hud-spots.json recall-overlay/assets/hud-spots.json
"""
import json
import sys
from pathlib import Path


def main():
    new_path, asset = Path(sys.argv[1]), Path(sys.argv[2])
    new = json.loads(new_path.read_text(encoding="utf-8"))
    old = json.loads(asset.read_text(encoding="utf-8"))["spots"]
    for key in sorted(set(old) | set(new["spots"])):
        before, after = old.get(key), new["spots"].get(key)
        show = lambda s: f"{s['anchor']:<10} {s['free_px']:>4}" if s else "-"
        print(f"{key:<30} {show(before):>15}  ->  {show(after)}")
    # One spot per line block, each skyline on one line.
    lines = ["{", f' "_source": {json.dumps(new["_source"], ensure_ascii=False)},', ' "spots": {']
    keys = sorted(new["spots"])
    for i, key in enumerate(keys):
        s = new["spots"][key]
        sky = ", ".join(str(v) for v in s["skyline"])
        comma = "," if i < len(keys) - 1 else ""
        lines.append(f'  "{key}": {{"anchor": "{s["anchor"]}", "free_px": {s["free_px"]},')
        lines.append(f'   "skyline": [{sky}]}}{comma}')
    lines += [" }", "}", ""]
    asset.write_text("\n".join(lines), encoding="utf-8")
    json.loads(asset.read_text(encoding="utf-8"))  # still valid JSON


if __name__ == "__main__":
    main()
