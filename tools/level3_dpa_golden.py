#!/usr/bin/env python3
"""Golden values for the HRAP geometry of Level III DPA products (product 81).

Usage (from the workspace root)::

    python tools/level3_dpa_golden.py [--placement DIR] [--rate-placement DIR...] [--check]

Writes ``testdata/level3/golden-dpa.json``. Requires numpy, pyproj and MetPy
1.7.1 (the reference venv).

Three parts:

``files``
    For every committed DPA file of ``testdata/level3/manifest.toml``: the
    radar's HRAP grid coordinates computed with pyproj (polar stereographic
    on a 6371.2 km sphere, true at 60N, central meridian 105W; HRAP mesh
    4.7625 km, pole at (401, 1601)), the array's west and north edges under
    the placement rule below, and the latitude / longitude (pyproj inverse)
    of the centres of boxes (0, 0), (0, 130), (65, 65), (130, 0), (130, 130)
    of the 131 x 131 array and (0, 0), (6, 6), (12, 12) of the 13 x 13 rate
    arrays (``rate_west``, ``rate_north``: see ``rate_placement``).

``placement``
    The evidence for the placement rule: for every DPA file in ``DIR``
    (``--placement``; real products from the AWS bucket
    ``unidata-nexrad-level3``, not committed), the packet 17 array is read
    with MetPy and its "outside coverage" level (255) compared with the boxes
    whose centre lies beyond 230 km of the radar (great circle, same sphere),
    for every west / north offset from ``floor(hx) - 66`` to
    ``floor(hx) - 64`` and ``floor(hy) + 65`` to ``floor(hy) + 67`` in
    quarter boxes. Recorded per file: SHA-256, site, radar HRAP position, the
    best offsets and their mismatch count, and the mismatch count of the
    runner-up, and the mismatch count of the rule (west = floor(hx) - 65,
    north = floor(hy) + 66, rows from the north). For the 47 sites of the
    2026 run the rule is the best offset at 46 and 2 boxes from the best
    (a quarter-box shift north) at KPDT.

``rate_placement``
    The evidence for the rate array rule (``--rate-placement DIR...``, the
    committed DPA files plus every file in the directories that has a
    packet 18 array): the 13 x 13 array is 1/4 LFM boxes (10 HRAP units)
    of the national grid, whose boxes have a corner at the pole (HRAP
    (401, 1601)), with the radar's box at row 6, column 6
    (``crates/recast-radar-io-level3/src/hrap.rs``). The array's "ND" level
    (7) is compared with the boxes wholly beyond 230 km (every one of 21
    points per box edge beyond it). Recorded per file: SHA-256, radar HRAP
    position, ND count, mismatches under the rule and under the corner rule
    of the earlier decoder (the 131 x 131 array's corner, ``floor(hx) - 65``,
    ``floor(hy) + 66``), and the best mismatch count with its shifts from the
    rule over -3 to 3 HRAP units in half units.
"""

import argparse
import glob
import hashlib
import json
import math
import sys
import tomllib
from datetime import datetime, timedelta
from pathlib import Path

import numpy as np
from pyproj import CRS, Transformer

import metpy.io.nexrad as nx
from metpy.io import Level3File

# Windows: nexrad_to_datetime uses datetime.fromtimestamp, which raises on the
# zero dates of 1990s products; compute the same value directly.
nx.nexrad_to_datetime = lambda d, ms: datetime(1970, 1, 1) + timedelta(days=d - 1, milliseconds=ms)

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "testdata" / "level3" / "golden-dpa.json"

R_EARTH_KM = 6371.2
MESH_KM = 4.7625
HRAP = CRS.from_proj4(
    "+proj=stere +lat_0=90 +lat_ts=60 +lon_0=-105 +a=6371200 +b=6371200 +units=m +no_defs"
)
SPHERE = CRS.from_proj4("+proj=longlat +a=6371200 +b=6371200 +no_defs")
FWD = Transformer.from_crs(SPHERE, HRAP, always_xy=True)
INV = Transformer.from_crs(HRAP, SPHERE, always_xy=True)


def hrap_xy(lat, lon):
    x, y = FWD.transform(lon, lat)
    return x / (MESH_KM * 1000) + 401.0, y / (MESH_KM * 1000) + 1601.0


