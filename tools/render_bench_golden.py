#!/usr/bin/env python3
"""Golden values for the real-data tests of recast-radar-render and recast-radar-bench.

The Rust unit tests decode real corpus files with the workspace's own readers and compare
what the renderer and the dealias-eval metrics compute against the JSON files this script
writes:

    testdata/golden/bench/dealias_eval.json   crates/recast-radar-bench/src/dealias_eval.rs (mod tests)
    testdata/golden/render/ktlx2024.json      crates/recast-radar-render/src/lib.rs (mod tests)
    testdata/golden/render/ktlx1999.json      crates/recast-radar-render/src/lib.rs (mod tests)
    testdata/golden/render/ktlx2013.json      crates/recast-radar-render/src/lib.rs (mod tests)
    testdata/golden/render/kewx.json          crates/recast-radar-render/src/lib.rs (mod derived_product_tests)

Every input value comes from a reader that is independent of recast-radar-tools:

- Py-ART 2.2.5 ``pyart.io.nexrad_level2.NEXRADLevel2File``: ray azimuths, Nyquist velocities,
  moment gate geometry, raw gate codes (``get_data(..., raw_data=True)``: 0 = no data,
  1 = range folded) and the data-block scale and offset;
- MetPy 1.7.1 ``metpy.io.Level2File``: the scaled moment values, cross-checked against
  Py-ART's raw codes (the script fails when they disagree);
- Py-ART ``pyart.io.read_nexrad_archive`` and ``pyart.correct.dealias_region_based`` for the
  region-based unfolding of a real Doppler sweep, and ``pyart.retrieve.storm_relative_velocity``
  for storm-relative velocities.

The expected outputs are computed here with numpy from those inputs, by reference
implementations of the documented rules (module docs of the Rust sources):

- dealias-eval metrics (dealias-v4 spec section 10.2): residual fold-boundary pairs (adjacent
  finite 4-neighbour pairs, azimuth seam included when the sweep closes 360 degrees, with
  |dv| > 1.2 * min(Nyquist of the two rows)); percent of finite gates moved by more than one
  Nyquist velocity; isolated specks (4-connected components of at most 3 gates that lie more
  than one Nyquist off the upper median of their finite 8-neighbourhood, at least 4 neighbours);
- the valid extent of a moment row: one past the last gate whose raw code is not the no-data
  code;
- the nearest radial to a query azimuth (angular distance on the circle);
- the 4/3-Earth beam geometry (Doviak and Zrnic 1993) that maps a slant range at one tilt to
  ground range, for locating the volume's reflectivity maximum on the lowest tilt.

Where the Rust code compares in f32 the reference does the same arithmetic in float32.

Test files are read from the committed corpus (testdata/files) and from the shared download
cache that recast-radar-testdata fills (%LOCALAPPDATA%\\recast-radar-tools\\testdata, or
$RECAST_RADAR_TESTDATA); every file is checked against its manifest sha256. Run
``cargo test -p recast-radar-bench -p recast-radar-render`` once to download the full
volumes ``l2-kdvn-20200810-180401`` and ``l2-kewx-20160413-022531``.

Usage:
    python tools/render_bench_golden.py [bench|render ...]

With no arguments every golden file is regenerated. The committed files were written with
Python 3.13, numpy 2.5.3, MetPy 1.7.1 and Py-ART 2.2.5.
"""

import gzip
import hashlib
import io
import json
import logging
import math
import os
import sys
import tomllib
import warnings
from pathlib import Path

import numpy as np

warnings.filterwarnings("ignore")
logging.getLogger("metpy.io.nexrad").setLevel(logging.ERROR)

ROOT = Path(__file__).resolve().parent.parent
TESTDATA = ROOT / "testdata"
GOLDEN = TESTDATA / "golden"

F32 = np.float32
BOUNDARY_NYQUIST_FRAC = F32(1.2)
AZIMUTH_BIN_WIDTH_DEG = 0.1
AZIMUTH_BINS = 3600
EARTH_RADIUS_M = 6_371_000.0
EFFECTIVE_EARTH_RADIUS_M = EARTH_RADIUS_M * 4.0 / 3.0


