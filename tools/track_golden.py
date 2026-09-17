#!/usr/bin/env python3
"""Golden values for the real-data tests of recast-radar-track.

The Rust tests decode real corpus files with the workspace readers and compare what
the storm-tracking products compute against the JSON files this script writes:

    testdata/golden/track/cells.json      crates/recast-radar-track/tests/cells_real.rs
    testdata/golden/track/swath.json      crates/recast-radar-track/tests/swath_real.rs
    testdata/golden/track/temporal.json   crates/recast-radar-track/tests/temporal_real.rs
    testdata/golden/track/tracking.json   crates/recast-radar-track/tests/tracking_real.rs
    testdata/golden/track/tracks.json     crates/recast-radar-track/tests/tracks_real.rs

Every input value comes from a reader that is independent of recast-radar-tools:

- NEXRAD Level II: Py-ART 2.2.5 ``pyart.io.read_nexrad_archive`` (fields, azimuths, ranges,
  ``pyart.retrieve.composite_reflectivity``, ``pyart.correct.dealias_region_based``) and
  MetPy 1.7.1 ``metpy.io.Level2File`` (per-sweep moment lists and ray elevations, volume
  times).
- NEXRAD Level III Storm Tracking Information (product 58): MetPy ``metpy.io.Level3File``
  (storm-id symbology packets and the tabular STORM ID / FCST MVT / DBZM HGT pages).
- DORADE: the block walker below (VOLD/SSWB/RADD/PARM/CELV/CSFD/CFAC/SWIB/RYIB/RDAT),
  written from the DORADE format description, not from the Rust reader.

The expected outputs are computed here with numpy and scipy from those inputs:

- storm cells: connected components (``scipy.ndimage.label``) and local maxima
  (``scipy.ndimage.maximum_filter``) of Py-ART's composite reflectivity, with true polar
  gate areas and Z^(4/7)-weighted centroids (Greene and Clark 1972 mass weighting);
- max-value swaths: per-gate maximum / signed extreme of two consecutive NOXP sector
  sweeps mapped onto the reference sweep by the documented 0.1-degree nearest-azimuth
  rule (float32 arithmetic where the Rust code uses f32);
- temporal grids: per-gate difference, trend, trapezoid accumulation, maximum, minimum,
  mean, exceedance duration and probability over co-registered NOXP sweeps;
- tracking: the SCIT storm cells of the Level III STI products for four consecutive KDVN
  volumes (ids, positions, DBZM, forecast movement), which the Rust test compares its own
  tracks against; volume times from MetPy for the time-gate case;
- rotation tracks and TDS: the strongest low-level cyclonic azimuthal shear on Py-ART's
  region-based-dealiased 0.5-degree velocity (a centred azimuthal derivative), 4/3-Earth
  beam height (Doviak and Zrnic 1993, eq. 2.28b) for the 0-2 km range bound, and the
  polarimetric debris criterion (RHOHV < 0.82 inside > 30 dBZ echo) counted on Py-ART's
  fields within 5 km of that circulation.

Test files are read from the committed corpus (testdata/files) and from the shared download
cache that recast-radar-testdata fills (%LOCALAPPDATA%\\recast-radar-tools\\testdata, or
$RECAST_RADAR_TESTDATA); every file is checked against its manifest sha256. Run
``cargo test -p recast-radar-track`` once to download the full volumes.

Usage:
    python tools/track_golden.py [cells|swath|temporal|tracking|tracks ...]

With no arguments every golden file is regenerated. The committed files were written with
Python 3.13, numpy 2.5.3, scipy 1.18.1, MetPy 1.7.1 and Py-ART 2.2.5.
"""

import gzip
import hashlib
import io
import json
import logging
import math
import os
import re
import struct
import sys
import tarfile
import tomllib
import warnings
from datetime import datetime, timezone
from pathlib import Path

import numpy as np

warnings.filterwarnings("ignore")
logging.getLogger("metpy.io.nexrad").setLevel(logging.ERROR)

ROOT = Path(__file__).resolve().parent.parent
TESTDATA = ROOT / "testdata"
GOLDEN = TESTDATA / "golden" / "track"

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


def write_golden(name, payload):
    path = GOLDEN / name
    path.parent.mkdir(parents=True, exist_ok=True)
    text = format_json(payload)
    json.loads(text)
    path.write_text(text + "\n", encoding="utf-8", newline="\n")
    print(f"wrote {path.relative_to(ROOT)} ({len(text)} bytes)")


def jf(value, digits=None):
    """JSON float: None for NaN; float32 values keep their shortest repr."""
    if value is None:
        return None
    if isinstance(value, np.floating):
        if not np.isfinite(value):
            return None
        if isinstance(value, np.float32) and digits is None:
            # Shortest decimal that round-trips to the same f32 (numpy's str()).
            return float(str(value))
        value = float(value)
    if isinstance(value, float) and not math.isfinite(value):
        return None
    if digits is not None:
        return round(float(value), digits)
    return float(value)


def sample_indices(count, limit, seed_offset=0):
    if count <= limit:
        return list(range(count))
    rng = np.random.default_rng(RNG_SEED + seed_offset)
    return sorted(int(i) for i in rng.choice(count, size=limit, replace=False))


# ------------------------------------------------------------- Level II (Py-ART) ---

def pyart_radar(entry_id):
    import pyart
    return pyart.io.read_nexrad_archive(str(corpus_path(entry_id)))


def pyart_composite(entry_id):
    """Py-ART's composite reflectivity of a Level II volume on the lowest sweep's
    geometry, azimuth-sorted: (azimuths deg, ranges m, dBZ with NaN for no data, radar)."""
    import pyart
    radar = pyart_radar(entry_id)
    comp = pyart.retrieve.composite_reflectivity(radar, field="reflectivity")
    z = np.ma.filled(comp.fields["composite_reflectivity"]["data"].astype(np.float64), np.nan)
    az = np.asarray(comp.azimuth["data"], dtype=np.float64)
    rng = np.asarray(comp.range["data"], dtype=np.float64)
    order = np.argsort(az)
    return az[order], rng, z[order], radar


