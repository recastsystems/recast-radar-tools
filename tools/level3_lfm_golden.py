#!/usr/bin/env python3
"""Golden values for the national radar grid of the Radar Coded Message.

Usage (from the workspace root)::

    python tools/level3_lfm_golden.py [--cache DIR] [--check]

Writes ``testdata/level3/golden-lfm.json``. Requires numpy, pyproj and MetPy
1.7.1 (the reference venv). ``--cache`` is where the AWS inputs of the
evidence cases are downloaded (default ``$RECAST_RADAR_L3_CACHE`` or
``~/radar-corpus/level3-rcm``); each is checked against its SHA-256.

The Radar Coded Message (product 74, ICD 2620001P Appendix B) locates its
data in the boxes of a local 25 x 25 grid of 1/4 LFM boxes (rows and columns
``A``-``Y`` from the north-west), each split into 4 x 4 1/16 LFM boxes
lettered ``A``-``P``. The ICD does not say where the national grid's box
edges lie. The rule tested here (``crates/recast-radar-io-level3/src/hrap.rs``):
the 1/4 and 1/16 LFM boxes have a corner at the North Pole of the HRAP grid
(HRAP (401, 1601); a 1/4 LFM box is 10 HRAP units, a 1/16 LFM box 2.5), the
box holding the radar is local box ``MM`` (row 12, column 12), and the 1/16
LFM boxes are lettered down the columns (``A``-``D`` the western column from
north to south). The projection is the HRAP polar stereographic (sphere of
6371.2 km, true at 60N, 105W), computed with pyproj.

Three parts:

``files``
    For every product-74 and product-83 (IRM) file of
    ``testdata/level3/manifest.toml``: the
    radar's HRAP position, the fine grid's west and north edges under the
    rule, and the latitude and longitude of the centres of fine boxes (0, 0),
    (0, 99), (49, 49), (99, 0) and (99, 99).

``cases``
    The evidence, from six volumes at radars from 71W to 107W: the Radar
    Coded Message and the Storm Tracking Information (58), Tornado Vortex
    Signature (61) and Digital Hybrid Scan Reflectivity (32) products of the
    same volume (committed corpus files, or AWS ``unidata-nexrad-level3``
    objects pinned by SHA-256). MetPy reads the STI current storm positions
    and the TVS positions (symbology I, J in 1/4 km: MetPy's x and y, east
    and north of the radar), which are placed on the sphere along the great
    circle from the radar and projected to HRAP. Recorded per case: every
    centroid and TVS with the fine box the message names, the fine box the
    rule gives and its distance from the named box (``outside_km``, 0 when
    inside; ground distance at the radar's latitude); and
    ``alignments_within_tolerance``: the offsets of the grid from the rule,
    in quarter HRAP units over one 10 x 10 period (1600 offsets), under
    which every feature lies within ``TOLERANCE_KM`` (0.5 km) of its named
    box. The intensity check places every Digital Hybrid Scan Reflectivity
    bin (radial centre, bin centre, great circle) in its fine box and
    categorises the box maximum with the thresholds 18, 30, 41, 46, 50 and
    57 dBZ; ``agree_rule`` is the number of boxes with data and a message
    level 0-6 whose category equals the message's level under the rule, and
    ``agree_best`` / ``best_offsets`` the best of the offsets within one
    fine box of the rule, in quarter HRAP units.

``levels``
    Levels 7 and 8 (beyond 124 nmi) against the Composite Reflectivity (38,
    2.2 nmi raster centred on the radar) of KTLX 2013-05-20 20:16: the median
    of the box maxima of each level.
"""

import argparse
import hashlib
import json
import math
import os
import re
import struct
import sys
import tomllib
import urllib.request
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
OUT = ROOT / "testdata" / "level3" / "golden-lfm.json"
AWS = "https://unidata-nexrad-level3.s3.amazonaws.com/"

R_EARTH_KM = 6371.2
MESH_KM = 4.7625
POLE = (401.0, 1601.0)
QUARTER = 10.0
FINE = 2.5
HRAP = CRS.from_proj4(
    "+proj=stere +lat_0=90 +lat_ts=60 +lon_0=-105 +a=6371200 +b=6371200 +units=m +no_defs"
)
SPHERE = CRS.from_proj4("+proj=longlat +a=6371200 +b=6371200 +no_defs")
FWD = Transformer.from_crs(SPHERE, HRAP, always_xy=True)
INV = Transformer.from_crs(HRAP, SPHERE, always_xy=True)
THRESHOLDS = np.array([18.0, 30.0, 41.0, 46.0, 50.0, 57.0])
# A feature "fits" its named box when it lies inside it or within this
# distance of it (the STI positions are to 0.25 km, the RCM's own placement
# rounds differently).
TOLERANCE_KM = 0.5