# ----------------------------------------------------------------- corpus ---

def load_manifest():
    files = []
    paths = [TESTDATA / "manifest.toml"]
    paths += sorted(p / "manifest.toml" for p in TESTDATA.iterdir()
                    if p.is_dir() and (p / "manifest.toml").is_file())
    for path in paths:
        with open(path, "rb") as fh:
            files += tomllib.load(fh).get("file", [])
    return {entry["id"]: entry for entry in files}


MANIFEST = load_manifest()


def cache_dir():
    if os.environ.get("RECAST_RADAR_TESTDATA"):
        return Path(os.environ["RECAST_RADAR_TESTDATA"])
    for var, suffix in (("LOCALAPPDATA", ()), ("XDG_CACHE_HOME", ()), ("HOME", (".cache",))):
        if var == "LOCALAPPDATA" and os.name != "nt":
            continue
        if os.environ.get(var):
            return Path(os.environ[var], *suffix, "recast-radar-tools", "testdata")
    return ROOT / ".testdata-cache"


def corpus_path(entry_id):
    entry = MANIFEST[entry_id]
    if "committed" in entry:
        rel = Path(entry["committed"])
        path = ROOT / rel if rel.parts[0] == "testdata" else TESTDATA / rel
    else:
        path = cache_dir() / entry_id
    if not path.is_file():
        raise SystemExit(f"{entry_id}: {path} is missing (run a cargo test that downloads it)")
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    if digest != entry["sha256"]:
        raise SystemExit(f"{entry_id}: sha256 {digest} != manifest {entry['sha256']}")
    return path


def format_json(value, indent=0):
    """JSON with objects indented one key per line and arrays of numbers (or of short
    arrays) on a single line."""
    pad = " " * indent
    if isinstance(value, dict):
        if not value:
            return "{}"
        items = [f'{pad} {json.dumps(key)}: {format_json(item, indent + 1)}'
                 for key, item in value.items()]
        return "{\n" + ",\n".join(items) + "\n" + pad + "}"
    if isinstance(value, list) and any(isinstance(item, dict) for item in value):
        items = [f"{pad} {format_json(item, indent + 1)}" for item in value]
        return "[\n" + ",\n".join(items) + "\n" + pad + "]"
    return json.dumps(value, allow_nan=False, separators=(",", ":"))


def write_golden(relative, payload):
    path = GOLDEN / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    text = format_json(payload)
    json.loads(text)
    path.write_text(text + "\n", encoding="utf-8", newline="\n")
    print(f"wrote {path.relative_to(ROOT)} ({len(text)} bytes)")


def jf(value):
    """JSON float: None for NaN; float32 values keep their shortest repr."""
    value = float(value)
    return value if math.isfinite(value) else None


def jlist(values):
    return [jf(v) for v in values]


# --------------------------------------------------------- Level II (Py-ART) ---

def unwrapped_bytes(entry_id):
    raw = corpus_path(entry_id).read_bytes()
    if raw[:2] == b"\x1f\x8b":
        raw = gzip.decompress(raw)
    return raw


def level2_file(entry_id):
    from pyart.io.nexrad_level2 import NEXRADLevel2File
    return NEXRADLevel2File(io.BytesIO(unwrapped_bytes(entry_id)))