def polar_enu(az_deg, rng_m):
    """East/north km of every gate of an azimuth-sorted polar grid."""
    r_km = rng_m[None, :] / 1000.0
    theta = np.deg2rad(az_deg)[:, None]
    return r_km * np.sin(theta), r_km * np.cos(theta)


def polar_gate_area_km2(az_deg, rng_m):
    """True polar gate area (range x azimuth step x gate spacing), km^2, per gate."""
    step_deg = 360.0 / len(az_deg)
    spacing = float(rng_m[1] - rng_m[0])
    return (rng_m[None, :] * np.deg2rad(step_deg) * spacing / 1.0e6) * np.ones((len(az_deg), 1))


def polar_components(z, az_deg, rng_m, threshold, min_area_km2, max_range_km=300.0):
    """8-connected components of gates >= threshold (azimuth wraps, range capped) with
    mass-weighted centroids (area x Z_lin^(4/7)) and true areas, strongest first."""
    from scipy import ndimage
    mask = np.nan_to_num(z, nan=-999.0) >= threshold
    mask &= (rng_m[None, :] / 1000.0) <= max_range_km
    rows = mask.shape[0]
    tiled = np.concatenate([mask, mask, mask], axis=0)
    labels, _ = ndimage.label(tiled, structure=np.ones((3, 3), dtype=int))
    labels = labels[rows:2 * rows]
    east, north = polar_enu(az_deg, rng_m)
    area = polar_gate_area_km2(az_deg, rng_m)
    out = []
    for label in np.unique(labels[labels > 0]):
        sel = labels == label
        cell_area = float(np.sum(area[sel]))
        if cell_area < min_area_km2:
            continue
        w = area[sel] * (10.0 ** (z[sel] / 10.0)) ** (4.0 / 7.0)
        out.append({
            "east_km": jf(np.sum(w * east[sel]) / np.sum(w), 3),
            "north_km": jf(np.sum(w * north[sel]) / np.sum(w), 3),
            "max_dbz": jf(np.max(z[sel]), 2),
            "area_km2": jf(cell_area, 2),
            "gates": int(np.sum(sel)),
        })
    out.sort(key=lambda c: -c["max_dbz"])
    return out


def cartesian_max_grid(z, az_deg, rng_m, half_km=300, cell_km=1.0):
    """Max-binned Cartesian image of a polar field (row 0 = north edge)."""
    east, north = polar_enu(az_deg, rng_m)
    size = int(round(2 * half_km / cell_km))
    col = np.floor((east + half_km) / cell_km).astype(int)
    row = np.floor((half_km - north) / cell_km).astype(int)
    valid = np.isfinite(z) & (col >= 0) & (col < size) & (row >= 0) & (row < size)
    grid = np.full(size * size, -np.inf)
    np.maximum.at(grid, row[valid] * size + col[valid], z[valid])
    grid[~np.isfinite(grid)] = np.nan
    return grid.reshape(size, size)


def nan_gaussian(a, sigma):
    from scipy import ndimage
    v = np.nan_to_num(a, nan=0.0)
    m = np.isfinite(a).astype(np.float64)
    vs = ndimage.gaussian_filter(v, sigma, truncate=2.0)
    ms = ndimage.gaussian_filter(m, sigma, truncate=2.0)
    out = vs / np.where(ms > 0, ms, np.nan)
    out[~np.isfinite(a)] = np.nan
    return out


# ------------------------------------------------------------- Level II (MetPy) ---

MSG31_NAMES = {b"REF": "REF", b"VEL": "VEL", b"SW ": "SW", b"SW": "SW", b"ZDR": "ZDR",
               b"PHI": "PHI", b"RHO": "RHO", b"CFP": "CFP"}


def metpy_file(entry_id):
    from metpy.io import Level2File
    raw = corpus_path(entry_id).read_bytes()
    if raw[:2] == bytes((0x1F, 0x8B)):
        raw = gzip.decompress(raw)
    return Level2File(io.BytesIO(raw))


def metpy_sweep_summary(entry_id):
    """Per sweep: first-ray elevation and the moment names its rays carry."""
    f = metpy_file(entry_id)
    sweeps = []
    for rays in f.sweeps:
        moments = []
        for ray in rays:
            if len(ray) == 5:
                names = [MSG31_NAMES.get(k, k.decode().strip()) for k in ray[4]]
            else:
                names = list(ray[1].keys())
            for name in names:
                if name not in moments:
                    moments.append(name)
        sweeps.append({"first_ray_elevation_deg": jf(F32(rays[0][0].el_angle)),
                       "rays": len(rays), "moments": moments})
    return f, sweeps


def expected_base_tilt(sweeps, moment):
    """Index of the lowest sweep (first-ray elevation, first on ties) carrying `moment`."""
    best = None
    for index, sweep in enumerate(sweeps):
        if moment not in sweep["moments"]:
            continue
        if best is None or sweep["first_ray_elevation_deg"] < sweeps[best]["first_ray_elevation_deg"]:
            best = index
    return best


def iso(dt):
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return dt.astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"


# ------------------------------------------------------------- DORADE walker ---