# Evidence cases: product -> committed corpus id or (AWS key, SHA-256).
CASES = [
    {
        "name": "KTLX 2013-05-20 20:16",
        "rcm": "l3-tlx-rcm-20130520-2016",
        "sti": "l3-tlx-nst-20130520-2016",
        "tvs": "l3-tlx-ntv-20130520-2016",
        "dhr": "l3-tlx-dhr-20130520-2016",
    },
    {
        "name": "KTLX 2022-05-03 00:45",
        "rcm": "l3-tlx-rcm-20220503-004553",
        "sti": ("TLX_NST_2022_05_03_00_45_53", "3ddb2d49e653a256091e43a723d7d48d79d891c7e59fa3630793b4c72f82cbee"),
        "tvs": ("TLX_NTV_2022_05_03_00_45_53", "5e6fe62aeea915a3865e50737813e893a1561a65b8d132d6a425889bad6c3df0"),
        "dhr": ("TLX_DHR_2022_05_03_00_45_53", "f67e4becc867d990e57a8c2b15e506220ec9f479989fe00b6fbe5297014d4a28"),
    },
    {
        "name": "KBOX 2022-05-16 20:16",
        "rcm": ("BOX_RCM_2022_05_16_20_16_41", "32c966aa848b11cc396e84ac02191fe997bd58711defaf223a7baa63b3351900"),
        "sti": ("BOX_NST_2022_05_16_20_16_41", "ce06948ae33b779fda00552b02c07f5e89cfe9e38929db5d1595f5162aeb90f6"),
        "tvs": ("BOX_NTV_2022_05_16_20_16_41", "6900ed7bdc968913509780cdf0d8aa97d16cbb5585046b2a2e6a11460856c5e2"),
        "dhr": ("BOX_DHR_2022_05_16_20_16_41", "08c50872082b6ff075a4e25093ec595334da257b2984859910a0b1440604b2bb"),
    },
    {
        "name": "KLWX 2022-05-16 21:46",
        "rcm": ("LWX_RCM_2022_05_16_21_46_49", "979c2f8cdc38ca3a894b224025bd37cee84d2511c067680957ada71605a9f7d7"),
        "sti": ("LWX_NST_2022_05_16_21_46_49", "5c21ebf86632c068ac7f76cf54f7d2818b450c32a6a907676c53d417ab547dc8"),
        "tvs": ("LWX_NTV_2022_05_16_21_46_49", "aca37a929c4047047b2e4bf9c242994ea7101f83efac1ff25b300b706368615a"),
        "dhr": ("LWX_DHR_2022_05_16_21_46_49", "3b8d15cd40add036e599adfa87995b2b8aeeec6895acee085dfb3c318e873bbb"),
    },
    {
        "name": "KMLB 2022-05-16 21:45",
        "rcm": ("MLB_RCM_2022_05_16_21_45_53", "58d78bbf370005054df3979cd0edee56d945deb35cd42b0c2edf80c0b98c20bd"),
        "sti": ("MLB_NST_2022_05_16_21_45_53", "83986eb362ff1044bfbc7cf0f4cef35afd70eee430c2cdb8fd8c944e5a40a5c7"),
        "tvs": ("MLB_NTV_2022_05_16_21_45_53", "f6b8a82a78850df149b7e63a6b0a672c6195d0082ecda3d19e24ed309294f2eb"),
        "dhr": ("MLB_DHR_2022_05_16_21_45_53", "156af1b9fcd8d2fc3d396c312d6baa448e13be02f7313269191fbc4e3e8284f6"),
    },
    {
        "name": "KGGW 2022-05-12 20:13",
        "rcm": ("GGW_RCM_2022_05_12_20_13_17", "50743839b5d80c7f0b778cd44422b89e87b12342919cb6d7f430e1eeebc9bb2f"),
        "sti": ("GGW_NST_2022_05_12_20_13_17", "f475aea73b8d119b1b07fc2402a3408a64fb988092f131aa4f6d82a2c5b8648f"),
        "tvs": ("GGW_NTV_2022_05_12_20_13_17", "1dacc2564848e77edff234d50153a3e8cdfebb52fd07937b2c43b8c2cded60e8"),
        "dhr": ("GGW_DHR_2022_05_12_20_13_17", "a2234aabbdb0a9ffc67db65e56663e577764e268c216c9e2d3c304e413141a9e"),
    },
]


def manifest():
    return {e["id"]: e for e in tomllib.loads((ROOT / "testdata" / "level3" / "manifest.toml").read_text())["file"]}