def level2_sweep(entry_id, scan, moment):
    """One moment of one sweep of a Level II file as Py-ART reads it: ray azimuths (f32,
    the file's angle field), per-ray Nyquist velocity, the data-block gate geometry, scale
    and offset, and the raw gate codes (0 = no data, 1 = range folded). The scaled values
    ((code - offset) / scale for codes >= 2, NaN otherwise) are cross-checked against
    MetPy."""
    f = level2_file(entry_id)
    info = f.scan_info()[scan]
    index = info["moments"].index(moment)
    gates = info["ngates"][index]
    codes = np.ma.filled(f.get_data(moment, gates, scans=[scan], raw_data=True), 0)
    codes = codes.astype(np.int64)
    records = [f.radial_records[i] for i in f.scan_msgs[scan]]
    if any(moment not in r or r[moment]["ngates"] != gates for r in records):
        raise SystemExit(f"{entry_id} scan {scan} {moment}: rays with missing or short data")
    scales = {(r[moment]["scale"], r[moment]["offset"], r[moment].get("word_size", 8))
              for r in records}
    if len(scales) != 1:
        raise SystemExit(f"{entry_id} scan {scan} {moment}: scale/offset change {scales}")
    (scale, offset, word_size), = scales
    values = np.where(codes >= 2, (codes.astype(np.float64) - offset) / scale, np.nan)
    values = values.astype(F32)
    metpy = metpy_values(entry_id, scan, moment)
    if metpy.shape != values.shape:
        raise SystemExit(f"{entry_id} scan {scan} {moment}: MetPy shape {metpy.shape} != {values.shape}")
    both = np.isfinite(metpy) & np.isfinite(values)
    if not np.array_equal(np.isfinite(metpy), np.isfinite(values)) or not np.allclose(
            metpy[both], values[both], rtol=0, atol=1e-4):
        raise SystemExit(f"{entry_id} scan {scan} {moment}: MetPy and Py-ART values differ")
    return {
        "entry": entry_id,
        "scan": scan,
        "moment": moment,
        "azimuth": f.get_azimuth_angles([scan]).astype(F32),
        "nyquist": f.get_nyquist_vel([scan]).astype(np.float64),
        "first_gate_m": int(info["first_gate"][index]),
        "gate_spacing_m": int(info["gate_spacing"][index]),
        "gates": int(gates),
        "scale": float(scale),
        "offset": float(offset),
        "word_size": int(word_size),
        "codes": codes,
        "values": values,
    }


METPY_NAMES = {"REF": b"REF", "VEL": b"VEL", "SW": b"SW ", "ZDR": b"ZDR", "PHI": b"PHI",
               "RHO": b"RHO", "CFP": b"CFP"}


def metpy_values(entry_id, scan, moment):
    from metpy.io import Level2File
    f = Level2File(io.BytesIO(unwrapped_bytes(entry_id)))
    rays = f.sweeps[scan]
    rows = []
    for ray in rays:
        if len(ray) == 5:
            blocks = ray[4]
            key = METPY_NAMES[moment]
            if key not in blocks and key.strip() in blocks:
                key = key.strip()
            data = blocks[key][1] if key in blocks else np.zeros(0)
        else:
            blocks = dict(ray[1])
            data = blocks[moment][1] if moment in blocks else np.zeros(0)
        rows.append(np.asarray(data, dtype=np.float64))
    gates = max(len(r) for r in rows)
    grid = np.full((len(rows), gates), np.nan, dtype=np.float64)
    for i, r in enumerate(rows):
        grid[i, :len(r)] = r
    return grid.astype(F32)


def row_valid_extents(codes):
    """One past the last gate whose raw code is not the no-data code (0), per row."""
    out = []
    for row in codes:
        nz = np.flatnonzero(row != 0)
        out.append(int(nz[-1]) + 1 if nz.size else 0)
    return out


# ----------------------------------------------------- dealias-eval metrics ---

def sweep_wraps(azimuths):
    rows = len(azimuths)
    if rows < 8:
        return False
    first, last = float(azimuths[0]), float(azimuths[-1])
    gap = min((first - last) % 360.0, (last - first) % 360.0)
    return gap <= 3.0 * (360.0 / rows)