def dorade_walk(raw, fields):
    """One DORADE sweep file: SSWB start time, RYIB azimuth/elevation (CFAC-corrected in
    float32, then folded into [0, 360) like the Rust reader), the CSFD/CELV gate geometry
    and every requested field decoded from RDAT (16-bit words: value = stored / scale -
    bias, NaN at the PARM bad-data flag; every word of the block is kept, like the Rust
    reader, so a trailing word past the CSFD cell count becomes a no-data gate)."""
    endian = "<" if struct.unpack_from("<i", raw, 4)[0] < 65536 else ">"

    def i16(b, o):
        return struct.unpack_from(endian + "h", b, o)[0]

    def i32(b, o):
        return struct.unpack_from(endian + "i", b, o)[0]

    def f32(b, o):
        return struct.unpack_from(endian + "f", b, o)[0]

    out = {"rays": [], "params": {}, "start_unix": None}
    cfac = (0.0, 0.0, 0.0)
    offset = 0
    current = None
    while offset + 8 <= len(raw):
        name = raw[offset:offset + 4].decode("latin-1")
        length = i32(raw, offset + 4)
        if length < 8 or offset + length > len(raw):
            break
        block = raw[offset:offset + length]
        if name == "SSWB":
            start = i32(block, 12)
            out["start_unix"] = start if start > 0 else None
        elif name == "RADD":
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
        elif name == "CSFD":
            segments = max(0, min(8, i32(block, 8)))
            out["first_cell_m"] = f32(block, 12)
            out["cell_spacing_m"] = f32(block, 16)
            out["cell_count"] = sum(max(0, i16(block, 48 + 2 * s)) for s in range(segments))
        elif name == "CFAC":
            cfac = (f32(block, 8), f32(block, 12), f32(block, 16))
        elif name == "SWIB":
            out["fixed_angle_deg"] = f32(block, 32)
        elif name == "RYIB":
            az = np.remainder(F32(F32(f32(block, 24)) + F32(cfac[0])), F32(360.0))
            current = {"azimuth_deg": F32(az), "elevation_deg": F32(F32(f32(block, 28)) + F32(cfac[1])),
                       "data": {}}
            out["rays"].append(current)
        elif name == "RDAT" and current is not None:
            pname = block[8:16].decode("latin-1").strip("\x00 ")
            if pname in fields:
                current["data"][pname] = block[16:]
        offset += length
    if out.get("compression", 0) != 0:
        raise SystemExit("compressed DORADE fields are not handled by this walker")
    out["fields"] = {}
    for field in fields:
        param = out["params"][field]
        if param["format"] != 2:
            raise SystemExit(f"{field}: only 16-bit DORADE fields are handled")
        rows = []
        for ray in out["rays"]:
            words = np.frombuffer(ray["data"][field], dtype=endian + "i2").astype(np.int64)
            rows.append(words)
        words = np.stack(rows)
        # Mirror the Rust reader's u16 storage: raw = word + 32768, offset = bias + 32768,
        # value = (raw - offset) / scale in f32.
        raw_u16 = (words + 32768).astype(np.float32)
        values = (raw_u16 - F32(param["bias"] + 32768.0)) / F32(param["scale"])
        values = np.where(words == param["bad"], np.nan, values).astype(np.float32)
        out["fields"][field] = values
    out["gate_count"] = int(out["fields"][fields[0]].shape[1])
    for ray in out["rays"]:
        del ray["data"]
    return out


def tgz_sweeps(entry_id):
    """(basename, bytes) of every non-empty swp.* member of a Zenodo NOXP archive, sorted by
    name (which sorts by time); symlinked duplicates are skipped."""
    out = {}
    with tarfile.open(corpus_path(entry_id), "r:gz") as tf:
        for member in tf.getmembers():
            base = member.name.rsplit("/", 1)[-1]
            if member.isfile() and member.size > 0 and base.startswith("swp."):
                out[base] = tf.extractfile(member).read()
    return sorted(out.items())


NOXP_ARCHIVE = "dorade-noxp-20090525-sweeps-tgz"


def noxp_sweep(name, raw, fields=("DZ", "VR")):
    sweep = dorade_walk(raw, list(fields))
    sweep["name"] = name
    sweep["azimuths"] = np.asarray([r["azimuth_deg"] for r in sweep["rays"]], dtype=np.float32)
    return sweep


# ---------------------------------------------------------------- Level III STI ---

def sti_storms(entry_id):
    """SCIT storm cells of a Level III Storm Tracking Information product: id, current
    position (km east/north of the radar, from the storm-id symbology packet), maximum
    reflectivity and its height, and the forecast movement (deg from / kt) from the
    tabular page ("NEW" storms have none)."""
    from metpy.io import Level3File
    f = Level3File(str(corpus_path(entry_id)))
    storms = {}
    for pkt in f.sym_block[0]:
        if pkt.get("type") == "Storm ID":
            storms[pkt["id"]] = {"id": pkt["id"], "east_km": float(pkt["x"]), "north_km": float(pkt["y"]),
                                 "forecast_from_deg": None, "forecast_kt": None,
                                 "max_dbz": None, "max_dbz_height_kft": None}
    for page in f.graph_pages:
        rows = {}
        for item in page:
            if "text" in item:
                rows[item["text"][:9].strip()] = item["text"]
        columns = lambda key: [rows.get(key, "")[10 + 10 * i:20 + 10 * i] for i in range(6)]
        for sid, mvt, dbz in zip(columns("STORM ID"), columns("FCST MVT"), columns("DBZM HGT")):
            sid = sid.strip()
            if sid not in storms:
                continue
            m = re.match(r"\s*(\d+)/\s*(\d+)", mvt)
            storms[sid]["forecast_from_deg"] = int(m.group(1)) if m else None
            storms[sid]["forecast_kt"] = int(m.group(2)) if m else None
            m = re.match(r"\s*(\d+)\s+([\d.]+)", dbz)
            storms[sid]["max_dbz"] = int(m.group(1)) if m else None
            storms[sid]["max_dbz_height_kft"] = float(m.group(2)) if m else None
    return f.metadata["vol_time"], [storms[k] for k in sorted(storms)]


# ------------------------------------------------------------------ geometry ---

EARTH_RADIUS_M = 6_371_000.0
EFFECTIVE_RADIUS_M = 4.0 / 3.0 * EARTH_RADIUS_M


def beam_height_m(slant_m, elevation_deg):
    """Beam-centre height above the radar, 4/3-Earth model (Doviak and Zrnic 1993, 2.28b)."""
    el = math.radians(elevation_deg)
    return math.sqrt(slant_m ** 2 + EFFECTIVE_RADIUS_M ** 2 + 2.0 * slant_m * EFFECTIVE_RADIUS_M * math.sin(el)) - EFFECTIVE_RADIUS_M


