"""Install a hud-scan.py result's skylines into recall-overlay/assets/hud-spots.json.

Only the `skyline` entries change: each game's `anchor` and `free_px` (the
island's spot and scale, from always-on HUD) stay exactly as they are. Prints
per game whether the skyline changed.

    python -I scripts/dev/hud-spots-update.py SCAN_OUT/hud-spots.json recall-overlay/assets/hud-spots.json
"""
import json
import sys
from pathlib import Path


def main():
    new_path, asset = Path(sys.argv[1]), Path(sys.argv[2])
    new = json.loads(new_path.read_text(encoding="utf-8"))
    table = json.loads(asset.read_text(encoding="utf-8"))
    spots = table["spots"]
    for key in sorted(spots):
        skyline = new["spots"].get(key, {}).get("skyline")
        changed = skyline is not None and skyline != spots[key].get("skyline")
        print(f"{key:<30} {spots[key]['anchor']:<11} {spots[key]['free_px']:>4}  skyline {'updated' if changed else 'kept' if skyline else 'none'}")
        if skyline is not None:
            spots[key]["skyline"] = skyline
    source = table["_source"].split("; skyline:")[0] + "; " + new["_source"]
    # One game per two lines: its spot, then its skyline.
    lines = ["{", f' "_source": {json.dumps(source, ensure_ascii=False)},', ' "spots": {']
    keys = list(spots)
    for i, key in enumerate(keys):
        spot = spots[key]
        comma = "," if i < len(keys) - 1 else ""
        head = f'"anchor": "{spot["anchor"]}", "free_px": {spot["free_px"]}'
        if "skyline" in spot:
            lines.append(f'  "{key}": {{{head},')
            lines.append(f'   "skyline": [{", ".join(str(v) for v in spot["skyline"])}]}}{comma}')
        else:
            lines.append(f'  "{key}": {{{head}}}{comma}')
    lines += [" }", "}", ""]
    asset.write_text("\n".join(lines), encoding="utf-8")
    json.loads(asset.read_text(encoding="utf-8"))  # still valid JSON


if __name__ == "__main__":
    main()