def boundary_pairs(values, nyq, wraps):
    """Adjacent finite 4-neighbour pairs with |dv| > 1.2 * min(N_a, N_b), in f32."""
    v = values.astype(F32)
    nyq = nyq.astype(F32)
    count = 0
    a, b = v[:, :-1], v[:, 1:]
    thr = (BOUNDARY_NYQUIST_FRAC * nyq)[:, None]
    count += int(np.count_nonzero(np.isfinite(a) & np.isfinite(b) & (np.abs(a - b) > thr)))
    if v.shape[0] > 1:
        a, b = v[:-1, :], v[1:, :]
        thr = (BOUNDARY_NYQUIST_FRAC * np.minimum(nyq[:-1], nyq[1:]))[:, None]
        count += int(np.count_nonzero(np.isfinite(a) & np.isfinite(b) & (np.abs(a - b) > thr)))
        if wraps:
            a, b = v[-1, :], v[0, :]
            thr = BOUNDARY_NYQUIST_FRAC * min(nyq[-1], nyq[0])
            count += int(np.count_nonzero(np.isfinite(a) & np.isfinite(b) & (np.abs(a - b) > thr)))
    return count


def percent_modified(output, raw, nyq):
    out = output.astype(F32)
    src = raw.astype(F32)
    both = np.isfinite(out) & np.isfinite(src)
    moved = both & (np.abs(out - src) > nyq.astype(F32)[:, None])
    finite = int(np.count_nonzero(both))
    return 100.0 * int(np.count_nonzero(moved)) / finite if finite else 0.0, int(np.count_nonzero(moved)), finite