# ---------------------------------------------------------------------- cells ---

def section_cells():
    payload = {}

    # KEWX hailstorm: every salient 60 dBZ core of Py-ART's composite.
    az, rng, z, radar = pyart_composite("l2-kewx-20160413-022531")
    cores = polar_components(z, az, rng, 60.0, 20.0)
    payload["kewx"] = {
        "entry": "l2-kewx-20160413-022531",
        "composite_max_dbz": jf(np.nanmax(z), 2),
        "core_threshold_dbz": 60.0,
        "min_core_area_km2": 20.0,
        "cores": cores,
    }

    # KDVN derecho: the 40 dBZ line envelope and its distinct 60 dBZ cores.
    az, rng, z, radar = pyart_composite("l2-kdvn-20200810-180401")
    from scipy import ndimage
    grid = nan_gaussian(cartesian_max_grid(z, az, rng, 300, 1.0), 1.5)
    size = grid.shape[0]
    filled = np.nan_to_num(grid, nan=-999.0)
    peaks_mask = (filled == ndimage.maximum_filter(filled, size=21)) & (filled >= 60.0)
    labels, _ = ndimage.label(filled >= 40.0, structure=np.ones((3, 3), dtype=int))
    peak_rows, peak_cols = np.where(peaks_mask)
    peak_labels = labels[peak_rows, peak_cols]
    best_label = max(set(peak_labels[peak_labels > 0]), key=lambda l: np.sum(peak_labels == l))
    candidates = sorted(
        ((float(filled[r, c]), c + 0.5 - 300.0, 300.0 - (r + 0.5)) for r, c, l in
         zip(peak_rows, peak_cols, peak_labels) if l == best_label),
        reverse=True)
    peaks = []
    for dbz, east, north in candidates:
        if all(math.hypot(east - p["east_km"], north - p["north_km"]) >= 15.0 for p in peaks):
            peaks.append({"east_km": jf(east, 1), "north_km": jf(north, 1), "smoothed_dbz": jf(dbz, 2)})
    if len(peaks) < 3:
        raise SystemExit("KDVN: expected at least three distinct line cores")
    envelope = labels == best_label
    rows_e, cols_e = np.where(envelope)
    payload["kdvn"] = {
        "entry": "l2-kdvn-20200810-180401",
        "composite_max_dbz": jf(np.nanmax(z), 2),
        "cartesian_cell_km": 1.0,
        "smoothing_sigma_km": 1.5,
        "envelope_threshold_dbz": 40.0,
        "peak_threshold_dbz": 60.0,
        "peak_window_km": 21,
        "min_peak_separation_km": 15.0,
        "envelope_area_km2": int(np.sum(envelope)),
        "envelope_east_km": [jf(cols_e.min() - 300.0, 1), jf(cols_e.max() + 1 - 300.0, 1)],
        "envelope_north_km": [jf(300.0 - (rows_e.max() + 1), 1), jf(300.0 - rows_e.min(), 1)],
        "line_peaks": peaks,
    }

    # Clear air: the largest 30 dBZ patch of Py-ART's composite.
    az, rng, z, radar = pyart_composite("l2-ktlx-20240515-000014")
    patches = polar_components(z, az, rng, 30.0, 0.0)
    largest = max((p["area_km2"] for p in patches), default=0.0)
    payload["clear_air"] = {
        "entry": "l2-ktlx-20240515-000014",
        "composite_max_dbz": jf(np.nanmax(z), 2),
        "patches_30dbz": len(patches),
        "largest_30dbz_patch_km2": jf(largest, 2),
    }

    # Status-only archive object: MetPy reads no sweeps.
    f = metpy_file("l2-tbwi-20230601-175101-stub")
    payload["stub"] = {"entry": "l2-tbwi-20230601-175101-stub", "sweeps": len(f.sweeps),
                       "volume_time": iso(f.dt)}
    write_golden("cells.json", payload)


# ---------------------------------------------------------------------- swath ---

AZ_SLOTS = 3600
SLOT_DEG = F32(F32(360.0) / F32(AZ_SLOTS))


def rust_round_f32(x):
    """f32::round (half away from zero) for non-negative values."""
    return np.floor(x + F32(0.5)).astype(np.float32)


def slot_of(azimuth_f32):
    slot = rust_round_f32(F32(azimuth_f32) / SLOT_DEG)
    return int(slot) % AZ_SLOTS


def azimuth_delta_f32(a, b):
    diff = np.remainder(np.abs(F32(a) - F32(b)), F32(360.0)).astype(np.float32)
    return np.minimum(diff, F32(360.0) - diff)


def slot_to_row_table(target_azimuths):
    table = np.zeros(AZ_SLOTS, dtype=int)
    for slot in range(AZ_SLOTS):
        slot_az = F32(F32(slot) * SLOT_DEG)
        deltas = azimuth_delta_f32(slot_az, target_azimuths)
        table[slot] = int(np.argmin(deltas))  # first minimum on ties
    return table


def swath_reference(frames, field, aggregation):
    """Per-gate swath of `frames` (walked sweeps, in loop order) for `field`, following the
    documented construction: reference geometry = the frame with the most rays (last on
    ties), every source ray mapped to the nearest reference row through 0.1-degree slots,
    gates aligned by index (identical range layout), NaN where no frame has data."""
    reference = frames[-1]
    for frame in frames:
        if len(frame["rays"]) > len(reference["rays"]):
            reference = frame
    target_az = reference["azimuths"]
    table = slot_to_row_table(target_az)
    rows = len(target_az)
    gates = reference["gate_count"]
    out = np.full((rows, gates), np.nan, dtype=np.float32)
    contributions = []
    for frame in frames:
        contribution = np.full((rows, gates), np.nan, dtype=np.float32)
        for row, az in enumerate(frame["azimuths"]):
            target_row = table[slot_of(az)]
            values = frame["fields"][field][row]
            existing = out[target_row]
            own = contribution[target_row]
            candidate_ok = np.isfinite(values)
            if aggregation == "max":
                take = candidate_ok & (~np.isfinite(existing) | (values > existing))
                take_own = candidate_ok & (~np.isfinite(own) | (values > own))
            else:
                take = candidate_ok & (~np.isfinite(existing) | (np.abs(values) > np.abs(existing)))
                take_own = candidate_ok & (~np.isfinite(own) | (np.abs(values) > np.abs(own)))
            existing[take] = values[take]
            own[take_own] = values[take_own]
        contributions.append(contribution)
    return reference, out, contributions