def lonlat(hx, hy):
    return INV.transform((hx - 401.0) * MESH_KM * 1000, (hy - 1601.0) * MESH_KM * 1000)


def great_circle_km(lat1, lon1, lat2, lon2):
    p1, p2 = np.radians(lat1), np.radians(lat2)
    c = np.sin(p1) * np.sin(p2) + np.cos(p1) * np.cos(p2) * np.cos(np.radians(lon2 - lon1))
    return R_EARTH_KM * np.arccos(np.clip(c, -1, 1))


def dpa_grid(path):
    f = Level3File(str(path))
    for layer in f.sym_block:
        for packet in layer:
            data = packet.get("data") if isinstance(packet, dict) else None
            if data is not None and np.shape(data) == (131, 131):
                return f, np.array(data, dtype=int)
    return f, None


def files_part():
    manifest = tomllib.loads((ROOT / "testdata" / "level3" / "manifest.toml").read_text())
    out = []
    for entry in manifest["file"]:
        if "product:81" not in entry.get("tags", []):
            continue
        path = ROOT / "testdata" / entry["committed"]
        f = Level3File(str(path))
        lat, lon = f.lat, f.lon
        hx, hy = hrap_xy(lat, lon)
        west, north = math.floor(hx) - 65.0, math.floor(hy) + 66.0
        rate_west, rate_north = rate_corner(hx, hy)
        boxes = []
        for size, (w, n), cells in ((1.0, (west, north), [(0, 0), (0, 130), (65, 65), (130, 0), (130, 130)]),
                                    (10.0, (rate_west, rate_north), [(0, 0), (6, 6), (12, 12)])):
            for row, col in cells:
                bx = w + (col + 0.5) * size
                by = n - (row + 0.5) * size
                blon, blat = lonlat(bx, by)
                boxes.append({"box_size": size, "row": row, "column": col,
                              "latitude": blat, "longitude": blon})
        out.append({"id": entry["id"], "latitude": lat, "longitude": lon,
                    "hrap_x": hx, "hrap_y": hy, "west": west, "north": north,
                    "rate_west": rate_west, "rate_north": rate_north, "boxes": boxes})
    return out


def rate_corner(hx, hy):
    """North-west corner of the 13 x 13 rate array: 1/4 LFM boxes (10 HRAP
    units) with a corner at the pole, the radar's box at row 6, column 6."""
    radar_west = 401.0 + 10.0 * math.floor((hx - 401.0) / 10.0)
    radar_north = 1601.0 + 10.0 * (math.floor((hy - 1601.0) / 10.0) + 1)
    return radar_west - 60.0, radar_north + 60.0


def rate_array(path):
    """MetPy reading and the first 13 x 13 rate array (packet 18) of a DPA file."""
    f = Level3File(str(path))
    for layer in getattr(f, "sym_block", []):
        for packet in layer:
            data = packet.get("data") if isinstance(packet, dict) else None
            if data is not None and np.shape(data) == (13, 13):
                return f, np.array(data, dtype=int)
    return f, None


def wholly_beyond(lat, lon, west, north, km=230.0):
    """Rate boxes whose every perimeter point (21 per edge) is beyond `km`."""
    rows, cols = np.mgrid[0:13, 0:13]
    t = np.linspace(0.0, 1.0, 21)
    edges = [(u, 0.0) for u in t] + [(u, 1.0) for u in t] + [(0.0, v) for v in t] + [(1.0, v) for v in t]
    nearest = np.full((13, 13), np.inf)
    for u, v in edges:
        blon, blat = lonlat(west + (cols + u) * 10.0, north - (rows + v) * 10.0)
        nearest = np.minimum(nearest, great_circle_km(lat, lon, blat, blon))
    return nearest > km


