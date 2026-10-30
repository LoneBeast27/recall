"""Install a hud-scan.py result's HUD grids into recall-overlay/assets/hud-spots.json.

Only the `hud` entries change: each game's `anchor` and `free_px` (the
island's spot and scale, from always-on HUD) stay exactly as they are. Prints
per game whether the grid changed.

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
        hud = new["spots"].get(key, {}).get("hud")
        changed = hud is not None and hud != spots[key].get("hud")
        print(f"{key:<30} {spots[key]['anchor']:<11} {spots[key]['free_px']:>4}  hud {'updated' if changed else 'kept' if hud else 'none'}")
        if hud is not None:
            spots[key]["hud"] = hud
    source = table["_source"].split("; hud grid:")[0] + "; " + new["_source"]
    # One game per two lines: its spot, then its HUD grid.
    lines = ["{", f' "_source": {json.dumps(source, ensure_ascii=False)},', ' "spots": {']
    keys = list(spots)
    for i, key in enumerate(keys):
        spot = spots[key]
        comma = "," if i < len(keys) - 1 else ""
        head = f'"anchor": "{spot["anchor"]}", "free_px": {spot["free_px"]}'
        if "hud" in spot:
            lines.append(f'  "{key}": {{{head},')
            lines.append(f'   "hud": [{", ".join(str(v) for v in spot["hud"])}]}}{comma}')
        else:
            lines.append(f'  "{key}": {{{head}}}{comma}')
    lines += [" }", "}", ""]
    asset.write_text("\n".join(lines), encoding="utf-8")
    json.loads(asset.read_text(encoding="utf-8"))  # still valid JSON


if __name__ == "__main__":
    main()