def source(ref, cache, entries):
    """(path, description, sha256) of a committed id or a pinned AWS object."""
    if isinstance(ref, str):
        entry = entries[ref]
        return ROOT / "testdata" / entry["committed"], {"id": ref, "sha256": entry["sha256"]}
    key, sha = ref
    path = Path(cache) / key
    if not path.exists():
        path.parent.mkdir(parents=True, exist_ok=True)
        with urllib.request.urlopen(AWS + key, timeout=60) as r:
            path.write_bytes(r.read())
    got = hashlib.sha256(path.read_bytes()).hexdigest()
    if got != sha:
        sys.exit(f"{key}: sha256 {got} != {sha}")
    return path, {"url": AWS + key, "sha256": sha}


def hrap(lat, lon):
    x, y = FWD.transform(lon, lat)
    return np.asarray(x) / (MESH_KM * 1000) + POLE[0], np.asarray(y) / (MESH_KM * 1000) + POLE[1]


def lonlat(hx, hy):
    return INV.transform((hx - POLE[0]) * MESH_KM * 1000, (hy - POLE[1]) * MESH_KM * 1000)


def destination(lat0, lon0, azimuth_deg, distance_km):
    """Great-circle destination on the 6371.2 km sphere."""
    p1, l1 = math.radians(lat0), math.radians(lon0)
    az = np.radians(azimuth_deg)
    d = np.asarray(distance_km, dtype=float) / R_EARTH_KM
    p2 = np.arcsin(np.sin(p1) * np.cos(d) + np.cos(p1) * np.sin(d) * np.cos(az))
    l2 = l1 + np.arctan2(np.sin(az) * np.sin(d) * np.cos(p1), np.cos(d) - np.sin(p1) * np.sin(p2))
    return np.degrees(p2), np.degrees(l2)


def east_north_to_hrap(lat0, lon0, east_km, north_km):
    e, n = np.asarray(east_km, dtype=float), np.asarray(north_km, dtype=float)
    return hrap(*destination(lat0, lon0, np.degrees(np.arctan2(e, n)), np.hypot(e, n)))


def fine_grid(radar_hx, radar_hy, dx=0.0, dy=0.0):
    """West and north edges of the 100 x 100 fine grid under the rule, shifted by (dx, dy)."""
    west = POLE[0] + QUARTER * math.floor((radar_hx - POLE[0]) / QUARTER)
    north = POLE[1] + QUARTER * (math.floor((radar_hy - POLE[1]) / QUARTER) + 1)
    return west - 12 * QUARTER + dx, north + 12 * QUARTER + dy


def fine_index(hx, hy, west, north):
    return np.floor((north - hy) / FINE).astype(int), np.floor((hx - west) / FINE).astype(int)


