#!/usr/bin/env python3
"""Golden values for the real-data tests of recast-radar-filters and recast-radar-map.

The Rust tests decode real corpus files with the crates' own readers and compare what
the filters and map products compute against the JSON files this script writes:

    testdata/golden/filters/gate_filter.json   crates/recast-radar-filters/tests/gate_filter_real.rs
    testdata/golden/filters/smooth.json        crates/recast-radar-filters/tests/smooth_real.rs
    testdata/golden/filters/interpolate.json   crates/recast-radar-filters/tests/interpolate_real.rs
    testdata/golden/map/rhi.json               crates/recast-radar-map/tests/rhi_real.rs
    testdata/golden/map/volumetric.json        crates/recast-radar-map/tests/volumetric_real.rs

Every input value comes from a reader that is independent of recast-radar-tools:

- NEXRAD Level II: MetPy 1.7.1 ``metpy.io.Level2File`` (ray azimuth and elevation, moment
  gate geometry and scaled gate values, Nyquist velocity). Py-ART 2.2.5 for
  ``pyart.filters.GateFilter`` and ``pyart.correct.dealias_region_based``.
- CfRadial: netCDF4-python 1.7.4 (``elevation``, ``azimuth``, ``range``, packed fields with
  ``_FillValue``/``scale_factor``, ``sweep_mode``).
- DORADE: the block walker below (SSWB/RADD/PARM/CELV/CSFD/CFAC/SWIB/RYIB/RDAT, HRD
  run-length decoding), written from the DORADE format description, not from the Rust
  reader.

The expected outputs are computed here with numpy from those inputs, by reference
implementations of the documented algorithms (module docs of the Rust sources):

- gate filter: Py-ART ``GateFilter.exclude_below('reflectivity', threshold)`` on Py-ART's
  velocity field;
- smoothing: the NaN-aware 3x3 binomial kernel ([1 2 1] x [1 2 1]) over azimuth x range,
  azimuth wrapping, range clamped, coverage never grown;
- display interpolation: the upsample policy table, cell-centred range subdivision, sub-rows
  only across believable azimuth gaps, nearest-parent coverage and echo edges, the velocity
  (30 m/s spread) and correlation-coefficient (0.97 floor) guards;
- RHI panels: 4/3-Earth beam geometry (Doviak and Zrnic 1993, eq. 2.28b/c) inverted per
  pixel, nearest beam within 1 degree and nearest gate;
- volume products: column walk over every reflectivity tilt at the lowest tilt's azimuths and
  ground ranges (nearest azimuth, nearest ground-range gate), composite = column maximum, echo
  top = highest beam with Z >= 18.3 dBZ, VIL (Greene and Clark 1972, 56 dBZ hail cap, surface
  layer from the lowest beam), SHI/MEHS (Witt et al. 1998), VIL density (VIL / echo top where
  the top is above 1.5 km), and MRMS-style vertical cross-sections (Zhang et al. 2005: linear
  in elevation angle between bracketing tilts, half-beamwidth edge extension, velocity guard).

Where the Rust code computes in f32 (azimuths, interpolation weights, gate values), the
reference does the same arithmetic in float32 so that values compare to 1e-4 or better.
Tie-breaking follows the documented nearest-neighbour rules (the lower neighbour on an exact
tie; for equal azimuths in one sweep, the later ray).

Test files are read from the committed corpus (testdata/files) and from the shared download
cache that recast-radar-testdata fills (%LOCALAPPDATA%\\recast-radar-tools\\testdata, or
$RECAST_RADAR_TESTDATA); every file is checked against its manifest sha256. Run
``cargo test -p recast-radar-map`` once to download the full volumes.

Usage:
    python tools/filters_map_golden.py [gate_filter|smooth|interpolate|rhi|volumetric ...]

With no arguments every golden file is regenerated. The committed files were written with
Python 3.13, numpy 2.5.3, MetPy 1.7.1, Py-ART 2.2.5 and netCDF4 1.7.4.
"""

import gzip
import hashlib
import json
import math
import os
import struct
import sys
import tomllib
import warnings
from pathlib import Path

import numpy as np

warnings.filterwarnings("ignore")

ROOT = Path(__file__).resolve().parent.parent
TESTDATA = ROOT / "testdata"
GOLDEN = TESTDATA / "golden"

F32 = np.float32
RNG_SEED = 20260916


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


def jf(value, digits=None):
    """JSON float: None for NaN; float32 values keep their shortest repr."""
    if value is None:
        return None
    value = float(value)
    if not math.isfinite(value):
        return None
    return round(value, digits) if digits is not None else value


def jlist(values, digits=None):
    return [jf(v, digits) for v in values]


def sample_indices(count, limit, seed_offset=0):
    if count <= limit:
        return np.arange(count)
    rng = np.random.default_rng(RNG_SEED + seed_offset)
    return np.sort(rng.choice(count, size=limit, replace=False))


# ------------------------------------------------------- f32 arithmetic ---

def rem_euclid32(x, m=360.0):
    """Rust f32::rem_euclid."""
    x = np.asarray(x, dtype=F32)
    m = F32(m)
    r = np.fmod(x, m)
    return np.where(r < 0, (r + m).astype(F32), r).astype(F32)


def signed_delta32(a, b):
    """Shortest signed step from a to b in (-180, 180], f32 (interpolate.rs)."""
    d = rem_euclid32(np.asarray(b, dtype=F32) - np.asarray(a, dtype=F32))
    return np.where(d > F32(180.0), (d - F32(360.0)).astype(F32), d).astype(F32)


def ang_dist32(a, b):
    """volumetric.rs ang_dist: min(d, 360 - d) with d = (a - b).rem_euclid(360), f32."""
    d = rem_euclid32(np.asarray(a, dtype=F32) - np.asarray(b, dtype=F32))
    return np.minimum(d, (F32(360.0) - d).astype(F32)).astype(F32)


def last_le_search(sorted_values, targets):
    """Rust slice::binary_search_by on sorted values: Ok(i) for the LAST equal element,
    else Err(insertion point). Returns (found, index) arrays."""
    idx = np.searchsorted(sorted_values, targets, side="right") - 1
    found = np.zeros(np.shape(targets), dtype=bool)
    ok = idx >= 0
    found[ok] = sorted_values[idx[ok]] == np.asarray(targets)[ok]
    insertion = np.where(found, idx, idx + 1)
    return found, insertion


# --------------------------------------------------------- Level II (MetPy) ---

MSG31_NAMES = {b"REF": "REF", b"VEL": "VEL", b"SW ": "SW", b"SW": "SW", b"ZDR": "ZDR",
               b"PHI": "PHI", b"RHO": "RHO", b"CFP": "CFP"}


def level2_sweeps(entry_id):
    """Sweeps of a Level II file as read by MetPy: per sweep the ray azimuths and elevations
    (f32 values of the file's angle fields), the Nyquist velocity of each ray, and per moment
    the rows (ray indices) carrying it, the gate geometry in metres and the scaled values
    (NaN below code 2, i.e. no data and range folded)."""
    import io
    import logging

    from metpy.io import Level2File

    logging.getLogger("metpy.io.nexrad").setLevel(logging.ERROR)
    raw = corpus_path(entry_id).read_bytes()
    if raw[:2] == bytes((0x1F, 0x8B)):
        raw = gzip.decompress(raw)
    f = Level2File(io.BytesIO(raw))
    sweeps = []
    for rays in f.sweeps:
        az, el, nyq = [], [], []
        moments = {}
        for index, ray in enumerate(rays):
            header = ray[0]
            az.append(header.az_angle)
            el.append(header.el_angle)
            if len(ray) == 5:
                blocks = {MSG31_NAMES.get(k, k.decode().strip()): v for k, v in ray[4].items()}
                nyq.append(ray[3].nyq_vel)
            else:
                blocks = dict(ray[1])
                nyq.append(header.nyq_vel)
            for name, (hdr, data) in blocks.items():
                m = moments.setdefault(name, {"rows": [], "data": [], "first": [], "spacing": []})
                m["rows"].append(index)
                m["data"].append(np.asarray(data, dtype=np.float64))
                m["first"].append(int(round(hdr.first_gate * 1000.0)))
                m["spacing"].append(int(round(hdr.gate_width * 1000.0)))
        for name, m in moments.items():
            if len(set(m["first"])) != 1 or len(set(m["spacing"])) != 1:
                raise SystemExit(f"{entry_id}: {name} gate geometry changes inside a sweep")
            gates = max(len(d) for d in m["data"])
            grid = np.full((len(m["rows"]), gates), np.nan)
            for row, d in enumerate(m["data"]):
                grid[row, :len(d)] = d
            m.update(first_gate_m=m["first"][0], gate_spacing_m=m["spacing"][0],
                     gate_count=gates, values=grid, rows=np.asarray(m["rows"]))
            del m["data"], m["first"], m["spacing"]
        sweeps.append({"az": np.asarray(az, dtype=F32), "el": np.asarray(el, dtype=F32),
                       "nyquist": np.asarray(nyq, dtype=np.float64), "moments": moments})
    return sweeps