def speck_count(values, nyq, wraps):
    """4-connected components (<= 3 gates) of gates more than one Nyquist off the upper
    median of their finite 8-neighbourhood (>= 4 neighbours)."""
    v = values.astype(F32)
    rows, gates = v.shape
    flagged = np.zeros((rows, gates), dtype=bool)
    for row in range(rows):
        n = F32(nyq[row])
        for gate in range(gates):
            value = v[row, gate]
            if not np.isfinite(value):
                continue
            hood = []
            for dr in (-1, 0, 1):
                r = row + dr
                if wraps:
                    r %= rows
                elif r < 0 or r >= rows:
                    continue
                for dg in (-1, 0, 1):
                    if dr == 0 and dg == 0:
                        continue
                    g = gate + dg
                    if g < 0 or g >= gates:
                        continue
                    s = v[r, g]
                    if np.isfinite(s):
                        hood.append(s)
            if len(hood) < 4:
                continue
            hood.sort()
            if abs(F32(value - hood[len(hood) // 2])) > n:
                flagged[row, gate] = True
    seen = np.zeros((rows, gates), dtype=bool)
    specks = 0
    for start in zip(*np.nonzero(flagged)):
        if seen[start]:
            continue
        seen[start] = True
        stack = [start]
        size = 0
        while stack:
            row, gate = stack.pop()
            size += 1
            for nr, ng in ((row - 1, gate), (row + 1, gate), (row, gate - 1), (row, gate + 1)):
                if wraps and rows > 1:
                    nr %= rows
                elif nr < 0 or nr >= rows:
                    continue
                if ng < 0 or ng >= gates:
                    continue
                if flagged[nr, ng] and not seen[nr, ng]:
                    seen[nr, ng] = True
                    stack.append((nr, ng))
        if size <= 3:
            specks += 1
    return specks


def run_length(row):
    runs = []
    for k in row:
        k = int(k)
        if runs and runs[-1][0] == k:
            runs[-1][1] += 1
        else:
            runs.append([k, 1])
    return runs


def pyart_region_unwrap(entry_id, scan, raw_values, nyquist):
    """Fold numbers of Py-ART's region-based dealiasing of the given sweep: the output
    velocity is raw + 2 * Nyquist * k with integer k per gate."""
    import pyart
    radar = pyart.io.read_nexrad_archive(str(corpus_path(entry_id)))
    corrected = pyart.correct.dealias_region_based(radar, vel_field="velocity")["data"]
    start, end = radar.get_start_end(scan)
    out = np.ma.filled(corrected[start:end + 1, :raw_values.shape[1]].astype(np.float64), np.nan)
    raw = raw_values.astype(np.float64)
    if out.shape != raw.shape:
        raise SystemExit(f"{entry_id}: Py-ART sweep shape {out.shape} != {raw.shape}")
    if not np.array_equal(np.isfinite(out), np.isfinite(raw)):
        raise SystemExit(f"{entry_id}: Py-ART changed the finite mask")
    interval = 2.0 * nyquist[:, None]
    folds = (out - raw) / interval
    k = np.where(np.isfinite(folds), np.round(folds), 0.0)
    if np.nanmax(np.abs(folds - k)) > 1e-6:
        raise SystemExit(f"{entry_id}: Py-ART output is not raw + 2N*k")
    return k.astype(np.int64), out.astype(F32)


def dealias_case(entry_id, scan, label, with_pyart):
    sweep = level2_sweep(entry_id, scan, "VEL")
    values = sweep["values"]
    nyq = sweep["nyquist"].astype(F32)
    if not np.all(np.isfinite(nyq) & (nyq > 0)):
        raise SystemExit(f"{entry_id}: a ray has no Nyquist velocity")
    wraps = sweep_wraps(sweep["azimuth"])
    rows, gates = values.shape
    case = {
        "id": entry_id,
        "sweep": scan,
        "label": label,
        "rows": rows,
        "gates": gates,
        "first_azimuth_deg": jf(sweep["azimuth"][0]),
        "last_azimuth_deg": jf(sweep["azimuth"][-1]),
        "wraps": wraps,
        "nyquist_mps": jlist(sorted(set(float(n) for n in nyq))),
        "finite_gates": int(np.count_nonzero(np.isfinite(values))),
        "range_folded_gates": int(np.count_nonzero(sweep["codes"] == 1)),
        "boundary_pairs": boundary_pairs(values, nyq, wraps),
        "speck_count": speck_count(values, nyq, wraps),
    }
    if wraps:
        case["boundary_pairs_without_seam"] = boundary_pairs(values, nyq, False)
        case["speck_count_without_seam"] = speck_count(values, nyq, False)
    if with_pyart:
        k, out = pyart_region_unwrap(entry_id, scan, values, sweep["nyquist"])
        percent, moved, finite = percent_modified(out, values, nyq)
        case["pyart_region"] = {
            "unfolded_gates": moved,
            "finite_gates": finite,
            "percent_modified": percent,
            "max_abs_fold": int(np.max(np.abs(k))),
            "boundary_pairs": boundary_pairs(out, nyq, wraps),
            "speck_count": speck_count(out, nyq, wraps),
            "unwrap_runs": [run_length(row) for row in k],
        }
    return case


def section_bench():
    payload = {
        "source": "tools/render_bench_golden.py bench: Py-ART 2.2.5 raw codes and "
                  "dealias_region_based, MetPy 1.7.1 values, numpy reference metrics",
        "boundary_nyquist_fraction": 1.2,
        "cases": [
            dealias_case("l2-kdvn-20200810-180401-trim", 1,
                         "KDVN 2020-08-10 derecho, 0.44 deg Doppler cut, 286-346 deg", True),
            dealias_case("l2-kdvn-20200810-180401", 1,
                         "KDVN 2020-08-10 derecho, 0.44 deg Doppler cut, full circle (download)", False),
            dealias_case("l2-pgua-20230524-030945-trim", 1,
                         "PGUA 2023-05-24 Mawar, 0.50 deg Doppler cut, 67-187 deg", False),
        ],
    }
    write_golden("bench/dealias_eval.json", payload)


# ------------------------------------------------------------------ render ---

def azimuth_bin(azimuth_deg):
    return int(round(float(azimuth_deg) % 360.0 / AZIMUTH_BIN_WIDTH_DEG)) % AZIMUTH_BINS


def geometry_payload(sweep):
    return {
        "rows": int(sweep["values"].shape[0]),
        "gates": sweep["gates"],
        "first_gate_m": sweep["first_gate_m"],
        "gate_spacing_m": sweep["gate_spacing_m"],
        "scale": sweep["scale"],
        "offset": sweep["offset"],
        "word_size": sweep["word_size"],
        "azimuth_deg": jlist(sweep["azimuth"]),
    }


def interior_range_folded(codes, limit):
    """(row, gate) of range-folded gates whose four row/gate neighbours are range folded."""
    rf = codes == 1
    inner = rf[1:-1, 1:-1] & rf[:-2, 1:-1] & rf[2:, 1:-1] & rf[1:-1, :-2] & rf[1:-1, 2:]
    rows, gates = np.nonzero(inner)
    pairs = [[int(r) + 1, int(g) + 1] for r, g in zip(rows, gates)]
    step = max(1, len(pairs) // limit)
    return pairs[::step][:limit]


def neighbour_extent_pairs(azimuth, extents, limit):
    """Adjacent radials with different valid extents: the azimuth bin at the midpoint
    between them, the row with the longer extent, and a gate that only that row fills."""
    out = []
    for row in range(len(azimuth) - 1):
        short, long = extents[row], extents[row + 1]
        if short == long or min(short, long) == 0:
            continue
        gap = (float(azimuth[row + 1]) - float(azimuth[row])) % 360.0
        if gap == 0.0 or gap > 6.0:
            continue
        mid = (float(azimuth[row]) + gap / 2.0) % 360.0
        longer = row + 1 if long > short else row
        out.append({
            "rows": [row, row + 1],
            "extents": [short, long],
            "midpoint_azimuth_deg": mid,
            "azimuth_bin": azimuth_bin(mid),
            "longer_row": longer,
            "gate": max(short, long) - 1,
        })
    step = max(1, len(out) // limit)
    return out[::step][:limit]


def nearest_ray_queries(azimuth, limit):
    """Query azimuths at least 0.15 deg away from every midpoint between neighbouring
    radials (by their 0.1 deg bin centres), with the nearest radial by angular distance."""
    centres = np.array([azimuth_bin(a) * AZIMUTH_BIN_WIDTH_DEG for a in azimuth])
    order = np.argsort(centres)
    sorted_centres = centres[order]
    mids = (sorted_centres + np.diff(np.concatenate([sorted_centres, [sorted_centres[0] + 360.0]])) / 2.0) % 360.0
    queries = []
    for q in np.arange(0.37, 360.0, 3.0):
        d = np.abs((mids - q + 180.0) % 360.0 - 180.0)
        if d.min() < 0.15:
            continue
        dist = np.abs((centres - q + 180.0) % 360.0 - 180.0)
        nearest = int(np.argmin(dist))
        ties = np.flatnonzero(dist == dist[nearest])
        if len(ties) != 1:
            continue
        queries.append({"azimuth_deg": round(float(q), 3), "row": nearest})
    step = max(1, len(queries) // limit)
    return queries[::step][:limit]


def section_render():
    # KTLX 2024-03-15 split cut: sweep 0 surveillance REF (1832 gates), sweep 1 Doppler VEL.
    entry = "l2-ktlx-20240315-000217-trim"
    surveillance = level2_sweep(entry, 0, "REF")
    doppler_vel = level2_sweep(entry, 1, "VEL")
    doppler_ref = level2_sweep(entry, 1, "REF")
    ref_extents = row_valid_extents(surveillance["codes"])
    vel_extents = row_valid_extents(doppler_vel["codes"])
    rf_rows, rf_gates = np.nonzero(doppler_vel["codes"] == 1)
    rf_ref_rows, rf_ref_gates = np.nonzero(doppler_ref["codes"] == 1)
    import pyart
    radar = pyart.io.read_nexrad_archive(str(corpus_path(entry)))
    storm = {"direction_deg": 225.0, "speed_mps": 18.0}
    srv = pyart.retrieve.storm_relative_velocity(
        radar, direction=storm["direction_deg"], speed=storm["speed_mps"], field="velocity")
    start, end = radar.get_start_end(1)
    gates = doppler_vel["gates"]
    srv = np.ma.filled(srv[start:end + 1, :gates].astype(np.float64), np.nan)
    pyart_vel = np.ma.filled(radar.fields["velocity"]["data"][start:end + 1, :gates].astype(np.float64), np.nan)
    if not np.allclose(np.nan_to_num(pyart_vel), np.nan_to_num(doppler_vel["values"]), atol=1e-4):
        raise SystemExit(f"{entry}: Py-ART Radar velocity differs from the raw-code values")
    rng = np.random.default_rng(20260916)
    finite_rows, finite_gates = np.nonzero(np.isfinite(srv))
    pick = rng.choice(len(finite_rows), size=40, replace=False)
    srv_samples = []
    for i in sorted(pick):
        r, g = int(finite_rows[i]), int(finite_gates[i])
        srv_samples.append({
            "row": r,
            "gate": g,
            "code": int(doppler_vel["codes"][r, g]),
            "velocity_mps": jf(doppler_vel["values"][r, g]),
            "storm_relative_mps": jf(srv[r, g]),
        })
    codes_present = sorted(set(int(c) for c in np.unique(doppler_vel["codes"]) if c >= 2))
    custom_samples = [{"code": c, "velocity_mps": (c - doppler_vel["offset"]) / doppler_vel["scale"]}
                      for c in codes_present]
    payload = {
        "source": "tools/render_bench_golden.py render: Py-ART 2.2.5 raw codes, MetPy 1.7.1 "
                  "values, pyart.retrieve.storm_relative_velocity",
        "id": entry,
        "surveillance": {
            "sweep": 0,
            "moment": "REF",
            **geometry_payload(surveillance),
            "no_data_code": 0,
            "range_folded_code": 1,
            "range_folded_gates": int(np.count_nonzero(surveillance["codes"] == 1)),
            "max_range_m": surveillance["first_gate_m"] + surveillance["gate_spacing_m"] * surveillance["gates"],
            "valid_extent": ref_extents,
            "longer_extent_neighbours": neighbour_extent_pairs(surveillance["azimuth"], ref_extents, 24),
        },
        "doppler": {
            "sweep": 1,
            "moment": "VEL",
            **geometry_payload(doppler_vel),
            "no_data_code": 0,
            "range_folded_code": 1,
            "no_data_gates": int(np.count_nonzero(doppler_vel["codes"] == 0)),
            "range_folded_gates": int(len(rf_rows)),
            "range_folded_first": [[int(r), int(g)] for r, g in zip(rf_rows[:32], rf_gates[:32])],
            "range_folded_interior": interior_range_folded(doppler_vel["codes"], 16),
            "valid_extent": vel_extents,
            "codes_present": codes_present,
            "custom_table_samples": custom_samples,
            "storm_motion": storm,
            "storm_relative_samples": srv_samples,
        },
        "doppler_reflectivity": {
            "sweep": 1,
            "moment": "REF",
            **geometry_payload(doppler_ref),
            "no_data_code": 0,
            "range_folded_code": 1,
            "range_folded_gates": int(len(rf_ref_rows)),
            "range_folded_first": [[int(r), int(g)] for r, g in zip(rf_ref_rows[:32], rf_ref_gates[:32])],
            "range_folded_interior": interior_range_folded(doppler_ref["codes"], 16),
        },
    }
    write_golden("render/ktlx2024.json", payload)

    # KTLX 1999-05-04 Message 1 surveillance sweep: 367 radials at ~1 deg, 460 1 km gates.
    entry = "l2-ktlx-19990504-002218-trim"
    legacy = level2_sweep(entry, 0, "REF")
    payload = {
        "source": "tools/render_bench_golden.py render: Py-ART 2.2.5 raw codes, MetPy 1.7.1 values",
        "id": entry,
        "surveillance": {
            "sweep": 0,
            "moment": "REF",
            **geometry_payload(legacy),
            "no_data_code": 0,
            "valid_extent": row_valid_extents(legacy["codes"]),
            "median_spacing_deg": jf(np.median(np.diff(legacy["azimuth"].astype(np.float64)) % 360.0)),
            "nearest_ray_queries": nearest_ray_queries(legacy["azimuth"], 60),
        },
    }
    write_golden("render/ktlx1999.json", payload)

    # KTLX 2013-05-20 Build 13.2: PHI is a 16-bit moment.
    entry = "l2-ktlx-20130520-201643-trim"
    phi = level2_sweep(entry, 0, "PHI")
    finite_rows, finite_gates = np.nonzero(np.isfinite(phi["values"]))
    pick = rng.choice(len(finite_rows), size=24, replace=False)
    phi_samples = [{"row": int(finite_rows[i]), "gate": int(finite_gates[i]),
                    "code": int(phi["codes"][finite_rows[i], finite_gates[i]]),
                    "value_deg": jf(phi["values"][finite_rows[i], finite_gates[i]])}
                   for i in sorted(pick)]
    payload = {
        "source": "tools/render_bench_golden.py render: Py-ART 2.2.5 raw codes, MetPy 1.7.1 values",
        "id": entry,
        "differential_phase": {
            "sweep": 0,
            "moment": "PHI",
            **geometry_payload(phi),
            "no_data_code": 0,
            "max_code": int(phi["codes"].max()),
            "finite_gates": int(np.count_nonzero(np.isfinite(phi["values"]))),
            "samples": phi_samples,
        },
    }
    write_golden("render/ktlx2013.json", payload)

    # KEWX 2016-04-13 full volume: the reflectivity maximum of the whole volume.
    entry = "l2-kewx-20160413-022531"
    radar = pyart.io.read_nexrad_archive(str(corpus_path(entry)))
    ref = radar.fields["reflectivity"]["data"]
    peak = np.unravel_index(np.ma.argmax(ref), ref.shape)
    ray, gate = int(peak[0]), int(peak[1])
    sweep = int(np.searchsorted(radar.sweep_end_ray_index["data"], ray))
    slant_m = float(radar.range["data"][gate])
    elevation = float(radar.elevation["data"][ray])
    height_m = math.sqrt(slant_m ** 2 + EFFECTIVE_EARTH_RADIUS_M ** 2
                         + 2.0 * slant_m * EFFECTIVE_EARTH_RADIUS_M * math.sin(math.radians(elevation))) \
        - EFFECTIVE_EARTH_RADIUS_M
    ground_m = EFFECTIVE_EARTH_RADIUS_M * math.asin(
        slant_m * math.cos(math.radians(elevation)) / (EFFECTIVE_EARTH_RADIUS_M + height_m))
    lowest_start, lowest_end = radar.get_start_end(0)
    lowest = ref[lowest_start:lowest_end + 1]
    lowest_peak = np.unravel_index(np.ma.argmax(lowest), lowest.shape)
    payload = {
        "source": "tools/render_bench_golden.py render: pyart.io.read_nexrad_archive 2.2.5",
        "id": entry,
        "sweeps": int(radar.nsweeps),
        "lowest_sweep": {
            "rows": int(lowest_end - lowest_start + 1),
            "gates": int(ref.shape[1]),
            "first_gate_m": float(radar.range["data"][0]),
            "gate_spacing_m": float(radar.range["data"][1] - radar.range["data"][0]),
            "max_dbz": jf(lowest.max()),
            "max_azimuth_deg": jf(radar.azimuth["data"][lowest_start + lowest_peak[0]]),
            "max_range_m": float(radar.range["data"][lowest_peak[1]]),
        },
        "volume_max": {
            "dbz": jf(ref.max()),
            "count": int(np.count_nonzero(ref == ref.max())),
            "sweep": sweep,
            "elevation_deg": elevation,
            "azimuth_deg": jf(radar.azimuth["data"][ray]),
            "slant_range_m": slant_m,
            "ground_range_m": ground_m,
            "height_above_radar_m": height_m,
        },
    }
    write_golden("render/kewx.json", payload)


SECTIONS = {"bench": section_bench, "render": section_render}


def main(argv):
    names = argv or list(SECTIONS)
    for name in names:
        if name not in SECTIONS:
            raise SystemExit(f"unknown section {name}; choose from {', '.join(SECTIONS)}")
    for name in names:
        SECTIONS[name]()


if __name__ == "__main__":
    main(sys.argv[1:])