def label(row, col):
    """Three-letter fine box: row, column, sub-box lettered down the columns."""
    if not (0 <= row < 100 and 0 <= col < 100):
        return None
    return chr(65 + row // 4) + chr(65 + col // 4) + chr(65 + (col % 4) * 4 + row % 4)


def rcm_text(path):
    data = path.read_bytes()
    m = re.match(rb"(\x01\r\r\n\d{3,5} ?\r\r\n)?[A-Z]{4}\d\d [A-Z]{4} \d{6}( [A-Z]{3})?\r\r\n([A-Z0-9]{3,6} *\r\r\n)?", data)
    msg = data[m.end():] if m else data
    hw = struct.unpack(">60H", msg[:120])
    offset = 2 * ((hw[54] << 16) | hw[55])
    lat = struct.unpack(">i", msg[20:24])[0] / 1000.0
    lon = struct.unpack(">i", msg[24:28])[0] / 1000.0
    return lat, lon, re.sub(r"\s+", "", msg[offset:].decode("latin-1"))


def box(text):
    r, c, s = (ord(ch) - 65 for ch in text)
    return r * 4 + s % 4, c * 4 + s // 4


def rcm_grid(flat):
    grid = np.zeros((100, 100), dtype=int)
    m = re.search(r"/NI(\d+):([^/]*)", flat)
    for group in [g for g in (m.group(2).split(",") if m else []) if g]:
        row, col = box(group[:3])
        levels = []
        for ch in group[3:]:
            if ch.isdigit():
                levels.append(int(ch))
            else:
                levels.extend([levels[-1]] * (ord(ch) - 64))
        for k, level in enumerate(levels):
            if col + k < 100:
                grid[row, col + k] = level
    return grid


def symbols(path, kind):
    """MetPy positions (east, north km) of STI storm IDs or TVS symbols."""
    f = Level3File(str(path))
    out = []
    for layer in getattr(f, "sym_block", []):
        for p in layer:
            if kind == "sti" and p.get("type") == "Storm ID":
                out.append((p["id"].strip(), p["x"], p["y"]))
            if kind == "tvs" and p.get("type") == "TVS":
                out.append((None, p["x"], p["y"]))
    return out


def files_part(entries):
    out = []
    for entry in entries.values():
        if not {"product:74", "product:83"} & set(entry.get("tags", [])):
            continue
        lat, lon, _ = rcm_text(ROOT / "testdata" / entry["committed"])
        hx, hy = hrap(lat, lon)
        west, north = fine_grid(float(hx), float(hy))
        boxes = []
        for row, col in ((0, 0), (0, 99), (49, 49), (99, 0), (99, 99)):
            blon, blat = lonlat(west + (col + 0.5) * FINE, north - (row + 0.5) * FINE)
            boxes.append({"row": row, "column": col, "latitude": blat, "longitude": blon})
        out.append({"id": entry["id"], "latitude": lat, "longitude": lon, "hrap_x": float(hx),
                     "hrap_y": float(hy), "west": west, "north": north, "boxes": boxes})
    return out


def dhr_points(path):
    f = Level3File(str(path))
    packet = f.sym_block[0][0]
    dbz = np.ma.filled(np.ma.masked_invalid(np.ma.asarray(f.map_data(np.array(packet["data"])))), -99.0)
    start = np.array(packet["start_az"], dtype=float)
    end = np.array(packet["end_az"], dtype=float)
    az = np.where(end < start, (start + end + 360.0) / 2.0, (start + end) / 2.0) % 360.0
    rng = (np.arange(dbz.shape[1]) + 0.5) * 1.0
    a, r = np.meshgrid(az, rng, indexing="ij")
    lat, lon = destination(f.lat, f.lon, a.ravel(), r.ravel())
    hx, hy = hrap(lat, lon)
    return hx, hy, dbz.ravel()


def intensity_agreement(points, rcm, west, north):
    hx, hy, dbz = points
    row, col = fine_index(hx, hy, west, north)
    ok = (row >= 0) & (row < 100) & (col >= 0) & (col < 100)
    mx = np.full((100, 100), -99.0)
    np.maximum.at(mx, (row[ok], col[ok]), dbz[ok])
    category = np.searchsorted(THRESHOLDS, mx, side="right")
    compared = (mx > -99.0) & (rcm <= 6)
    return int((category[compared] == rcm[compared]).sum()), int(compared.sum())


def case_part(case, cache, entries):
    rcm_path, rcm_src = source(case["rcm"], cache, entries)
    lat, lon, flat = rcm_text(rcm_path)
    hx, hy = (float(v) for v in hrap(lat, lon))
    part_a = flat[flat.find("/NEXRAA"):flat.find("/ENDAA")]
    part_c = flat[flat.find("/NEXRCC"):]
    named_cells = dict(re.findall(r"C(..)([A-Y]{2}[A-P])\d{6}", part_a[part_a.find("/NCEN"):]))
    named_tvs = re.findall(r"TVS\d\d([A-Y]{2}[A-P])", part_c)
    inputs = {"rcm": rcm_src}
    features = []
    sti_path, inputs["sti"] = source(case["sti"], cache, entries)
    cells = {cid: (x, y) for cid, x, y in symbols(sti_path, "sti")}
    for cid, name in sorted(named_cells.items()):
        if cid.strip() in cells:
            features.append(("centroid " + cid.strip(), name, [cells[cid.strip()]]))
    tvs_path, inputs["tvs"] = source(case["tvs"], cache, entries)
    tvs = [(x, y) for _, x, y in symbols(tvs_path, "tvs")]
    for name in named_tvs:
        features.append(("tvs", name, tvs))
    positions = [[tuple(float(v) for v in east_north_to_hrap(lat, lon, x, y)) for x, y in cands]
                 for _, _, cands in features]
    km_per_unit = MESH_KM * (1 + math.sin(math.radians(lat))) / (1 + math.sin(math.radians(60.0)))

    def outside_km(dx, dy):
        """Per feature: distance (km) from the nearest candidate to the named box."""
        west, north = fine_grid(hx, hy, dx, dy)
        out = []
        for (_, name, _), cands in zip(features, positions):
            row, col = box(name)
            x0, x1 = west + col * FINE, west + (col + 1) * FINE
            y1, y0 = north - row * FINE, north - (row + 1) * FINE
            out.append(min(math.hypot(max(x0 - px, 0.0, px - x1), max(y0 - py, 0.0, py - y1))
                           for px, py in cands) * km_per_unit)
        return out

    west, north = fine_grid(hx, hy)
    rows = []
    for (what, name, _), cands, dist in zip(features, positions, outside_km(0.0, 0.0)):
        got = [label(*(int(v) for v in fine_index(px, py, west, north))) for px, py in cands]
        rows.append({"feature": what, "message_box": name, "rule_boxes": got, "match": name in got,
                     "outside_km": round(dist, 3)})
    fitting = []
    for dx in np.arange(0.0, QUARTER, 0.25):
        for dy in np.arange(0.0, QUARTER, 0.25):
            sx = float(dx - QUARTER if dx >= QUARTER / 2 else dx)
            sy = float(dy - QUARTER if dy >= QUARTER / 2 else dy)
            if max(outside_km(sx, sy)) <= TOLERANCE_KM:
                fitting.append([sx, sy])
    dhr_path, inputs["dhr"] = source(case["dhr"], cache, entries)
    points = dhr_points(dhr_path)
    grid = rcm_grid(flat)
    rule_agree, compared = intensity_agreement(points, grid, *fine_grid(hx, hy))
    scan = []
    for dx in np.arange(-2.5, 2.51, 0.25):
        for dy in np.arange(-2.5, 2.51, 0.25):
            agree, _ = intensity_agreement(points, grid, *fine_grid(hx, hy, dx, dy))
            scan.append((agree, float(dx), float(dy)))
    best = max(a for a, _, _ in scan)
    return {
        "name": case["name"], "inputs": inputs, "latitude": lat, "longitude": lon,
        "hrap_x": hx, "hrap_y": hy, "features": rows,
        "features_matching": sum(r["match"] for r in rows),
        "max_outside_km": max(r["outside_km"] for r in rows),
        "alignments_within_tolerance": sorted(fitting),
        "intensity": {"compared_boxes": compared, "agree_rule": rule_agree, "agree_best": best,
                      "best_offsets": sorted([dx, dy] for a, dx, dy in scan if a == best)},
    }


def levels_part(entries):
    rcm_path = ROOT / "testdata" / entries["l3-tlx-rcm-20130520-2016"]["committed"]
    lat, lon, flat = rcm_text(rcm_path)
    hx, hy = (float(v) for v in hrap(lat, lon))
    grid = rcm_grid(flat)
    f = Level3File(str(ROOT / "testdata" / entries["l3-tlx-ncz-20130520-2016"]["committed"]))
    packet = [p for p in f.sym_block[0] if isinstance(p, dict) and "data" in p][0]
    z = np.ma.filled(np.ma.masked_invalid(np.ma.asarray(f.map_data(np.array(packet["data"])))), -99.0)
    ny, nx_ = z.shape
    cell = 2.2 * 1.852
    xs = (np.arange(nx_) - nx_ / 2 + 0.5) * cell
    ys = (ny / 2 - np.arange(ny) - 0.5) * cell
    x, y = np.meshgrid(xs, ys)
    px, py = east_north_to_hrap(lat, lon, x.ravel(), y.ravel())
    west, north = fine_grid(hx, hy)
    row, col = fine_index(px, py, west, north)
    ok = (row >= 0) & (row < 100) & (col >= 0) & (col < 100)
    mx = np.full((100, 100), -99.0)
    np.maximum.at(mx, (row[ok], col[ok]), z.ravel()[ok])
    out = {}
    for level in (7, 8):
        sel = (grid == level) & (mx > -99.0)
        out[str(level)] = {"boxes": int((grid == level).sum()), "with_composite": int(sel.sum()),
                           "median_dbz": float(np.median(mx[sel]))}
    return {"case": "l3-tlx-rcm-20130520-2016 with l3-tlx-ncz-20130520-2016", "levels": out}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--cache", default=os.environ.get(
        "RECAST_RADAR_L3_CACHE", str(Path.home() / "radar-corpus" / "level3-rcm")))
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    entries = manifest()
    golden = {
        "files": files_part(entries),
        "cases": [case_part(case, args.cache, entries) for case in CASES],
        "levels": levels_part(entries),
    }
    text = json.dumps(golden, indent=1, sort_keys=True) + "\n"
    if args.check:
        if OUT.read_text() != text:
            sys.exit(f"{OUT} differs")
        return
    with open(OUT, "w", newline="\n") as out:
        out.write(text)
    for case in golden["cases"]:
        print(case["name"], "features", case["features_matching"], "/", len(case["features"]),
              "max outside km", case["max_outside_km"],
              "alignments", case["alignments_within_tolerance"], "intensity", case["intensity"])
    print(golden["levels"])


if __name__ == "__main__":
    main()