def grid_summary(values):
    finite = np.isfinite(values)
    return {"finite": int(np.sum(finite)), "sum": jf(np.sum(values[finite].astype(np.float64)), 4),
            "max": jf(np.max(values[finite]), 4) if finite.any() else None,
            "min": jf(np.min(values[finite]), 4) if finite.any() else None}


def cell_samples(values, mask, limit, seed_offset):
    rows, cols = np.where(mask)
    picks = sample_indices(len(rows), limit, seed_offset)
    return [[int(rows[i]), int(cols[i]), jf(values[rows[i], cols[i]])] for i in picks]


def section_swath():
    sweeps = dict(tgz_sweeps(NOXP_ARCHIVE))
    names = ["swp.1090525203529.NOXPRVP.0.0.5_PPI_v1", "swp.1090525203659.NOXPRVP.0.0.5_PPI_v1"]
    frames = [noxp_sweep(name, sweeps[name]) for name in names]
    reference, ref_max, parts = swath_reference(frames, "DZ", "max")
    only_first = np.isfinite(parts[0]) & ~np.isfinite(parts[1])
    only_second = np.isfinite(parts[1]) & ~np.isfinite(parts[0])
    first_wins = np.isfinite(parts[0]) & np.isfinite(parts[1]) & (parts[0] > parts[1])
    reflectivity = grid_summary(ref_max)
    reflectivity.update({
        "samples": cell_samples(ref_max, np.isfinite(ref_max), 48, 1),
        "only_first_frame": cell_samples(ref_max, only_first, 24, 2),
        "only_second_frame": cell_samples(ref_max, only_second, 24, 3),
        "first_frame_larger": cell_samples(ref_max, first_wins, 24, 4),
        "only_first_count": int(np.sum(only_first)),
        "only_second_count": int(np.sum(only_second)),
        "first_frame_larger_count": int(np.sum(first_wins)),
        "union_count": int(np.sum(np.isfinite(parts[0]) | np.isfinite(parts[1]))),
    })
    _, ref_mag, vparts = swath_reference(frames, "VR", "magnitude")
    both = np.isfinite(vparts[0]) & np.isfinite(vparts[1])
    sign_flip = both & (np.sign(vparts[0]) != np.sign(vparts[1])) & (np.abs(ref_mag) >= 3.0)
    velocity = grid_summary(ref_mag)
    velocity.update({
        "samples": cell_samples(ref_mag, np.isfinite(ref_mag), 48, 5),
        "sign_flips": cell_samples(ref_mag, sign_flip, 24, 6),
        "sign_flip_count": int(np.sum(sign_flip)),
        "negative_count": int(np.sum(np.isfinite(ref_mag) & (ref_mag < 0))),
    })
    payload = {
        "noxp": {
            "archive": NOXP_ARCHIVE,
            "members": names,
            "start_unix": [f["start_unix"] for f in frames],
            "rays": [len(f["rays"]) for f in frames],
            "gate_count": reference["gate_count"],
            "first_gate_m": int(round(reference["first_cell_m"])),
            "gate_spacing_m": int(round(reference["cell_spacing_m"])),
            "reference_member": reference["name"],
            "reference_azimuths_deg": [jf(a) for a in reference["azimuths"]],
            "valid_gates": {"DZ": [int(np.sum(np.isfinite(f["fields"]["DZ"]))) for f in frames],
                            "VR": [int(np.sum(np.isfinite(f["fields"]["VR"]))) for f in frames]},
            "reflectivity_max": reflectivity,
            "velocity_max_magnitude": velocity,
        }
    }
    for key, entry in (("tstl", "l2-tstl-20230331-230314-trim"), ("ktlx", "l2-ktlx-20240315-000217-trim")):
        _, summary = metpy_sweep_summary(entry)
        payload[key] = {
            "entry": entry,
            "sweeps": summary,
            "base_tilt": {m: expected_base_tilt(summary, m) for m in ("REF", "VEL", "SW", "ZDR", "RHO", "PHI")},
        }
    write_golden("swath.json", payload)


# ------------------------------------------------------------------- temporal ---