def rate_placement_part(paths):
    out = []
    for path in paths:
        f, grid = rate_array(path)
        if grid is None:
            continue
        nd = grid == 7
        lat, lon = f.lat, f.lon
        hx, hy = hrap_xy(lat, lon)
        west, north = rate_corner(hx, hy)
        rule = int((wholly_beyond(lat, lon, west, north) != nd).sum())
        corner = int((wholly_beyond(lat, lon, math.floor(hx) - 65.0, math.floor(hy) + 66.0) != nd).sum())
        scores = []
        for dx in np.arange(-3.0, 3.01, 0.5):
            for dy in np.arange(-3.0, 3.01, 0.5):
                m = int((wholly_beyond(lat, lon, west + dx, north + dy) != nd).sum())
                scores.append((m, float(dx), float(dy)))
        best = min(m for m, _, _ in scores)
        data = Path(path).read_bytes()
        out.append({
            "file": Path(path).name,
            "sha256": hashlib.sha256(data).hexdigest(),
            "hrap_x": hx, "hrap_y": hy,
            "nd_boxes": int(nd.sum()),
            "rule_mismatches": rule,
            "corner_rule_mismatches": corner,
            "best_mismatches": best,
            "best_shifts": sorted([dx, dy] for m, dx, dy in scores if m == best),
        })
    return out


def placement_part(directory):
    out = []
    for path in sorted(glob.glob(str(Path(directory) / "*"))):
        f, grid = dpa_grid(path)
        if grid is None:
            continue
        outside = grid == 255
        lat, lon = f.lat, f.lon
        hx, hy = hrap_xy(lat, lon)
        fx, fy = math.floor(hx), math.floor(hy)
        rows, cols = np.mgrid[0:131, 0:131]
        scores = []
        for dx in np.arange(-66.0, -63.99, 0.25):
            for dy in np.arange(65.0, 67.01, 0.25):
                blon, blat = lonlat(fx + dx + cols + 0.5, fy + dy - rows - 0.5)
                beyond = great_circle_km(lat, lon, blat, blon) > 230.0
                scores.append((int((beyond != outside).sum()), float(dx), float(dy)))
        rule = next(m for m, dx, dy in scores if dx == -65.0 and dy == 66.0)
        scores.sort()
        data = Path(path).read_bytes()
        out.append({
            "file": Path(path).name,
            "sha256": hashlib.sha256(data).hexdigest(),
            "hrap_x": hx, "hrap_y": hy,
            "best_west_offset": scores[0][1], "best_north_offset": scores[0][2],
            "best_mismatches": scores[0][0], "runner_up_mismatches": scores[1][0],
            "rule_mismatches": rule,
            "outside_boxes": int(outside.sum()),
        })
    return out


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--placement", help="directory of DPA products for the placement check")
    parser.add_argument("--rate-placement", nargs="+", metavar="DIR",
                        help="directories of DPA products for the rate array placement check "
                             "(the committed DPA files are always included)")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    golden = {"files": files_part()}
    previous = json.loads(OUT.read_text()) if OUT.exists() else {}
    if args.placement:
        golden["placement"] = placement_part(args.placement)
    else:
        golden["placement"] = previous.get("placement", [])
    if args.rate_placement:
        manifest = tomllib.loads((ROOT / "testdata" / "level3" / "manifest.toml").read_text())
        paths = [ROOT / "testdata" / e["committed"] for e in manifest["file"]
                 if "product:81" in e.get("tags", [])]
        for directory in args.rate_placement:
            paths.extend(sorted(Path(p) for p in glob.glob(str(Path(directory) / "*"))))
        golden["rate_placement"] = rate_placement_part(paths)
    else:
        golden["rate_placement"] = previous.get("rate_placement", [])
    text = json.dumps(golden, indent=1, sort_keys=True) + "\n"
    if args.check:
        if OUT.read_text() != text:
            sys.exit(f"{OUT} differs")
        return
    with open(OUT, "w", newline="\n") as out:
        out.write(text)
    for p in golden.get("placement", []):
        print(p["file"], p["best_west_offset"], p["best_north_offset"],
              p["best_mismatches"], p["runner_up_mismatches"], p["rule_mismatches"])
    rate = golden.get("rate_placement", [])
    for p in rate:
        print(p["file"], p["nd_boxes"], p["rule_mismatches"], p["corner_rule_mismatches"],
              p["best_mismatches"], p["best_shifts"][:4])
    print(len(rate), "rate arrays:", sum(p["rule_mismatches"] for p in rate), "mismatches under the rule,",
          sum(p["corner_rule_mismatches"] for p in rate), "under the corner rule")


if __name__ == "__main__":
    main()