# ------------------------------------------------------------- DORADE walker ---

def dorade_sweep(entry_id, field):
    """One DORADE sweep file: rays with RYIB azimuth/elevation (plus CFAC corrections) and
    status, the CELV/CSFD gate geometry, and `field` decoded from RDAT (16-bit, raw or HRD
    run-length) as value = stored / scale - bias with the PARM bad-data flag as NaN."""
    raw = corpus_path(entry_id).read_bytes()
    endian = "<" if struct.unpack_from("<i", raw, 4)[0] < 65536 else ">"

    def i16(b, o):
        return struct.unpack_from(endian + "h", b, o)[0]

    def i32(b, o):
        return struct.unpack_from(endian + "i", b, o)[0]

    def f32(b, o):
        return struct.unpack_from(endian + "f", b, o)[0]

    out = {"rays": [], "params": {}}
    cfac = (0.0, 0.0, 0.0)
    offset = 0
    current = None
    while offset + 8 <= len(raw):
        name = raw[offset:offset + 4].decode("latin-1")
        length = i32(raw, offset + 4)
        if length < 8 or offset + length > len(raw):
            break
        block = raw[offset:offset + length]
        if name == "RADD":
            out["scan_mode"] = i16(block, 50)
            out["compression"] = i16(block, 68)
        elif name == "PARM":
            pname = block[8:16].decode("latin-1").strip("\x00 ")
            out["params"][pname] = {"format": i16(block, 78), "scale": f32(block, 92),
                                    "bias": f32(block, 96), "bad": i32(block, 100)}
        elif name == "CELV":
            count = i32(block, 8)
            cells = np.frombuffer(block, dtype=endian + "f4", count=count, offset=12)
            out["first_cell_m"] = float(cells[0])
            out["cell_spacing_m"] = float(cells[1] - cells[0])
            out["cell_count"] = count
            # The line through the first and last cell centres: the uniform
            # range coordinate of the FM301 model (design note 6.6).
            out["uniform_spacing_m"] = (float(cells[-1]) - float(cells[0])) / (count - 1)
        elif name == "CSFD":
            out["first_cell_m"] = f32(block, 12)
            out["cell_spacing_m"] = f32(block, 16)
            out["cell_count"] = i16(block, 48)
        elif name == "CFAC":
            cfac = (f32(block, 8), f32(block, 12), f32(block, 16))
        elif name == "SWIB":
            out["fixed_angle_deg"] = f32(block, 32)
        elif name == "RYIB":
            current = {"azimuth_deg": F32(f32(block, 24)) + F32(cfac[0]),
                       "elevation_deg": F32(f32(block, 28)) + F32(cfac[1]),
                       "status": i32(block, 40), "data": None}
            out["rays"].append(current)
        elif name == "RDAT" and current is not None:
            pname = block[8:16].decode("latin-1").strip("\x00 ")
            if pname == field:
                current["data"] = block[16:]
        offset += length
    param = out["params"][field]
    cells = out["cell_count"]
    out["rdat_words"] = max(len(ray["data"]) // 2 for ray in out["rays"])
    for ray in out["rays"]:
        words = np.frombuffer(ray["data"], dtype=endian + "i2")
        if out["compression"] == 0 and np.any(words[cells:] != param["bad"]):
            raise SystemExit(f"{entry_id}: RDAT words past the {cells} cells carry data")
        if out["compression"] == 1:
            decoded = []
            i = 0
            while i < len(words) and len(decoded) < cells:
                control = int(words[i]) & 0xFFFF
                i += 1
                if control in (0, 1):
                    break
                if control & 0x8000:
                    n = control & 0x7FFF
                    decoded.extend(int(w) for w in words[i:i + n])
                    i += n
                else:
                    decoded.extend([param["bad"]] * control)
            words = np.asarray(decoded[:cells] + [param["bad"]] * (cells - len(decoded)))
        words = np.asarray(words[:cells], dtype=np.int64)
        values = words / param["scale"] - param["bias"]
        values = np.where(words == param["bad"], np.nan, values)
        ray["values"] = values
        del ray["data"]
    return out


# ---------------------------------------------------------------- gate filter ---

def section_gate_filter():
    import pyart

    entry = "l2-ktlx-20240315-000217-trim"
    sweep = 1
    radar = pyart.io.read_nexrad_archive(str(corpus_path(entry)))
    sl = radar.get_slice(sweep)
    velocity = radar.fields["velocity"]["data"][sl]
    vel_valid = ~np.ma.getmaskarray(velocity)
    cases = []
    for threshold in (0.0, 10.0, 20.0):
        gate_filter = pyart.filters.GateFilter(radar)
        gate_filter.exclude_below("reflectivity", threshold)
        kept = ~gate_filter.gate_excluded[sl] & vel_valid
        values = np.where(kept, np.ma.getdata(velocity), 0.0)
        cases.append({
            "threshold_dbz": threshold,
            "kept_total": int(kept.sum()),
            "kept_per_ray": [int(v) for v in kept.sum(axis=1)],
            "kept_sum_per_ray": jlist(values.sum(axis=1)),
        })

    # Legacy Message 1 Doppler sweep: velocity and spectrum width only (MetPy shows no
    # reflectivity gates in sweep 2 of the split cut).
    legacy = "l2-ktlx-19990504-002218-trim"
    legacy_sweeps = level2_sweeps(legacy)
    doppler = legacy_sweeps[1]["moments"]

    payload = {
        "source": "tools/filters_map_golden.py gate_filter; Py-ART 2.2.5 GateFilter.exclude_below"
                  " on read_nexrad_archive, MetPy 1.7.1 Level2File",
        "doppler_cut": {
            "id": entry,
            "sweep": sweep,
            "rays": int(sl.stop - sl.start),
            "velocity_valid_total": int(vel_valid.sum()),
            "reflectivity_valid_total": int(np.ma.count(radar.fields["reflectivity"]["data"][sl])),
            "thresholds": cases,
        },
        "velocity_only_cut": {
            "id": legacy,
            "sweep": 1,
            "moments": sorted(doppler),
            "velocity_valid_total": int(np.isfinite(doppler["VEL"]["values"]).sum()),
        },
        # JMA N6 is the radial-velocity product (GRIB2 parameter Pvr): no reflectivity in any
        # sweep. The count is the one the curation walker recorded in the manifest description
        # of jma-n6-20191012-090000-rs47773 (GRIB2 section and run-length walker).
        "velocity_only_volume": {
            "id": "jma-n6-20191012-090000-rs47773",
            "sweeps": 13,
            "velocity_valid_total": 547108,
        },
    }
    write_golden("filters/gate_filter.json", payload)


# ------------------------------------------------------------------- smoothing ---

def smooth_reference(values):
    """3x3 binomial NaN-aware smoothing: float64 sums (exact for half-dB values), f32 divide."""
    rows, gates = values.shape
    finite = np.isfinite(values)
    src = np.where(finite, values, 0.0)
    weight_src = finite.astype(np.float64)
    kernel = (1.0, 2.0, 1.0)
    total = np.zeros_like(src)
    weight = np.zeros_like(src)
    for dr, kr in zip((-1, 0, 1), kernel):
        row_src = np.roll(src, -dr, axis=0)
        row_w = np.roll(weight_src, -dr, axis=0)
        for dg, kg in zip((-1, 0, 1), kernel):
            shifted = np.zeros_like(src)
            shifted_w = np.zeros_like(src)
            if dg == -1:
                shifted[:, 1:] = row_src[:, :-1]
                shifted_w[:, 1:] = row_w[:, :-1]
            elif dg == 1:
                shifted[:, :-1] = row_src[:, 1:]
                shifted_w[:, :-1] = row_w[:, 1:]
            else:
                shifted, shifted_w = row_src, row_w
            total += shifted * (kr * kg)
            weight += shifted_w * (kr * kg)
    out = np.full(values.shape, np.nan, dtype=F32)
    ok = finite & (weight > 0)
    out[ok] = (total[ok].astype(F32) / weight[ok].astype(F32)).astype(F32)
    return out


def neighbourhood(values, row, gate):
    rows, gates = values.shape
    cells = []
    for dr in (-1, 0, 1):
        r = (row + dr) % rows
        for dg in (-1, 0, 1):
            g = gate + dg
            if 0 <= g < gates:
                cells.append(values[r, g])
    return np.asarray(cells)


def smooth_case(entry, sweep, full_circle, sample_limit=200):
    sweeps = level2_sweeps(entry)
    ref = sweeps[sweep]["moments"]["REF"]
    values = ref["values"]
    rows, gates = values.shape
    smoothed = smooth_reference(values)
    finite = np.isfinite(values)
    checked_rows = np.arange(rows) if full_circle else np.arange(1, rows - 1)

    constant, edge, steep = [], [], []
    for row in checked_rows:
        for gate in np.nonzero(finite[row])[0]:
            cells = neighbourhood(values, row, gate)
            valid = cells[np.isfinite(cells)]
            center = values[row, gate]
            if len(valid) == len(cells) and np.all(valid == center):
                constant.append((row, gate, center))
            elif len(valid) < len(cells) and np.all(valid == center):
                edge.append((row, gate, center))
            elif len(valid) == len(cells) == 9:
                steep.append((float(valid.max() - valid.min()), row, gate, float(valid.min()),
                              float(valid.max()), float(smoothed[row, gate])))
    steep.sort(key=lambda item: (-item[0], item[1], item[2]))

    def pick(items, limit, offset):
        return [items[i] for i in sample_indices(len(items), limit, offset)]

    row_valid = np.isfinite(smoothed).sum(axis=1)
    row_sum = np.where(np.isfinite(smoothed), smoothed.astype(np.float64), 0.0).sum(axis=1)
    samples = []
    rng = np.random.default_rng(RNG_SEED + sweep)
    candidates = np.argwhere(np.isfinite(smoothed[checked_rows]))
    for i in rng.choice(len(candidates), size=min(sample_limit, len(candidates)), replace=False):
        row, gate = candidates[i]
        row = checked_rows[row]
        samples.append([int(row), int(gate), jf(smoothed[row, gate])])
    samples.sort()
    return {
        "id": entry,
        "sweep": sweep,
        "rows": rows,
        "gates": gates,
        "full_circle": full_circle,
        "checked_rows": [int(checked_rows[0]), int(checked_rows[-1])],
        "native_valid_total": int(finite.sum()),
        "native_row_valid": [int(v) for v in finite.sum(axis=1)],
        "row_valid": [int(v) for v in row_valid],
        "row_sum": jlist(row_sum),
        "samples": samples,
        "constant_neighbourhood": {
            "count": len(constant),
            "samples": [[int(r), int(g), jf(v)] for r, g, v in pick(constant, sample_limit, 1)],
        },
        "edge_unchanged": {
            "count": len(edge),
            "samples": [[int(r), int(g), jf(v)] for r, g, v in pick(edge, sample_limit, 2)],
        },
        "steepest": [
            {"row": int(r), "gate": int(g), "spread": jf(s), "min": jf(lo), "max": jf(hi),
             "smoothed": jf(v)}
            for s, r, g, lo, hi, v in steep[:50]
        ],
    }


def section_smooth():
    payload = {
        "source": "tools/filters_map_golden.py smooth; MetPy 1.7.1 Level2File reflectivity,"
                  " numpy 3x3 binomial reference",
        "cases": [
            smooth_case("l2-kdvn-20200810-180401-trim", 0, full_circle=False),
            smooth_case("l2-ktlx-20130520-201643-trim", 0, full_circle=False),
            smooth_case("l2-ktlx-19990504-002218-trim", 0, full_circle=True),
        ],
    }
    write_golden("filters/smooth.json", payload)


# ------------------------------------------------------- display interpolation ---

TARGET_AZIMUTH_DEG = F32(0.25)
TARGET_GATE_SPACING_M = 250
MAX_FACTOR = 4
MAX_GRID_BYTES = 64 << 20
ROW_LIMIT = 1 << 15
MAX_GATES = (1 << 16) - 1
MAX_AZIMUTH_HALF_WIDTH_DEG = F32(3.0)
VELOCITY_GUARD_SPREAD_MPS = F32(30.0)
CC_GUARD_FLOOR = F32(0.97)
MIN_SYNTH_DELTA_DEG = F32(0.01)


def upsample_factors(nominal_deg, spacing_m, rows, gates):
    """The documented policy (interpolate.rs module docs): <= 0.25 deg and <= 250 m, at most
    4x per axis, exact integer sub-gate geometry, packed row/gate limits, 64 MB budget."""
    azimuth = 1
    nominal_deg = F32(nominal_deg)
    if np.isfinite(nominal_deg) and nominal_deg > 0:
        while azimuth < MAX_FACTOR and nominal_deg / F32(azimuth) > TARGET_AZIMUTH_DEG + F32(1e-3):
            azimuth += 1
    rng = 1
    if spacing_m > 0:
        while rng < MAX_FACTOR and F32(spacing_m) / F32(rng) > F32(TARGET_GATE_SPACING_M):
            rng += 1

        def exact(factor):
            return spacing_m % factor == 0 and (spacing_m - spacing_m // factor) % 2 == 0

        while rng > 1 and not exact(rng):
            rng -= 1
    while azimuth > 1 and rows * azimuth >= ROW_LIMIT:
        azimuth -= 1
    while rng > 1 and gates * rng > MAX_GATES:
        rng -= 1
    while rows * azimuth * gates * rng * 4 > MAX_GRID_BYTES:
        if rng > 1:
            rng -= 1
        elif azimuth > 1:
            azimuth -= 1
        else:
            break
    return azimuth, rng


def rust_div2(value):
    """Rust integer division by 2 (truncates toward zero)."""
    return int(value / 2)


def upsample_reference(azimuths, values, spacing_m, policy):
    """Reference display upsampling of one moment grid (rows x gates, NaN = no data)."""
    rows, gates = values.shape
    az = rem_euclid32(azimuths)
    deltas = signed_delta32(az, np.roll(az, -1))
    magnitudes = np.sort(np.abs(deltas[np.isfinite(deltas)]).astype(F32))
    nominal = magnitudes[len(magnitudes) // 2]
    fa, fr = upsample_factors(nominal, spacing_m, rows, gates)
    result = {"nominal_azimuth_deg": nominal, "factors": (fa, fr), "identity": fa <= 1 and fr <= 1}
    if result["identity"]:
        return result
    gap_limit = min(F32(nominal * F32(2.0)), F32(MAX_AZIMUTH_HALF_WIDTH_DEG * F32(2.0)))
    plan = []
    for row in range(rows):
        plan.append((row, row, F32(0.0), az[row]))
        delta = deltas[row]
        if fa > 1 and abs(delta) >= MIN_SYNTH_DELTA_DEG and abs(delta) <= gap_limit:
            hi = (row + 1) % rows
            for step in range(1, fa):
                t = F32(step) / F32(fa)
                plan.append((row, hi, t, rem_euclid32(az[row] + t * delta)[()]))

    # Cell-centred range subdivision: sub-gate centres between native gate centres.
    new_gates = gates * fr
    sub = np.arange(new_gates)
    x = ((sub.astype(F32) + F32(0.5)) / F32(fr) - F32(0.5)).astype(F32)
    nearest = sub // fr
    inner = (x > 0) & (x < gates - 1)
    lo = np.where(x <= 0, 0, np.where(x >= gates - 1, gates - 1, np.floor(x).astype(np.int64)))
    hi = np.where(inner, lo + 1, lo)
    u = np.where(inner, (x - lo.astype(F32)).astype(F32), F32(0.0)).astype(F32)

    src = values.astype(F32)
    out = np.full((len(plan), new_gates), np.nan, dtype=F32)
    classes = {name: np.zeros((len(plan), new_gates), dtype=bool)
               for name in ("edge", "guarded", "uniform", "blend", "boundary_blocked")}
    for index, (row_lo, row_hi, t, _) in enumerate(plan):
        a = src[row_lo]
        b = src[row_hi]
        near = (a if t <= 0.5 else b)[nearest]
        valid = np.isfinite(near)
        if t == F32(0.5):
            # A sub-row on the beam boundary renders only where both beams have echo.
            blocked = valid & ~np.isfinite(b[nearest])
            classes["boundary_blocked"][index] = blocked
            valid &= ~blocked
        v00, v01, v10, v11 = a[lo], a[hi], b[lo], b[hi]
        all4 = np.isfinite(v00) & np.isfinite(v01) & np.isfinite(v10) & np.isfinite(v11)
        with np.errstate(invalid="ignore"):
            mn = np.minimum(np.minimum(v00, v01), np.minimum(v10, v11))
            mx = np.maximum(np.maximum(v00, v01), np.maximum(v10, v11))
            if policy == "cc":
                guarded = all4 & (mn < CC_GUARD_FLOOR)
            elif policy == "velocity":
                guarded = all4 & ((mx - mn).astype(F32) > VELOCITY_GUARD_SPREAD_MPS)
            else:
                guarded = np.zeros(new_gates, dtype=bool)
            lo_v = (v00 + (v01 - v00) * u).astype(F32)
            hi_v = (v10 + (v11 - v10) * u).astype(F32)
            blend = (lo_v + (hi_v - lo_v) * t).astype(F32)
        out[index] = np.where(valid, np.where(all4 & ~guarded, blend, near), np.nan)
        uniform = all4 & (v00 == v01) & (v01 == v10) & (v10 == v11)
        classes["edge"][index] = valid & ~all4
        classes["guarded"][index] = valid & guarded
        classes["uniform"][index] = valid & uniform & ~guarded
        classes["blend"][index] = valid & all4 & ~guarded & ~uniform
    result.update(gap_limit_deg=gap_limit, plan=plan, out_gate_count=new_gates, values=out,
                  classes=classes, lo=lo, hi=hi, nearest=nearest)
    return result


def interpolation_case(label, entry, sweep, moment, azimuths, radial_rows, values, first_gate_m,
                       spacing_m, policy, extra=None, sample_limit=100):
    ref = upsample_reference(azimuths, values, spacing_m, policy)
    case = {"label": label, "id": entry, "sweep": sweep, "moment": moment, "policy": policy,
            "rows": int(values.shape[0]), "gates": int(values.shape[1]),
            "first_gate_m": first_gate_m, "gate_spacing_m": spacing_m,
            "native_valid_total": int(np.isfinite(values).sum()),
            "native_row_valid": [int(v) for v in np.isfinite(values).sum(axis=1)],
            "native_azimuths_deg": jlist(rem_euclid32(azimuths), 5),
            "nominal_azimuth_deg": jf(ref["nominal_azimuth_deg"]),
            "factors": list(ref["factors"]), "identity": bool(ref["identity"])}
    case.update(extra or {})
    if ref["identity"]:
        return case
    fa, fr = ref["factors"]
    sub = spacing_m // fr
    out = ref["values"]
    plan = ref["plan"]
    case.update({
        "gap_limit_deg": jf(ref["gap_limit_deg"]),
        "out_first_gate_m": first_gate_m + rust_div2(sub - spacing_m),
        "out_gate_spacing_m": sub,
        "out_gate_count": int(ref["out_gate_count"]),
        "out_rows": len(plan),
        "row_azimuths_deg": jlist([p[3] for p in plan], 5),
        "row_parents": [[int(p[0]), int(p[1]), jf(p[2])] for p in plan],
        "row_radial_index": [int(radial_rows[p[0] if p[2] <= 0.5 else p[1]]) for p in plan],
        "row_valid": [int(v) for v in np.isfinite(out).sum(axis=1)],
        "row_sum": jlist(np.where(np.isfinite(out), out.astype(np.float64), 0.0).sum(axis=1), 4),
        "sample_layout": ["row", "gate", "value", "nearest_native", "parents_min", "parents_max"],
    })
    classes = {}
    for offset, (name, mask) in enumerate(ref["classes"].items()):
        cells = np.argwhere(mask)
        samples = []
        for i in sample_indices(len(cells), sample_limit, 10 + offset):
            row, gate = (int(v) for v in cells[i])
            lo_row, hi_row, t, _ = plan[row]
            nearest_native = values[lo_row if t <= 0.5 else hi_row, int(ref["nearest"][gate])]
            lo, hi = int(ref["lo"][gate]), int(ref["hi"][gate])
            parents = np.asarray([values[lo_row, lo], values[lo_row, hi], values[hi_row, lo],
                                  values[hi_row, hi]])
            finite = parents[np.isfinite(parents)]
            samples.append([row, gate, jf(out[row, gate]), jf(nearest_native),
                            jf(finite.min()) if len(finite) else None,
                            jf(finite.max()) if len(finite) else None])
        classes[name] = {"count": int(mask.sum()), "samples": samples}
    case["classes"] = classes
    return case


def level2_moment_case(label, entry, sweep, moment, policy, sweeps=None):
    sweeps = sweeps or level2_sweeps(entry)
    s = sweeps[sweep]
    m = s["moments"][moment]
    return interpolation_case(label, entry, sweep, moment, s["az"][m["rows"]], m["rows"],
                              m["values"], m["first_gate_m"], m["gate_spacing_m"], policy,
                              extra={"nyquist_mps": jf(np.median(s["nyquist"][m["rows"]]))})


def section_interpolate():
    import netCDF4

    cases = [
        level2_moment_case("legacy_reflectivity", "l2-ktlx-19990504-002218-trim", 0, "REF",
                           "linear"),
        level2_moment_case("aliased_velocity", "l2-klix-20050829-130035-trim", 1, "VEL",
                           "velocity"),
        level2_moment_case("correlation_coefficient", "l2-kgwx-20130601-235640", 0, "RHO", "cc"),
    ]

    noxp = dorade_sweep("dorade-noxp-20090525-203211-sector", "DZ")
    rays = [ray for ray in noxp["rays"] if ray["status"] == 0]
    cases.append(interpolation_case(
        "sector", "dorade-noxp-20090525-203211-sector", 0, "DZ",
        np.asarray([ray["azimuth_deg"] for ray in rays], dtype=F32), np.arange(len(rays)),
        np.vstack([ray["values"] for ray in rays]), int(round(noxp["first_cell_m"])),
        int(round(noxp["cell_spacing_m"])), "linear",
        # CSFD declares 1001 cells; each uncompressed RDAT block holds 1002 words because the
        # block is padded to a 4-byte boundary with one bad-data word.
        extra={"csfd_cell_count": noxp["cell_count"], "rdat_word_count": noxp["rdat_words"]}))

    # DOW8 RHI through netCDF4: azimuth steps far below 0.25 deg and 125 m gates. The first
    # gate is the CfRadial reader's documented start of gate 0 (range[0] - spacing / 2).
    dow8 = netCDF4.Dataset(str(corpus_path("cfrad1-dow8-20211011-223602-rhi-trim3-classic")))
    rng = np.asarray(dow8.variables["range"][:], dtype=np.float64)
    spacing = round(rng[1] - rng[0])
    dbz = np.ma.filled(dow8.variables["DBZHC"][:].astype(np.float64), np.nan)
    cases.append(interpolation_case(
        "fine_rhi", "cfrad1-dow8-20211011-223602-rhi-trim3-classic", 0, "DBZHC",
        np.asarray(dow8.variables["azimuth"][:], dtype=F32), np.arange(dbz.shape[0]), dbz,
        int(round(rng[0] - spacing / 2.0)), int(spacing), "linear"))

    # Whole legacy volume: every sweep and moment, output dimensions only.
    grids = []
    for index, s in enumerate(level2_sweeps("l2-ktlx-19990504-002218")):
        for name, m in sorted(s["moments"].items()):
            ref = upsample_reference(s["az"][m["rows"]], m["values"], m["gate_spacing_m"], "linear")
            item = {"sweep": index, "moment": name, "rows": int(len(m["rows"])),
                    "gates": int(m["gate_count"]), "gate_spacing_m": m["gate_spacing_m"],
                    "factors": list(ref["factors"])}
            if not ref["identity"]:
                item.update(out_rows=len(ref["plan"]), out_gate_count=int(ref["out_gate_count"]),
                            out_gate_spacing_m=m["gate_spacing_m"] // ref["factors"][1])
            grids.append(item)

    payload = {
        "source": "tools/filters_map_golden.py interpolate; MetPy 1.7.1 Level2File, netCDF4 1.7.4,"
                  " DORADE block walker; numpy float32 reference of the display upsampler",
        "cases": cases,
        "volume": {"id": "l2-ktlx-19990504-002218", "grids": grids},
    }
    write_golden("filters/interpolate.json", payload)


# ------------------------------------------------------------------ RHI panels ---

EARTH_RADIUS_M = 6_371_000.0
EFFECTIVE_EARTH_RADIUS_M = EARTH_RADIUS_M * 4.0 / 3.0
MAX_BEAM_GAP_DEG = F32(1.0)


def beam_height_m(slant_m, elevation_deg):
    """Doviak and Zrnic (1993) eq. 2.28b, 4/3 Earth (scalar)."""
    ae = EFFECTIVE_EARTH_RADIUS_M
    theta = elevation_deg * (math.pi / 180.0)
    return math.sqrt(slant_m * slant_m + ae * ae + 2.0 * slant_m * ae * math.sin(theta)) - ae


def beam_ground_range_m(slant_m, elevation_deg):
    """Doviak and Zrnic (1993) eq. 2.28c, 4/3 Earth (scalar)."""
    ae = EFFECTIVE_EARTH_RADIUS_M
    theta = elevation_deg * (math.pi / 180.0)
    h = beam_height_m(slant_m, elevation_deg)
    return ae * math.asin((slant_m * math.cos(theta)) / (ae + h))


def invert_beam(ground_m, height_m):
    """Ground range and height above the radar to (slant range, elevation deg): law of
    cosines on the effective Earth sphere."""
    ae = EFFECTIVE_EARTH_RADIUS_M
    target = ae + height_m
    phi = ground_m / ae
    sin_phi, cos_phi = math.sin(phi), math.cos(phi)
    slant = math.sqrt(ae * ae + target * target - 2.0 * ae * target * cos_phi)
    elevation = math.atan2(target * cos_phi - ae, target * sin_phi)
    return slant, math.degrees(elevation)


def rust_round(x):
    """f64::round: half away from zero."""
    return math.floor(x + 0.5) if x >= 0 else -math.floor(-x + 0.5)


def rhi_panel(elevations, values, first_gate_m, spacing_m, width, height, top_m, max_range_m,
              ray_ids, sample_limit=150, seed=0):
    """Reference native RHI panel: every pixel inverse-mapped to (slant, elevation), nearest gate
    (centre at first_gate_m + i * spacing) and nearest beam within 1 degree."""
    elevations = np.asarray(elevations, dtype=F32)
    order = np.argsort(elevations, kind="stable")
    sorted_el = elevations[order]
    gates = values.shape[1]
    panel = np.full((height, width), np.nan)
    kind = np.zeros((height, width), dtype=np.int8)  # 0 data, 1 no data, 2 no gate, 3 no beam
    picks = {}
    for y in range(height):
        z = float(F32(top_m)) * (1.0 - y / (height - 1))
        for x in range(width):
            s = float(F32(max_range_m)) * x / (width - 1)
            slant, elevation = invert_beam(s, z)
            gate = (slant - first_gate_m) / spacing_m
            if gate < -0.5 or gate >= gates - 0.5:
                kind[y, x] = 2
                continue
            gate = max(rust_round(gate), 0)
            e = F32(elevation)
            index = int(np.searchsorted(sorted_el, e, side="left"))
            before = index - 1 if index > 0 else None
            after = index if index < len(sorted_el) else None
            if before is not None and after is not None:
                pick = before if abs(F32(e - sorted_el[before])) <= abs(F32(sorted_el[after] - e)) \
                    else after
            else:
                pick = before if before is not None else after
            if abs(F32(sorted_el[pick] - e)) > MAX_BEAM_GAP_DEG:
                kind[y, x] = 3
                continue
            row = int(order[pick])
            value = values[row, gate]
            panel[y, x] = value
            kind[y, x] = 0 if np.isfinite(value) else 1
            picks[(y, x)] = (row, gate)
    finite = np.isfinite(panel)
    cells = [key for key in sorted(picks) if np.isfinite(panel[key])]
    samples = []
    for i in sample_indices(len(cells), sample_limit, seed):
        y, x = cells[i]
        row, gate = picks[(y, x)]
        samples.append([x, y, int(ray_ids[row]), row, gate, jf(panel[y, x])])
    return {
        "width": width, "height": height, "top_m": top_m, "max_range_m": max_range_m,
        "row_valid": [int(v) for v in finite.sum(axis=1)],
        "row_sum": jlist(np.where(finite, panel, 0.0).sum(axis=1), 4),
        "data_pixels": int((kind == 0).sum()),
        "no_data_pixels": int((kind == 1).sum()),
        "beyond_gates_pixels": int((kind == 2).sum()),
        "no_beam_pixels": int((kind == 3).sum()),
        "no_beam_samples": [[int(y), int(x)] for y, x in np.argwhere(kind == 3)[
            sample_indices(int((kind == 3).sum()), 50, seed + 1)]],
        "beyond_gates_samples": [[int(y), int(x)] for y, x in np.argwhere(kind == 2)[
            sample_indices(int((kind == 2).sum()), 50, seed + 2)]],
        "sample_layout": ["x", "y", "file_ray", "grid_row", "gate", "value"],
        "samples": samples,
    }


def circular_mean_deg(azimuths):
    sin_sum = 0.0
    cos_sum = 0.0
    for az in azimuths:
        radians = float(az) * (math.pi / 180.0)
        sin_sum += math.sin(radians)
        cos_sum += math.cos(radians)
    return float(F32(math.degrees(math.atan2(sin_sum, cos_sum)) % 360.0))


def rhi_geometry(elevations, azimuths, first_gate_m, spacing_m, gates):
    max_slant = first_gate_m + spacing_m * gates
    elevations = [float(e) for e in np.asarray(elevations, dtype=F32)]
    top = max(float(F32(beam_height_m(max_slant, e))) for e in elevations)
    ground = max(float(F32(beam_ground_range_m(max_slant, e))) for e in elevations)
    mean = circular_mean_deg(azimuths)
    resultant = math.hypot(sum(math.sin(math.radians(float(a))) for a in azimuths),
                           sum(math.cos(math.radians(float(a))) for a in azimuths)) / len(azimuths)
    return {
        "rays": len(elevations),
        "first_gate_m": jf(first_gate_m), "gate_spacing_m": jf(spacing_m), "gate_count": gates,
        "elevation_min_deg": jf(min(elevations)), "elevation_max_deg": jf(max(elevations)),
        "azimuth_spread_deg": jf(math.degrees(math.sqrt(2.0 * (1.0 - min(resultant, 1.0))))),
        "circular_mean_azimuth_deg": jf(mean),
        "coverage_top_m": jf(top),
        "coverage_range_m": jf(ground),
    }


def section_rhi():
    import netCDF4

    # The panels use the gate geometry of the FM301 model: the file's gate centres on a uniform
    # line from the first to the last centre (CfRadial `range`, DORADE CELV), in metres.

    # DOW8 CfRadial RHI (netCDF4).
    dow8_id = "cfrad1-dow8-20211011-223602-rhi-trim3-classic"
    dow8 = netCDF4.Dataset(str(corpus_path(dow8_id)))
    rng = np.asarray(dow8.variables["range"][:], dtype=np.float64)
    first = float(rng[0])
    spacing = (float(rng[-1]) - first) / (len(rng) - 1)
    elevation = np.asarray(dow8.variables["elevation"][:], dtype=F32)
    azimuth = np.asarray(dow8.variables["azimuth"][:], dtype=F32)
    dbz = np.ma.filled(dow8.variables["DBZHC"][:].astype(np.float64), np.nan)
    mode = b"".join(np.ma.filled(dow8.variables["sweep_mode"][0], b"")).decode().strip()
    dow8_case = {
        "id": dow8_id,
        "sweep_mode": mode,
        "range_first_m": jf(rng[0]),
        "fixed_angle_deg": jf(dow8.variables["fixed_angle"][0]),
        **rhi_geometry(elevation, azimuth, first, spacing, dbz.shape[1]),
        "valid_gates": int(np.isfinite(dbz).sum()),
        "panels": [
            rhi_panel(elevation, dbz, first, spacing, 768, 320, 15_000.0, 60_000.0,
                      np.arange(len(elevation)), seed=100),
            rhi_panel(elevation, dbz, first, spacing, 260, 60, 15_000.0, 130_000.0,
                      np.arange(len(elevation)), seed=200),
        ],
        "beam37_gate316_dbz": jf(dbz[37, 316]),
    }

    # DOW6 DORADE RHI (walker); transition rays (RYIB status != 0) are not part of the sweep.
    dow6_id = "dorade-dow6-20211230-222139-rhi-head41"
    dow6 = dorade_sweep(dow6_id, "DBZHC")
    kept = [(index, ray) for index, ray in enumerate(dow6["rays"]) if ray["status"] == 0]
    elevation6 = np.asarray([ray["elevation_deg"] for _, ray in kept], dtype=F32)
    azimuth6 = np.asarray([ray["azimuth_deg"] for _, ray in kept], dtype=F32)
    values6 = np.vstack([ray["values"] for _, ray in kept])
    first6 = dow6["first_cell_m"]
    spacing6 = dow6["uniform_spacing_m"]
    dow6_case = {
        "id": dow6_id,
        "radd_scan_mode": dow6["scan_mode"],
        "file_rays": len(dow6["rays"]),
        "transition_rays": len(dow6["rays"]) - len(kept),
        **rhi_geometry(elevation6, azimuth6, first6, spacing6, values6.shape[1]),
        "valid_gates": int(np.isfinite(values6).sum()),
        "panels": [
            rhi_panel(elevation6, values6, first6, spacing6, 400, 200, 26_000.0, 50_000.0,
                      [index for index, _ in kept], seed=300),
        ],
    }

    # PPI sweeps: NEXRAD Level II is always plan-position (Py-ART scan_type "ppi").
    import pyart

    ppi = []
    for entry in ("l2-ktlx-20240315-000217-trim", "l2-ktlx-20130520-201643-trim",
                  "l2-kewx-20160413-022531-trim"):
        radar = pyart.io.read_nexrad_archive(str(corpus_path(entry)))
        sweeps = level2_sweeps(entry)
        for index, s in enumerate(sweeps):
            geometry = rhi_geometry(s["el"], s["az"], 0, 250, 1)
            ppi.append({
                "id": entry, "sweep": index, "scan_type": radar.scan_type,
                "rays": geometry["rays"],
                "elevation_min_deg": geometry["elevation_min_deg"],
                "elevation_max_deg": geometry["elevation_max_deg"],
                "azimuth_first_deg": jf(s["az"][0]), "azimuth_last_deg": jf(s["az"][-1]),
                "azimuth_spread_deg": geometry["azimuth_spread_deg"],
                "circular_mean_azimuth_deg": geometry["circular_mean_azimuth_deg"],
                "arithmetic_mean_azimuth_deg": jf(np.mean(s["az"].astype(np.float64))),
            })

    # Every radial of a whole PPI volume in one list: more than 10 degrees of elevation but
    # azimuths all around the circle, so not an RHI.
    flat_id = "l2-ktlx-20130520-201643"
    flat = level2_sweeps(flat_id)
    flat_el = np.concatenate([s["el"] for s in flat])
    flat_az = np.concatenate([s["az"] for s in flat])
    flat_geometry = rhi_geometry(flat_el, flat_az, 0, 250, 1)
    flattened = {"id": flat_id, "sweeps": len(flat), "rays": flat_geometry["rays"],
                 "elevation_min_deg": flat_geometry["elevation_min_deg"],
                 "elevation_max_deg": flat_geometry["elevation_max_deg"],
                 "azimuth_spread_deg": flat_geometry["azimuth_spread_deg"]}

    payload = {
        "source": "tools/filters_map_golden.py rhi; netCDF4 1.7.4, DORADE block walker, MetPy 1.7.1"
                  " and Py-ART 2.2.5; 4/3-Earth panel reference",
        "note": "first_gate_m and gate_spacing_m are the FM301 model's uniform gate centres: CfRadial"
                " range[0] and DORADE CELV first cell, spacing (last - first) / (gates - 1)",
        "dow8": dow8_case,
        "dow6": dow6_case,
        "ppi": ppi,
        "flattened_volume": flattened,
    }
    write_golden("map/rhi.json", payload)


# ------------------------------------------------------------- volume products ---

ECHO_TOP_THRESHOLD_DBZ = F32(18.3)
VIL_HAIL_CAP_DBZ = F32(56.0)
XS_AE_M = 4.0 / 3.0 * 6_371_000.0
HALF_BEAMWIDTH_RAD = 0.475 * math.pi / 180.0


def volume_columns(sweeps, moment):
    """Every tilt carrying `moment`, sorted by tilt elevation (stable): the tilt elevation is
    the sweep's first radial elevation, per-gate beam-centre ground range and height from the
    gate centres first_gate_m + i * spacing, and azimuth -> row lookup tables."""
    cols = []
    for index, s in enumerate(sweeps):
        m = s["moments"].get(moment)
        if m is None:
            continue
        elevation = float(s["el"][0])
        slant = [float(m["first_gate_m"]) + g * float(m["gate_spacing_m"])
                 for g in range(m["gate_count"])]
        az = rem_euclid32(s["az"][m["rows"]])
        order = np.argsort(az, kind="stable")
        cols.append({
            "sweep": index, "elevation": elevation, "rows": m["rows"],
            "sorted_az": az[order], "sorted_rows": order, "row_az": az,
            "ground": np.asarray([beam_ground_range_m(r, elevation) for r in slant]),
            "height": np.asarray([beam_height_m(r, elevation) for r in slant]),
            "values": m["values"].astype(F32), "first_gate_m": m["first_gate_m"],
            "gate_spacing_m": m["gate_spacing_m"],
        })
    cols.sort(key=lambda col: F32(col["elevation"]))
    return cols


def nearest_row(col, az):
    """Nearest azimuth row (an exact azimuth match takes the later ray; ties go to the lower
    neighbour in sorted order)."""
    sorted_az = col["sorted_az"]
    az = F32(az)
    found, i = last_le_search(sorted_az, np.asarray([az]))
    i = int(i[0])
    if found[0]:
        return int(col["sorted_rows"][i])
    n = len(sorted_az)
    lo = n - 1 if i == 0 else i - 1
    hi = 0 if i >= n else i
    if ang_dist32(sorted_az[lo], az) <= ang_dist32(sorted_az[hi], az):
        return int(col["sorted_rows"][lo])
    return int(col["sorted_rows"][hi])


def gate_for_ground_range(col, s):
    """Nearest gate by beam-centre ground range; none beyond the last gate or more than half a
    gate short of the first (no smearing into the cone of silence). Vectorized over `s`."""
    g = col["ground"]
    n = len(g)
    s = np.atleast_1d(np.asarray(s, dtype=np.float64))
    half = 0.5 * (g[1] - g[0]) if n >= 2 else 0.0
    valid = ~((s > g[n - 1]) | (s < g[0] - half))
    found, i = last_le_search(g, s)
    i = np.clip(i, 0, n)
    idx = np.where(found, i, 0)
    inner = ~found & (i > 0) & (i < n)
    lo = np.clip(i - 1, 0, n - 1)
    hi = np.clip(i, 0, n - 1)
    idx = np.where(inner, np.where((g[hi] - s) < (s - g[lo]), hi, lo), idx)
    idx = np.where(~found & (i >= n), n - 1, idx)
    return valid, idx


def column_stack(sweeps):
    """Samples of every reflectivity tilt at the base tilt's azimuths and ground ranges:
    V[c, row, gate] (NaN = no sample) and H[c, gate] (+inf = no gate)."""
    cols = volume_columns(sweeps, "REF")
    base = min(cols, key=lambda col: (F32(col["elevation"]), col["sweep"]))
    rows = len(base["rows"])
    gates = base["values"].shape[1]
    s = base["ground"]
    base_az = base["row_az"]
    V = np.full((len(cols), rows, gates), np.nan, dtype=F32)
    H = np.full((len(cols), gates), np.inf)
    for c, col in enumerate(cols):
        valid, idx = gate_for_ground_range(col, s)
        H[c] = np.where(valid, col["height"][idx], np.inf)
        row_idx = np.asarray([nearest_row(col, az) for az in base_az])
        V[c] = np.where(valid[None, :], col["values"][row_idx[:, None], idx[None, :]], np.nan)
    return cols, base, V, H


def row_summary(grid):
    finite = np.isfinite(grid)
    return {"row_valid": [int(v) for v in finite.sum(axis=1)],
            "row_sum": jlist(np.where(finite, grid.astype(np.float64), 0.0).sum(axis=1), 4),
            "valid": int(finite.sum())}


def grid_max(grid, base):
    if not np.isfinite(grid).any():
        return None
    row, gate = np.unravel_index(np.nanargmax(np.where(np.isfinite(grid), grid, -np.inf)),
                                 grid.shape)
    return {"row": int(row), "gate": int(gate), "value": jf(grid[row, gate]),
            "azimuth_deg": jf(base["row_az"][row], 3),
            "ground_range_m": jf(base["ground"][gate], 1)}


def volume_products(sweeps, freezing_level_m=3200.0, minus20c_level_m=6400.0):
    cols, base, V, H = column_stack(sweeps)
    C, rows, gates = V.shape
    finite = np.isfinite(V)
    with np.errstate(invalid="ignore"):
        composite = np.where(finite.any(axis=0), np.nanmax(np.where(finite, V, -np.inf), axis=0),
                             np.nan).astype(F32)
        above = finite & (V >= ECHO_TOP_THRESHOLD_DBZ)
        top = np.where(above, np.broadcast_to(H[:, None, :], V.shape), -np.inf).max(axis=0)
    echo_top = np.where(np.isfinite(top), top.astype(F32), np.nan).astype(F32)

    # Profiles sorted by beam height per gate (stable: tilt order on equal heights).
    order = np.argsort(H, axis=0, kind="stable")
    Hs = np.take_along_axis(H, order, axis=0)
    Vs = np.take_along_axis(V, np.broadcast_to(order[:, None, :], V.shape), axis=0)
    h0 = max(freezing_level_m, 0.0)
    hm20 = max(minus20c_level_m, freezing_level_m + 1.0)
    vil = np.zeros((rows, gates))
    shi = np.zeros((rows, gates))
    count = np.zeros((rows, gates), dtype=np.int64)
    prev_h = np.zeros((rows, gates))
    prev_z = np.zeros((rows, gates))
    prev_e = np.zeros((rows, gates))
    for k in range(C):
        v = Vs[k].astype(np.float64)
        ok = np.isfinite(Vs[k])
        h = np.broadcast_to(Hs[k][None, :], (rows, gates))
        with np.errstate(invalid="ignore", over="ignore"):
            z_lin = np.power(10.0, np.minimum(Vs[k], VIL_HAIL_CAP_DBZ).astype(np.float64) / 10.0)
            w = np.clip((v - 40.0) / 10.0, 0.0, 1.0)
            ke = np.where(w <= 0.0, 0.0, 5.0e-6 * np.power(10.0, 0.084 * v) * w)
        first = ok & (count == 0)
        surface = first & (h > 0.0)
        vil = np.where(surface, vil + 3.44e-6 * np.power(z_lin, 4.0 / 7.0) * h, vil)
        later = ok & (count > 0)
        dh = np.maximum(h - prev_h, 0.0)
        with np.errstate(invalid="ignore"):
            vil = np.where(later, vil + 3.44e-6 * np.power(0.5 * (prev_z + z_lin), 4.0 / 7.0) * dh,
                           vil)
            mid_h = 0.5 * (prev_h + h)
            wt = np.clip((mid_h - h0) / (hm20 - h0), 0.0, 1.0)
            hail = later & ~((h <= h0) | (dh <= 0.0))
            shi = np.where(hail, shi + wt * (0.5 * (prev_e + ke)) * dh, shi)
        prev_h = np.where(ok, h, prev_h)
        prev_z = np.where(ok, z_lin, prev_z)
        prev_e = np.where(ok, ke, prev_e)
        count = count + ok
    vil_grid = np.where((count > 0) & (vil > 0.0), vil.astype(F32), np.nan).astype(F32)
    shi = shi * 0.1
    mehs = np.where((count >= 2) & (shi > 1.0), (2.54 * np.sqrt(shi)).astype(F32),
                    np.nan).astype(F32)
    with np.errstate(invalid="ignore", divide="ignore"):
        density = np.where(np.isfinite(vil_grid) & np.isfinite(echo_top) & (echo_top > F32(1500.0)),
                           (F32(1000.0) * vil_grid / echo_top).astype(F32), np.nan).astype(F32)

    base_c = next(c for c, col in enumerate(cols) if col is base)
    base_values = V[base_c]
    base_height = np.broadcast_to(H[base_c][None, :], (rows, gates))
    return {
        "cols": cols, "base": base, "V": V, "H": H,
        "composite": composite, "echo_top": echo_top, "vil": vil_grid, "mehs": mehs,
        "vil_density": density, "base_values": base_values,
        "composite_above_base": int((np.isfinite(base_values)
                                     & (composite > base_values)).sum()),
        "echo_top_above_base_beam": int((np.isfinite(echo_top)
                                         & (echo_top > base_height.astype(F32))).sum()),
    }


def interp_profile(profile, z, s, policy):
    first = profile[0]
    last = profile[-1]
    if z <= first["h"]:
        extend = max(first["r"] * HALF_BEAMWIDTH_RAD, 300.0)
        return first["v"] if first["h"] - z <= extend else None, "below"
    if z >= last["h"]:
        extend = last["r"] * HALF_BEAMWIDTH_RAD
        return last["v"] if z - last["h"] <= extend else None, "above"
    theta_i = invert_beam_xs(s, z)[1]
    for lo, hi in zip(profile, profile[1:]):
        if lo["h"] <= z <= hi["h"]:
            nearest = lo["v"] if (z - lo["h"]) <= (hi["h"] - z) else hi["v"]
            if policy == "cc" and min(lo["v"], hi["v"]) < F32(0.97):
                return nearest, "guarded"
            if policy == "velocity" and abs(F32(hi["v"] - lo["v"])) > F32(30.0):
                return nearest, "guarded"
            span = hi["theta"] - lo["theta"]
            if abs(span) < 1e-6:
                return lo["v"], "same_tilt"
            w2 = F32(min(max((theta_i - lo["theta"]) / span, 0.0), 1.0))
            return F32(lo["v"] + (hi["v"] - lo["v"]) * w2), "blend"
    return last["v"], "above"


def invert_beam_xs(s, h):
    sigma = s / XS_AE_M
    r = math.sqrt(max(XS_AE_M * XS_AE_M + (XS_AE_M + h) * (XS_AE_M + h)
                      - 2.0 * XS_AE_M * (XS_AE_M + h) * math.cos(sigma), 0.0))
    if r < 1.0:
        return 0.0, 90.0
    sin_theta = ((XS_AE_M + h) * (XS_AE_M + h) - XS_AE_M * XS_AE_M - r * r) / (2.0 * XS_AE_M * r)
    return r, math.degrees(math.asin(min(max(sin_theta, -1.0), 1.0)))


def cross_section(cols, start, end, width, height, top_m, policy):
    """MRMS-style reconstruction along the ground path. Returns the native and path-smoothed
    sections, pixel counts per interpolation case, and the (sweep, ray, grid row, gate) of every
    tilt sample taken."""
    native = np.full((height, width), np.nan, dtype=F32)
    kinds = {}
    used = []
    for x in range(width):
        f = F32(x) / F32(width - 1)
        east = F32(start[0]) + (F32(end[0]) - F32(start[0])) * f
        north = F32(start[1]) + (F32(end[1]) - F32(start[1])) * f
        s = float(np.hypot(F32(east), F32(north))) * 1000.0
        az = rem_euclid32(F32(np.arctan2(F32(east), F32(north))) * F32(57.2957795130823208768))[()]
        profile = []
        for col in cols:
            valid, idx = gate_for_ground_range(col, s)
            if not valid[0]:
                continue
            gate = int(idx[0])
            row = nearest_row(col, az)
            v = col["values"][row, gate]
            if not np.isfinite(v):
                continue
            h = float(col["height"][gate])
            profile.append({"h": h, "theta": float(F32(col["elevation"])),
                            "r": invert_beam_xs(s, h)[0], "v": F32(v)})
            used.append((col["sweep"], int(col["rows"][row]), row, gate))
        profile.sort(key=lambda p: p["h"])
        if not profile:
            continue
        for y in range(height):
            z = F32(top_m) * (F32(1.0) - F32(y) / F32(height - 1))
            value, kind = interp_profile(profile, float(z), s, policy)
            kinds[kind] = kinds.get(kind, 0) + (value is not None)
            if value is not None:
                native[y, x] = value
    filled = native.copy()
    for y in range(height):
        for x in range(width):
            if np.isfinite(native[y, x]):
                continue
            total = F32(0.0)
            n = F32(0.0)
            for dx in (-2, -1, 1, 2):
                xi = x + dx
                if 0 <= xi < width and np.isfinite(native[y, xi]):
                    total = F32(total + native[y, xi])
                    n = F32(n + F32(1.0))
            if n >= 2.0:
                filled[y, x] = F32(total / n)
    smoothed = filled.copy()
    for y in range(height):
        for x in range(width):
            if not np.isfinite(filled[y, x]):
                continue
            total = F32(0.0)
            n = F32(0.0)
            for dx in (-1, 0, 1):
                xi = x + dx
                if 0 <= xi < width and np.isfinite(filled[y, xi]):
                    total = F32(total + filled[y, xi])
                    n = F32(n + F32(1.0))
            if n > 0.0:
                smoothed[y, x] = F32(total / n)
    return native, smoothed, kinds, sorted(set(used))


def section_values(grid):
    return [jlist(row, 4) for row in grid]


def products_payload(entry, sweeps, products, extra=None):
    base = products["base"]
    payload = {
        "id": entry,
        "tilts": [{"sweep": col["sweep"], "elevation_deg": jf(F32(col["elevation"])),
                   "rays": int(len(col["rows"])), "gates": int(col["values"].shape[1]),
                   "first_gate_m": col["first_gate_m"], "gate_spacing_m": col["gate_spacing_m"]}
                  for col in products["cols"]],
        "base_sweep": base["sweep"],
        "rows": int(len(base["rows"])),
        "gates": int(base["values"].shape[1]),
        "composite_above_base": products["composite_above_base"],
        "echo_top_above_base_beam": products["echo_top_above_base_beam"],
    }
    for name in ("composite", "echo_top", "vil", "mehs", "vil_density"):
        payload[name] = {**row_summary(products[name]), "max": grid_max(products[name], base)}
    payload.update(extra or {})
    return payload


def section_volumetric():
    import pyart

    out = {"source": "tools/filters_map_golden.py volumetric; MetPy 1.7.1 Level2File, Py-ART 2.2.5"
                     " dealias_region_based; numpy column-walk reference",
           "freezing_level_m": 3200.0, "minus20c_level_m": 6400.0}

    # Hail storm: KEWX 2016-04-13 (San Antonio).
    entry = "l2-kewx-20160413-022531"
    sweeps = level2_sweeps(entry)
    products = volume_products(sweeps)
    volume_max = max(float(np.nanmax(s["moments"]["REF"]["values"])) for s in sweeps
                     if "REF" in s["moments"])
    out["hail"] = products_payload(entry, sweeps, products, {"volume_max_dbz": volume_max})

    # Clear air: KTLX 2024-05-15 VCP 35.
    entry = "l2-ktlx-20240515-000014"
    sweeps = level2_sweeps(entry)
    products = volume_products(sweeps)
    volume_max = max(float(np.nanmax(s["moments"]["REF"]["values"])) for s in sweeps
                     if "REF" in s["moments"])
    out["clear_air"] = products_payload(entry, sweeps, products, {"volume_max_dbz": volume_max})

    # Truncated legacy volume: one sweep of 68 reflectivity radials.
    entry = "l2-ktlx-19990503-230052"
    sweeps = level2_sweeps(entry)
    products = volume_products(sweeps)
    out["truncated"] = products_payload(entry, sweeps, products, {
        "sweeps": len(sweeps), "moments": sorted(sweeps[0]["moments"]),
        "reflectivity_valid": int(np.isfinite(sweeps[0]["moments"]["REF"]["values"]).sum())})

    # Moore tornado: KTLX 2013-05-20 cross-sections.
    entry = "l2-ktlx-20130520-201643"
    sweeps = level2_sweeps(entry)
    ref_cols = volume_columns(sweeps, "REF")
    vel_cols = volume_columns(sweeps, "VEL")
    sections = {"id": entry, "reflectivity": [], "velocity": []}
    # Through the Moore supercell W of the radar, and across the radar site (inside ~2 km of
    # ground range the low tilts' first gates are farther out than the high tilts').
    for start, end, width, height, top in (((-45.0, -0.8), (-5.0, -0.8), 160, 90, 18_000.0),
                                           ((-6.0, 0.3), (6.0, 0.3), 120, 60, 10_000.0)):
        native, smoothed, kinds, _ = cross_section(ref_cols, start, end, width, height, top,
                                                   "linear")
        sections["reflectivity"].append({
            "start_km": list(start), "end_km": list(end), "width": width, "height": height,
            "top_m": top, "kinds": kinds,
            "native": section_values(native), "smoothed": section_values(smoothed)})

    radar = pyart.io.read_nexrad_archive(str(corpus_path(entry)))
    dealiased = pyart.correct.dealias_region_based(radar)
    raw_velocity = np.ma.filled(radar.fields["velocity"]["data"].astype(np.float64), np.nan)
    unfolded = np.ma.filled(dealiased["data"].astype(np.float64), np.nan)
    # "couplet" crosses the Moore mesocyclone W of the radar (guarded pixels where bracketing
    # tilts differ by more than 30 m/s); "quiet_north" runs 10-30 km out along azimuth 340 deg,
    # where Py-ART's region-based dealiasing changes none of the sampled gates.
    for label, start, end, width, height, top in (
            ("couplet", (-30.0, -2.0), (-10.0, -2.0), 120, 80, 12_000.0),
            ("quiet_north", (-3.4, 9.4), (-10.3, 28.2), 120, 80, 12_000.0)):
        native, smoothed, kinds, used = cross_section(vel_cols, start, end, width, height, top,
                                                      "velocity")
        changes = 0
        for sweep, ray, _, gate in used:
            ray_index = int(radar.sweep_start_ray_index["data"][sweep]) + ray
            if raw_velocity[ray_index, gate] != unfolded[ray_index, gate]:
                changes += 1
        sections["velocity"].append({
            "label": label, "start_km": list(start), "end_km": list(end), "width": width,
            "height": height, "top_m": top, "kinds": kinds, "sampled_gates": len(used),
            "pyart_region_dealias_changed_gates": changes,
            "max_abs_velocity_mps": jf(max(abs(float(col["values"][row, gate]))
                                           for col in vel_cols for (sw, _, row, gate) in used
                                           if sw == col["sweep"]) if used else None),
            "native": section_values(native)})
    out["cross_sections"] = sections
    write_golden("map/volumetric.json", out)


SECTIONS = {
    "gate_filter": section_gate_filter,
    "smooth": section_smooth,
    "interpolate": section_interpolate,
    "rhi": section_rhi,
    "volumetric": section_volumetric,
}


def main(argv):
    names = argv or list(SECTIONS)
    for name in names:
        if name not in SECTIONS:
            raise SystemExit(f"unknown section {name}; one of {', '.join(SECTIONS)}")
    for name in names:
        print(f"== {name}")
        SECTIONS[name]()


if __name__ == "__main__":
    main(sys.argv[1:])