def section_temporal():
    sweeps = tgz_sweeps(NOXP_ARCHIVE)
    frames = [noxp_sweep(name, raw, ("DZ",)) for name, raw in sweeps]
    rays = 171
    frames = [f for f in frames if len(f["rays"]) == rays]
    if len(frames) != 11:
        raise SystemExit(f"expected 11 NOXP sweeps with {rays} rays, found {len(frames)}")
    times = [f["start_unix"] for f in frames]
    dz = [f["fields"]["DZ"] for f in frames]
    older, newer = dz[0], dz[1]
    both = np.isfinite(older) & np.isfinite(newer)
    difference = np.where(both, (newer - older).astype(np.float32), np.nan).astype(np.float32)
    elapsed = times[1] - times[0]
    hours = F32(elapsed / 3600.0)
    trend = np.where(both, ((newer - older).astype(np.float32) / hours).astype(np.float32), np.nan).astype(np.float32)

    # Trapezoid accumulation over the first four frames (rates clamped at zero).
    acc_frames = dz[:4]
    accumulated = np.zeros_like(older, dtype=np.float32)
    seen = np.zeros(older.shape, dtype=bool)
    for k in range(3):
        left, right = acc_frames[k], acc_frames[k + 1]
        h = F32((times[k + 1] - times[k]) / 3600.0)
        ok = np.isfinite(left) & np.isfinite(right)
        inc = (F32(0.5) * (np.maximum(left, F32(0.0)) + np.maximum(right, F32(0.0)))).astype(np.float32) * h
        accumulated = np.where(ok, (accumulated + inc.astype(np.float32)).astype(np.float32), accumulated)
        seen |= ok
    accumulated = np.where(seen, accumulated, np.nan).astype(np.float32)

    # Exceedance duration (minutes above 40 dBZ, half credit for one-sided windows) over
    # the same four frames.
    threshold = F32(40.0)
    minutes = np.zeros_like(older, dtype=np.float32)
    seen_d = np.zeros(older.shape, dtype=bool)
    for k in range(3):
        left, right = acc_frames[k], acc_frames[k + 1]
        m = F32((times[k + 1] - times[k]) / 60.0)
        ok = np.isfinite(left) & np.isfinite(right)
        occupancy = np.where((left >= threshold) & (right >= threshold), F32(1.0),
                             np.where((left >= threshold) | (right >= threshold), F32(0.5), F32(0.0))).astype(np.float32)
        minutes = np.where(ok, (minutes + occupancy * m).astype(np.float32), minutes)
        seen_d |= ok
    minutes = np.where(seen_d, minutes, np.nan).astype(np.float32)

    # Probability, maximum, minimum and mean over all eleven frames.
    stack = np.stack(dz)
    finite = np.isfinite(stack)
    valid = np.sum(finite, axis=0)
    exceeded = np.sum(finite & (stack >= threshold), axis=0)
    probability = np.where(valid > 0, (F32(100.0) * exceeded.astype(np.float32)) / valid.astype(np.float32), np.nan).astype(np.float32)
    maximum = np.where(valid > 0, np.nanmax(np.where(finite, stack, -np.inf), axis=0), np.nan).astype(np.float32)
    minimum = np.where(valid > 0, np.nanmin(np.where(finite, stack, np.inf), axis=0), np.nan).astype(np.float32)
    running = np.zeros(older.shape, dtype=np.float32)
    for layer in dz:  # sequential f32 sum in frame order, like the Rust fold
        running = (running + np.where(np.isfinite(layer), layer, F32(0.0))).astype(np.float32)
    mean = np.where(valid > 0, running / valid.astype(np.float32), np.nan).astype(np.float32)

    def summary(values, seed, extra=None):
        out = grid_summary(values)
        out["samples"] = cell_samples(values, np.isfinite(values), 48, seed)
        if extra:
            out.update(extra)
        return out

    partial = (valid > 0) & (valid < len(frames)) & (exceeded > 0)
    payload = {
        "noxp": {
            "archive": NOXP_ARCHIVE,
            "members": [f["name"] for f in frames],
            "start_unix": times,
            "rays": rays,
            "gate_count": frames[0]["gate_count"],
            "first_gate_m": int(round(frames[0]["first_cell_m"])),
            "gate_spacing_m": int(round(frames[0]["cell_spacing_m"])),
            "valid_gates": [int(np.sum(np.isfinite(d))) for d in dz],
            "difference": summary(difference, 11, {"newer": 1, "older": 0}),
            "trend": summary(trend, 12, {"elapsed_s": elapsed}),
            "accumulation": summary(accumulated, 13, {"frames": 4}),
            "duration": summary(minutes, 14, {"frames": 4, "threshold_dbz": 40.0}),
            "probability": summary(probability, 15, {
                "threshold_dbz": 40.0,
                "count_100": int(np.sum(probability == 100.0)),
                "count_0": int(np.sum(probability == 0.0)),
                "partial_samples": [s + [int(valid[s[0], s[1]]), int(exceeded[s[0], s[1]])]
                                    for s in cell_samples(probability, partial, 24, 16)],
                "partial_count": int(np.sum(partial)),
            }),
            "maximum": summary(maximum, 17),
            "minimum": summary(minimum, 18),
            "mean": summary(mean, 19),
        }
    }
    write_golden("temporal.json", payload)


# ------------------------------------------------------------------- tracking ---

KDVN_SEQUENCE = [
    ("l2-kdvn-20200810-175718", "l3-kdvn-20200810-1757-nst"),
    ("l2-kdvn-20200810-180401", "l3-kdvn-20200810-1804-nst"),
    ("l2-kdvn-20200810-181043", "l3-kdvn-20200810-1810-nst"),
    ("l2-kdvn-20200810-181724", "l3-kdvn-20200810-1817-nst"),
]


def section_tracking():
    volumes = []
    previous = None
    for l2, l3 in KDVN_SEQUENCE:
        vol_time, storms = sti_storms(l3)
        f = metpy_file(l2)
        for storm in storms:
            storm["range_km"] = jf(math.hypot(storm["east_km"], storm["north_km"]), 2)
            if previous is not None:
                nearest = min(math.hypot(storm["east_km"] - p["east_km"], storm["north_km"] - p["north_km"])
                              for p in previous)
                storm["nearest_previous_km"] = jf(nearest, 2)
                storm["new"] = storm["id"] not in {p["id"] for p in previous}
        volumes.append({
            "level2": l2,
            "sti": l3,
            "sti_volume_time": iso(vol_time),
            "level2_volume_time": iso(f.dt),
            "storms": storms,
        })
        previous = storms
    ids = [set(s["id"] for s in v["storms"]) for v in volumes]
    persistent = sorted(set.intersection(*ids))
    payload = {
        "kdvn": {
            "volumes": volumes,
            "persistent_ids": persistent,
        },
        "time_gate": {
            "first": {"entry": "l2-ktlx-20130520-201643", "volume_time": iso(metpy_file("l2-ktlx-20130520-201643").dt)},
            "second": {"entry": "l2-ktlx-20240315-000217", "volume_time": iso(metpy_file("l2-ktlx-20240315-000217").dt)},
        },
    }
    write_golden("tracking.json", payload)


# --------------------------------------------------------------------- tracks ---

def section_tracks():
    import pyart
    entry = "l2-ktlx-20130520-201643"
    radar = pyart_radar(entry)
    # Lowest Doppler sweep (the split-cut velocity tilt) and lowest dual-pol sweep.
    vel = radar.fields["velocity"]["data"]
    ref = radar.fields["reflectivity"]["data"]
    rho = radar.fields["cross_correlation_ratio"]["data"]
    sweep_el = [float(radar.fixed_angle["data"][s]) for s in range(radar.nsweeps)]
    doppler = min((s for s in range(radar.nsweeps) if not np.all(np.ma.getmaskarray(vel[radar.get_slice(s)]))),
                  key=lambda s: sweep_el[s])
    dualpol = min((s for s in range(radar.nsweeps) if not np.all(np.ma.getmaskarray(rho[radar.get_slice(s)]))),
                  key=lambda s: sweep_el[s])
    dealiased = pyart.correct.dealias_region_based(radar, vel_field="velocity", keep_original=False)
    v = np.ma.filled(dealiased["data"].astype(np.float64), np.nan)
    rng = np.asarray(radar.range["data"], dtype=np.float64)
    r_km = rng / 1000.0

    def sweep_fields(s):
        """Azimuth-sorted velocity, reflectivity, centred azimuthal shear (1e-3 s^-1,
        cyclonic positive, both neighbours valid, rows wrap) and ENU of one sweep."""
        sl = radar.get_slice(s)
        az = np.asarray(radar.azimuth["data"][sl], dtype=np.float64)
        order = np.argsort(az)
        az = az[order]
        vs = v[sl][order]
        zs = np.ma.filled(ref[sl].astype(np.float64), np.nan)[order]
        up = np.roll(vs, -1, axis=0)
        down = np.roll(vs, 1, axis=0)
        step = np.deg2rad(np.abs((np.roll(az, -1) - np.roll(az, 1) + 180.0) % 360.0 - 180.0))[:, None]
        shear = (up - down) / (rng[None, :] * step) * 1000.0
        shear[~(np.isfinite(up) & np.isfinite(down))] = np.nan
        east, north = polar_enu(az, rng)
        return {"az": az, "v": vs, "z": zs, "up": up, "down": down, "shear": shear,
                "east": east, "north": north, "el": float(np.mean(radar.elevation["data"][sl]))}

    # The low-level composite draws on the lowest velocity sweeps at or below 2 deg
    # (at most three: 0.5, 0.9 and 1.3 deg here).
    low = sorted((s for s in range(radar.nsweeps) if sweep_el[s] <= 2.0
                  and not np.all(np.ma.getmaskarray(vel[radar.get_slice(s)]))), key=lambda s: sweep_el[s])[:3]
    low_fields = {s: sweep_fields(s) for s in low}
    base = low_fields[doppler]
    az, zs, shear, up, down, east, north, el = (base["az"], base["z"], base["shear"], base["up"],
                                                 base["down"], base["east"], base["north"], base["el"])
    # Strongest cyclonic shear inside 5-60 km, in >= 30 dBZ echo, on the lowest sweep.
    window = (r_km[None, :] >= 5.0) & (r_km[None, :] <= 60.0) & (np.nan_to_num(zs, nan=-99) >= 30.0)
    candidate = np.where(window, shear, np.nan)
    peak = np.unravel_index(np.nanargmax(candidate), candidate.shape)
    peak_e, peak_n = float(east[peak]), float(north[peak])
    peak_shear = float(candidate[peak])
    # Range where the lowest Doppler beam leaves the 0-2 km layer.
    bound_km = max(r for r in np.arange(5.0, 300.0, 0.25) if beam_height_m(r * 1000.0, el) <= 2000.0)
    # A strong echo beyond the bound and one inside it (>= 40 dBZ on the Doppler sweep).
    strong = np.nan_to_num(zs, nan=-99) >= 40.0
    beyond = np.where(strong & (r_km[None, :] >= bound_km + 15.0) & (r_km[None, :] <= 200.0))
    inside = np.where(strong & (r_km[None, :] >= bound_km - 25.0) & (r_km[None, :] <= bound_km - 10.0)
                      & np.isfinite(shear))
    pick_beyond = np.argmax(zs[beyond])
    pick_inside = np.argmax(zs[inside])
    # A location (40-100 km) with no echo above 5 dBZ within 3 km on ANY low-level
    # sweep, and one inside >= 20 dBZ echo on every low-level sweep with negligible
    # azimuthal shear (|shear| < 2.5e-3 s^-1 within 1.5 km on each of them).
    from scipy import ndimage
    cart = None
    cart_v = None
    for fields in low_fields.values():
        layer = cartesian_max_grid(fields["z"], fields["az"], rng, 150, 1.0)
        cart = layer if cart is None else np.fmax(cart, layer)
        present = np.where(np.isfinite(fields["v"]), 1.0, np.nan)
        layer_v = cartesian_max_grid(present, fields["az"], rng, 150, 1.0)
        cart_v = layer_v if cart_v is None else np.fmax(cart_v, layer_v)
    echo_any = ndimage.maximum_filter(np.nan_to_num(cart, nan=-99.0), size=7)
    velocity_any = ndimage.maximum_filter(np.nan_to_num(cart_v, nan=0.0), size=7)
    dist_grid = np.hypot(np.arange(300)[None, :] + 0.5 - 150.0, 150.0 - (np.arange(300)[:, None] + 0.5))
    ring = (dist_grid >= 40.0) & (dist_grid <= 100.0)
    # No echo and no velocity at all within 3 km: the frame has no data there.
    rr, cc = np.where(ring & (echo_any < 5.0) & (velocity_any < 1.0))
    k = np.argmin(np.abs(dist_grid[rr, cc] - 60.0))
    no_data = (float(cc[k] + 0.5 - 150.0), float(150.0 - (rr[k] + 0.5)))
    # Velocity present but no echo reaching 20 dBZ within 3 km: whatever shear the
    # velocity carries, the reflectivity floor keeps it off the display.
    rr, cc = np.where(ring & (echo_any < 15.0) & (velocity_any >= 1.0))
    k = np.argmin(np.abs(dist_grid[rr, cc] - 60.0))
    no_echo = (float(cc[k] + 0.5 - 150.0), float(150.0 - (rr[k] + 0.5)))
    calm = np.where((np.nan_to_num(zs, nan=-99) >= 30.0) & (r_km[None, :] >= 20.0) & (r_km[None, :] <= 80.0)
                    & np.isfinite(shear) & (np.abs(shear) < 0.5))
    calm_pick = None
    for i in sample_indices(len(calm[0]), 2000, 21):
        r0, g0 = calm[0][i], calm[1][i]
        e0, n0 = east[r0, g0], north[r0, g0]
        worst = 0.0
        for fields in low_fields.values():
            near = (np.abs(fields["east"] - e0) < 1.5) & (np.abs(fields["north"] - n0) < 1.5)
            if not (np.all(np.nan_to_num(fields["z"][near], nan=-99) >= 20.0)
                    and np.all(np.abs(np.nan_to_num(fields["shear"][near], nan=0.0)) < 2.5)):
                worst = None
                break
            worst = max(worst, float(np.max(np.abs(np.nan_to_num(fields["shear"][near], nan=0.0)))))
        if worst is not None:
            calm_pick = (float(e0), float(n0), worst)
            break
    if calm_pick is None:
        raise SystemExit("no calm in-echo location found")
    # Debris gates on the lowest dual-pol sweep within 5 km of the circulation.
    sd = radar.get_slice(dualpol)
    az_d = np.asarray(radar.azimuth["data"][sd], dtype=np.float64)
    rho_d = np.ma.filled(rho[sd].astype(np.float64), np.nan)
    ref_d = np.ma.filled(ref[sd].astype(np.float64), np.nan)
    el_d = float(np.mean(radar.elevation["data"][sd]))
    east_d, north_d = polar_enu(az_d, rng)
    within = np.hypot(east_d - peak_e, north_d - peak_n) <= 5.0
    within &= (r_km[None, :] >= 5.0)
    heights = np.array([beam_height_m(r, el_d) for r in rng])
    within &= (heights[None, :] <= 3000.0)
    debris = within & (np.nan_to_num(rho_d, nan=9.0) < 0.82) & (np.nan_to_num(ref_d, nan=-99.0) > 30.0)
    dr, dg = np.where(debris)
    debris_gates = [[jf(east_d[r, g], 3), jf(north_d[r, g], 3), jf(rho_d[r, g], 4), jf(ref_d[r, g], 2)]
                    for r, g in zip(dr, dg)]
    payload = {
        "moore": {
            "entry": entry,
            "doppler_sweep": int(doppler),
            "doppler_elevation_deg": jf(el, 3),
            "low_level_sweeps": [int(s) for s in low],
            "low_level_elevations_deg": [jf(low_fields[s]["el"], 3) for s in low],
            "dualpol_sweep": int(dualpol),
            "dualpol_elevation_deg": jf(el_d, 3),
            "nyquist_mps": jf(float(radar.instrument_parameters["nyquist_velocity"]["data"][radar.get_slice(doppler)][0]), 2),
            "circulation": {
                "east_km": jf(peak_e, 2), "north_km": jf(peak_n, 2),
                "azimuth_deg": jf(az[peak[0]], 2), "range_km": jf(r_km[peak[1]], 3),
                "shear_e3": jf(peak_shear, 3),
                "dealiased_up_mps": jf(up[peak], 2), "dealiased_down_mps": jf(down[peak], 2),
            },
            "low_level_top_m": 2000.0,
            "beam_top_range_km": jf(bound_km, 2),
            "beyond_bound": {"east_km": jf(east[beyond][pick_beyond], 2), "north_km": jf(north[beyond][pick_beyond], 2),
                             "range_km": jf(r_km[beyond[1][pick_beyond]], 2), "dbz": jf(zs[beyond][pick_beyond], 1)},
            "inside_bound": {"east_km": jf(east[inside][pick_inside], 2), "north_km": jf(north[inside][pick_inside], 2),
                             "range_km": jf(r_km[inside[1][pick_inside]], 2), "dbz": jf(zs[inside][pick_inside], 1)},
            "no_data": {"east_km": jf(no_data[0], 1), "north_km": jf(no_data[1], 1)},
            "no_echo": {"east_km": jf(no_echo[0], 1), "north_km": jf(no_echo[1], 1)},
            "calm_echo": {"east_km": jf(calm_pick[0], 2), "north_km": jf(calm_pick[1], 2),
                          "max_abs_shear_e3_within_1_5_km": jf(calm_pick[2], 3)},
            "tds": {
                "radius_km": 5.0, "cc_max": 0.82, "min_dbz": 30.0,
                "gates_within_radius": int(np.sum(within)),
                "debris_gate_count": len(debris_gates),
                "debris_gates": debris_gates,
                "min_cc": jf(np.min(rho_d[debris]), 4) if debris_gates else None,
                "max_dbz": jf(np.max(ref_d[debris]), 2) if debris_gates else None,
            },
        }
    }
    write_golden("tracks.json", payload)


# ----------------------------------------------------------------------- main ---

SECTIONS = {
    "cells": section_cells,
    "swath": section_swath,
    "temporal": section_temporal,
    "tracking": section_tracking,
    "tracks": section_tracks,
}


def main(argv):
    names = argv or list(SECTIONS)
    for name in names:
        if name not in SECTIONS:
            raise SystemExit(f"unknown section {name!r}; choose from {', '.join(SECTIONS)}")
    for name in names:
        SECTIONS[name]()


if __name__ == "__main__":
    main(sys.argv[1:])
