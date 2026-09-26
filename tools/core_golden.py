#!/usr/bin/env python3
"""Golden values for the real-data model tests of recast-radar-core.

The Rust tests in ``crates/recast-radar-core/tests/real_model.rs`` and
``real_merge.rs`` and ``real_cycles.rs`` decode real corpus files with the workspace
readers and compare the model values (fields, sweeps, rays, per-ray instrument
variables, and the results of ``merge_volumes`` and ``scan_cycles``) against
``testdata/golden/core/model.json``, which this script writes.

Every expected value comes from a reader that shares no code with the crates:

- NEXRAD Level II: Py-ART 2.2.5 ``pyart.io.nexrad_level2.NEXRADLevel2File`` for the raw
  moment codes, data-block headers (gate count, first gate, spacing, scale, offset, word
  size) and radial headers; MetPy 1.7.1 ``metpy.io.Level2File`` for the sweep layout
  (radial counts, elevation and azimuth of every radial, moment names).
- ODIM_H5: h5py (``what``/``where`` attributes, raw data planes).
- CfRadial 1: netCDF4-python (raw ``prt``, ``unambiguous_range``, ``n_samples``,
  ``azimuth``, sweep indices).
- JMA GRIB2: a section walker written from the JMA radar GRIB2 template documentation
  (product elevation, grid start azimuth, radial and gate counts), as in
  ``tools/golden_io_formats.py``.

The expected outcomes of ``merge_volumes`` are computed here by a reference
implementation of the merge rules documented on that function (site check, earliest
time, fixed-angle match within 0.05 deg, ray-geometry match with wrap-aware azimuths,
collection-time match within 60 s of the sweeps' first rays, the nearest in time
without a name collision, unmatched sweeps kept as sweeps of their own,
first-part-wins collisions by FM301 field name, stable fixed-angle sort and
renumbering), fed only with the metadata the independent readers produce. The gate
alignment of fields (``separate_fields``) is not modelled: no merge here has fields
whose gates do not align.

The expected ``scan_cycles`` of single files come from a reference implementation of the
rules documented on ``recast_radar_core::model::scan_cycles`` (collection order by each
sweep's first ray; a new cycle where a sweep repeats a cut of the current cycle, the same
fixed angle within 0.05 deg, gate count, first gate, spacing and field names, or starts
more than 240 s after every sweep of the cycle ended), fed with the sweep times and
geometry of the independent readers. The merge
inputs use the FM301 conventions the design note (``docs/design/fm301-model.md``)
documents for each reader:

- field names: ODIM ``what/quantity`` verbatim, and ODIM quality groups as
  ``<quantity>_qualityK`` (a plane's) or ``qualityK`` (a dataset's); JMA reflectivity and velocity as DBZH
  and VRADH; Level II data blocks through the design note's table (REF -> DBZH,
  VEL -> VRADH, SW -> WRADH, ZDR -> ZDR, PHI -> PHIDP, RHO -> RHOHV, CFP -> CCORH);
- time references: ODIM ``/what`` date and time; for JMA the earliest sweep observation
  start (the section 1 reference time plus the smallest template 4.51022 observation
  start offset, octets 51-52); for Level II
  the first radial's collection time floored to the second;
- fixed angles: ODIM ``where/elangle``; the JMA product elevation; for Level II the
  Message 5 cut angle of the sweep's elevation number (MetPy ``vcp_info``), or the first
  radial's elevation when the file has no VCP message;
- sweep start times: ODIM dataset ``what/startdate`` and ``starttime``; for JMA the
  reference time plus the sweep's observation start offset; for Level II the earliest
  radial collection time of the sweep (MetPy radial headers).

Float comparisons use float32 like the Rust code.

Files come from the committed corpus (testdata/files/...) or the shared download cache
that recast-radar-testdata fills (%LOCALAPPDATA%\\recast-radar-tools\\testdata, or
$RECAST_RADAR_TESTDATA); every file is checked against its manifest sha256. Run
``cargo test -p recast-radar-core`` once to download the full volumes.

Usage:
    python tools/core_golden.py

The committed file was written with Python 3.13, numpy 2.5.3, Py-ART 2.2.5,
MetPy 1.7.1, h5py 3.16.0 and netCDF4 1.7.4.
"""

import copy
import datetime
import gzip
import hashlib
import io
import json
import logging
import os
import struct
import sys
import tomllib
import urllib.request
import warnings
from pathlib import Path

import h5py
import netCDF4
import numpy as np

warnings.filterwarnings("ignore")
logging.disable(logging.CRITICAL)

ROOT = Path(__file__).resolve().parent.parent
TESTDATA = ROOT / "testdata"
OUT = TESTDATA / "golden" / "core" / "model.json"

F32 = np.float32
ANGLE_TOLERANCE = F32(0.05)
# recast_radar_core::model::MERGE_TIME_TOLERANCE_S
MERGE_TIME_TOLERANCE_S = 60.0
# recast_radar_core::model::MAX_SCAN_PAUSE_S
MAX_SCAN_PAUSE_S = 240.0

# FM301 names of the Level II data blocks (design note, section 8).
NEXRAD_FIELD_NAMES = {"REF": "DBZH", "VEL": "VRADH", "SW": "WRADH", "ZDR": "ZDR",
                      "PHI": "PHIDP", "RHO": "RHOHV", "CFP": "CCORH"}


# ------------------------------------------------------------------ corpus ---

def load_manifest():
    files = []
    paths = [TESTDATA / "manifest.toml"] if (TESTDATA / "manifest.toml").is_file() else []
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
    if os.name == "nt" and os.environ.get("LOCALAPPDATA"):
        return Path(os.environ["LOCALAPPDATA"], "recast-radar-tools", "testdata")
    if os.environ.get("XDG_CACHE_HOME"):
        return Path(os.environ["XDG_CACHE_HOME"], "recast-radar-tools", "testdata")
    if os.environ.get("HOME"):
        return Path(os.environ["HOME"], ".cache", "recast-radar-tools", "testdata")
    return ROOT / ".testdata-cache"


def corpus_path(entry_id):
    entry = MANIFEST[entry_id]
    if "committed" in entry:
        path = TESTDATA / entry["committed"].removeprefix("testdata/")
    else:
        path = cache_dir() / entry_id
        if not path.is_file():
            path.parent.mkdir(parents=True, exist_ok=True)
            with urllib.request.urlopen(entry["urls"][0], timeout=600) as r:
                path.write_bytes(r.read())
    data = path.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    if digest != entry["sha256"] or len(data) != entry["size"]:
        raise ValueError(f"{entry_id}: sha256/size mismatch ({digest}, {len(data)})")
    return path


def corpus_bytes(entry_id):
    return corpus_path(entry_id).read_bytes()


def f32(x):
    return float(F32(x))


def epoch_s(text, fmt):
    """Seconds since 1970 of a UTC time written as `fmt`."""
    return datetime.datetime.strptime(text, fmt).replace(tzinfo=datetime.UTC).timestamp()


# ------------------------------------------------------------ Level II ---

def level2_bytes(entry_id):
    data = corpus_bytes(entry_id)
    if data[:2] == b"\x1f\x8b":
        data = gzip.decompress(data)
    return data


def metpy_file(data):
    from metpy.io import Level2File
    return Level2File(io.BytesIO(data))


def pyart_file(data):
    from pyart.io.nexrad_level2 import NEXRADLevel2File
    return NEXRADLevel2File(io.BytesIO(data))


def metpy_sweep_layout(sweep):
    """Radial elevations, azimuths and moment names of one MetPy sweep."""
    first = sweep[0][0]
    if isinstance(sweep[0][1], dict) and len(sweep[0]) == 2:
        # Message 1: (header, {name: (hdr, data)})
        names = [name for name in ("REF", "VEL", "SW") if name in sweep[0][1]]
        el = [f32(ray[0].el_angle) for ray in sweep]
        az = [f32(ray[0].az_angle) for ray in sweep]
    else:
        names = [k.decode() for k in sweep[0][4] if k not in (b"VOL", b"ELV", b"RAD")]
        el = [f32(ray[0].el_angle) for ray in sweep]
        az = [f32(ray[0].az_angle) for ray in sweep]
    # Earliest radial collection time (date and milliseconds of day), epoch seconds.
    start_s = min(((int(ray[0].date) - 1) * 86_400_000 + int(ray[0].time_ms)) / 1000 for ray in sweep)
    return {"rays": len(sweep), "elevation_deg": f32(first.el_angle), "moments": names,
            "azimuth_deg": az, "ray_elevation_deg": el, "elevation_number": int(first.el_num),
            "start_s": start_s}


def level2_summary(entry_id, data=None, azimuths=True):
    """Sweep layout of a Level II file as the reference merge sees it."""
    data = level2_bytes(entry_id) if data is None else data
    L = metpy_file(data)
    sweeps = []
    for sweep in L.sweeps:
        if not sweep:
            # MetPy opens an empty sweep for a record that holds no radial
            # (the metadata record of a chunk prefix).
            continue
        layout = metpy_sweep_layout(sweep)
        # The reader's fixed angle: the VCP cut angle of the elevation number,
        # else the first radial's elevation.
        layout["fixed_angle_deg"] = layout["elevation_deg"]
        vcp = getattr(L, "vcp_info", None)
        if vcp is not None and 1 <= layout["elevation_number"] <= len(vcp.els):
            layout["fixed_angle_deg"] = f32(vcp.els[layout["elevation_number"] - 1].el_angle)
        if not azimuths:
            layout = {k: v for k, v in layout.items()
                      if k not in ("azimuth_deg", "ray_elevation_deg", "start_s")}
        sweeps.append(layout)
    header = data[:24]
    icao = header[20:24].decode("ascii", "replace").strip("\0 ")
    time = L.dt.strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"
    # The reader's time reference: the first radial's collection time (radial
    # header date and milliseconds of day), floored to the second.
    first = next(sweep for sweep in L.sweeps if sweep)[0][0]
    first_ms = (int(first.date) - 1) * 86_400_000 + int(first.time_ms)
    first_time = datetime.datetime.fromtimestamp(first_ms / 1000, datetime.UTC)
    reference = first_time.strftime("%Y-%m-%dT%H:%M:%SZ")
    first_radial = first_time.strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"
    return {"icao": icao, "time": time, "time_reference": reference, "first_radial_time": first_radial,
            "sweeps": sweeps}


def raw_grid_summary(records, name, probe):
    """Per-ray raw-code sums, sentinel counts and probes of one Py-ART moment."""
    rows = [record[name] for record in records]
    first = rows[0]
    ngates = max(int(r["ngates"]) for r in rows)
    row_sums = []
    zero = 0
    one = 0
    for r in rows:
        data = np.asarray(r["data"]).astype(np.uint64)
        row_sums.append(int(data.sum()))
        zero += int((data == 0).sum())
        one += int((data == 1).sum())
    probes = []
    for ray, gate in probe:
        data = rows[ray]["data"]
        raw = int(data[gate]) if gate < len(data) else None
        scaled = None
        if raw is not None and raw > 1:
            scaled = f32((F32(raw) - F32(first["offset"])) / F32(first["scale"]))
        probes.append({"ray": ray, "gate": gate, "raw": raw, "scaled": scaled})
    return {
        "rays": len(rows), "gates": ngates,
        "gates_per_ray": [int(r["ngates"]) for r in rows],
        "first_gate_m": int(first["first_gate"]), "gate_spacing_m": int(first["gate_spacing"]),
        "word_size": int(first["word_size"]), "scale": float(first["scale"]),
        "offset": float(first["offset"]),
        "raw_row_sums": row_sums, "code_0_count": zero, "code_1_count": one,
        "probes": probes,
    }


def level2_grids():
    out = {}
    # KTLX 2024-03-15 split cut: REF 8-bit on sweep 1, REF/VEL/SW on sweep 2.
    data = level2_bytes("l2-ktlx-20240315-000217-trim")
    P = pyart_file(data)
    sweep0 = [P.radial_records[i] for i in P.scan_msgs[0]]
    sweep1 = [P.radial_records[i] for i in P.scan_msgs[1]]
    out["ktlx_2024_ref_sweep0"] = raw_grid_summary(
        sweep0, "REF", [(0, 0), (0, 1), (0, 100), (7, 1831), (239, 512), (479, 1000)])
    out["ktlx_2024_vel_sweep1"] = raw_grid_summary(
        sweep1, "VEL", [(0, 0), (0, 40), (100, 300), (479, 1191)])
    out["ktlx_2024_ref_sweep0"]["all_rays_same_gate_count"] = len(
        set(out["ktlx_2024_ref_sweep0"]["gates_per_ray"])) == 1
    # KTLX 2013-05-20: PHI is 16-bit.
    data = level2_bytes("l2-ktlx-20130520-201643-trim")
    P = pyart_file(data)
    sweep0 = [P.radial_records[i] for i in P.scan_msgs[0]]
    out["ktlx_2013_phi_sweep0"] = raw_grid_summary(
        sweep0, "PHI", [(0, 0), (0, 5), (12, 700), (479, 1191)])
    # KDMX 2008-05-25 sweep 11 (index 10): VEL gate count varies along the sweep.
    data = level2_bytes("l2-kdmx-20080525-205148")
    P = pyart_file(data)
    sweep10 = [P.radial_records[i] for i in P.scan_msgs[10]]
    counts = [int(r["VEL"]["ngates"]) for r in sweep10]
    longest = max(counts)
    shortest = min(counts)
    # probes: the last real gate and the first padded gate of short rays.
    probe = [(0, longest - 1)]
    short_rays = [i for i, c in enumerate(counts) if c < longest][:3]
    for ray in short_rays:
        codes = np.asarray(sweep10[ray]["VEL"]["data"])
        echo = np.flatnonzero(codes > 1)
        if len(echo):
            probe.append((ray, int(echo[-1])))
        probe += [(ray, counts[ray] - 1), (ray, counts[ray]), (ray, longest - 1)]
    out["kdmx_2008_vel_sweep10"] = raw_grid_summary(sweep10, "VEL", probe)
    out["kdmx_2008_vel_sweep10"]["shortest_ray_gates"] = shortest
    out["kdmx_2008_vel_sweep10"]["short_ray_count"] = sum(1 for c in counts if c < longest)
    L = metpy_file(data)
    out["kdmx_2008_vel_sweep10"]["elevation_deg"] = f32(L.sweeps[10][0][0].el_angle)
    out["kdmx_2008_vel_sweep10"]["sweep_index"] = 10
    return out


# ---------------------------------------------------------------- ODIM ---

def h5_attr(group, name):
    value = group.attrs[name]
    if isinstance(value, bytes):
        return value.decode()
    if hasattr(value, "item"):
        return value.item()
    return value


def is_quality(name):
    return name.startswith("quality") and name[7:].isdigit()


def quality_order(name):
    return int(name[7:]) if is_quality(name) else -1


def odim_summary(entry_id, probe_gates=()):
    """Sweeps of an ODIM PVOL: elevation, nrays, nbins, quantities, times, raw probes."""
    h = h5py.File(corpus_path(entry_id), "r")
    what = h["what"]
    sweeps = []
    names = sorted((k for k in h if k.startswith("dataset")), key=lambda s: int(s[7:]))
    for name in names:
        g = h[name]
        where = g["where"]
        quantities = {}
        quality_fields = []
        for dname in sorted(k for k in g if k.startswith("data")):
            d = g[dname]
            q = h5_attr(d["what"], "quantity")
            raw = d["data"][()]
            entry = {"shape": list(raw.shape), "dtype": str(raw.dtype),
                     "gain": h5_attr(d["what"], "gain"), "offset": h5_attr(d["what"], "offset"),
                     "nodata": h5_attr(d["what"], "nodata"), "undetect": h5_attr(d["what"], "undetect"),
                     "raw_probes": [{"ray": r, "gate": c, "raw": int(raw[r, c])} for r, c in probe_gates
                                    if r < raw.shape[0] and c < raw.shape[1]]}
            quantities[q] = entry
            # Quality groups of a plane are fields `<quantity>_qualityK`.
            quality_fields += [f"{q}_{k}" for k in sorted(d, key=quality_order) if is_quality(k)]
        # Quality groups of a dataset are fields `qualityK`.
        quality_fields += [k for k in sorted(g, key=quality_order) if is_quality(k)]
        nrays = int(h5_attr(where, "nrays"))
        sweeps.append({
            "dataset": name, "elevation_deg": f32(h5_attr(where, "elangle")),
            "rays": nrays, "gates": int(h5_attr(where, "nbins")),
            "gate_spacing_m": float(h5_attr(where, "rscale")), "first_gate_m": float(h5_attr(where, "rstart")) * 1000.0,
            "start": h5_attr(g["what"], "startdate") + "T" + h5_attr(g["what"], "starttime"),
            "start_s": epoch_s(h5_attr(g["what"], "startdate") + h5_attr(g["what"], "starttime"), "%Y%m%d%H%M%S"),
            "end_s": epoch_s(h5_attr(g["what"], "enddate") + h5_attr(g["what"], "endtime"), "%Y%m%d%H%M%S"),
            "quantities": quantities,
            "quality_fields": quality_fields,
            # ODIM rays are azimuth bins: centre of bin i is (i + 0.5) * 360 / nrays.
            "azimuth_deg": [f32((F32(i) + F32(0.5)) * F32(360.0) / F32(nrays)) for i in range(nrays)],
        })
    return {"source": h5_attr(what, "source"), "time": h5_attr(what, "date") + "T" + h5_attr(what, "time") + "Z",
            "sweeps": sweeps}


def odim_site_id(summary):
    for field in summary["source"].split(","):
        key, _, value = field.partition(":")
        if key == "NOD":
            return value.upper()
    raise ValueError(summary["source"])


# ------------------------------------------------------------ CfRadial ---

def irene_summary():
    path = corpus_path("cfrad1-irene-sr2-20110827-120420-sur-sweeps01")
    ds = netCDF4.Dataset(path)
    v = {}
    for name in ("prt", "unambiguous_range", "n_samples", "azimuth", "elevation",
                 "sweep_start_ray_index", "sweep_end_ray_index", "fixed_angle", "nyquist_velocity"):
        var = ds.variables[name]
        var.set_auto_maskandscale(False)
        v[name] = np.asarray(var[:])
    has_pulse_count = "pulse_count" in ds.variables
    has_independent = "independent_samples" in ds.variables
    sweeps = []
    for s in range(len(v["fixed_angle"])):
        a, b = int(v["sweep_start_ray_index"][s]), int(v["sweep_end_ray_index"][s])
        prt = v["prt"][a:b + 1]
        rng = v["unambiguous_range"][a:b + 1]
        sweeps.append({
            "rays": b - a + 1,
            "prt_s_unique": sorted({f32(x) for x in prt}),
            "unambiguous_range_km_unique": sorted({f32(F32(x) / F32(1000.0)) for x in rng}),
            "n_samples_unique": sorted({int(x) for x in v["n_samples"][a:b + 1]}),
            "azimuth_first": f32(v["azimuth"][a]), "azimuth_last": f32(v["azimuth"][b]),
        })
    return {"instrument": ds.getncattr("instrument_name"), "sweeps": sweeps,
            "has_pulse_count": has_pulse_count, "has_independent_samples": has_independent,
            "time_coverage_start": ds.variables["time_coverage_start"][:].tobytes().decode().strip("\0")}


# ----------------------------------------------------------------- JMA ---

def tar_members(data):
    pos = 0
    members = []
    while pos + 512 <= len(data):
        header = data[pos:pos + 512]
        if header == b"\0" * 512:
            break
        name = header[:100].split(b"\0")[0].decode()
        size = int(header[124:136].split(b"\0")[0].strip() or b"0", 8)
        members.append((name, pos + 512, size))
        pos += 512 + (size + 511) // 512 * 512
    return members


def sm16(raw):
    return None if raw == 0xFFFF else (-(raw & 0x7FFF) if raw & 0x8000 else raw)


def jma_member_sweeps(data):
    """(elevation_deg, radials, gates, start_azimuth_deg, station_id, observation start) per sweep."""
    assert data[:4] == b"GRIB" and data[7] == 2
    total = struct.unpack(">Q", data[8:16])[0]
    msg = data[:total]
    pos = 16
    sections = []
    while pos < len(msg):
        if msg[pos:pos + 4] == b"7777":
            break
        length = struct.unpack(">I", msg[pos:pos + 4])[0]
        sections.append((msg[pos + 4], pos, length))
        pos += length
    sweeps = []
    grid = None
    product = None
    reference = None
    for number, offset, length in sections:
        body = msg[offset:offset + length]
        if number == 1:
            year = struct.unpack(">H", body[12:14])[0]
            reference = f"{year:04}-{body[14]:02}-{body[15]:02}T{body[16]:02}:{body[17]:02}:{body[18]:02}Z"
        elif number == 3:
            grid = {"gates": struct.unpack(">I", body[14:18])[0], "radials": struct.unpack(">I", body[18:22])[0],
                    "gate_spacing_m": struct.unpack(">I", body[30:34])[0] / 1000.0,
                    "range_start_m": struct.unpack(">I", body[34:38])[0] / 1000.0,
                    "start_azimuth_deg": struct.unpack(">H", body[39:41])[0] / 100.0}
        elif number == 4:
            product = {"station_id": body[24:28].decode("ascii").strip(),
                       "elevation_deg": (lambda v: None if v is None else v / 100.0)(sm16(struct.unpack(">H", body[41:43])[0])),
                       "observation_start_offset_s": sm16(struct.unpack(">H", body[50:52])[0]) or 0}
        elif number == 7:
            sweeps.append({**grid, **product})
    return reference, sweeps


def jma_summary(entry_id):
    data = corpus_bytes(entry_id)
    name, offset, size = tar_members(data)[0]
    reference, sweeps = jma_member_sweeps(data[offset:offset + size])
    for s in sweeps:
        n = s["radials"]
        step = F32(360.0) / F32(n)
        s["azimuth_deg"] = [f32(F32(F32(s["start_azimuth_deg"]) + step * F32(i)) % F32(360.0)) for i in range(n)]
    reference_s = epoch_s(reference, "%Y-%m-%dT%H:%M:%SZ")
    for s in sweeps:
        s["start_s"] = reference_s + s["observation_start_offset_s"]
    # The reader's time reference: the earliest sweep observation start.
    earliest = min(s.pop("observation_start_offset_s") for s in sweeps)
    time_reference = (datetime.datetime.strptime(reference, "%Y-%m-%dT%H:%M:%SZ")
                      + datetime.timedelta(seconds=min(earliest, 0))).strftime("%Y-%m-%dT%H:%M:%SZ")
    return {"member": name, "reference_time": reference, "time_reference": time_reference,
            "station_id": sweeps[0]["station_id"], "sweeps_scan_order": sweeps}


# ------------------------------------------------------ reference merge ---

def azimuth_difference(a, b):
    diff = abs(F32(a) - F32(b)) % F32(360.0)
    return min(diff, F32(360.0) - diff)


def rays_match(a, b):
    return len(a["azimuth_deg"]) == len(b["azimuth_deg"]) and all(
        azimuth_difference(x, y) <= ANGLE_TOLERANCE for x, y in zip(a["azimuth_deg"], b["azimuth_deg"]))


def reference_merge(parts):
    """Reference implementation of recast_radar_core::merge_volumes.

    A part is {"site", "time", "sweeps": [{"fixed_angle_deg", "azimuth_deg": [...],
    "start_s", "fields": {name: tag}}]}; the tag says which part a field came from.
    """
    base = copy.deepcopy(parts[0])
    inputs = [{"site": p["site"], "time": p["time"],
               "sweeps": [{"fixed_angle_deg": c["fixed_angle_deg"], "rays": len(c["azimuth_deg"]),
                           "fields": sorted(c["fields"])} for c in p["sweeps"]]} for p in parts]
    report = {"merged_fields": 0, "separate_sweeps": 0, "separate_fields": 0, "field_collisions": 0}
    for part in parts[1:]:
        if part["site"] != base["site"]:
            return {"error": f"sites {base['site']} vs {part['site']}", "parts": inputs}
        base["time"] = min(base["time"], part["time"])
        for sweep in part["sweeps"]:
            matched = False
            target = None  # ((collides, gap), index)
            for index, existing in enumerate(base["sweeps"]):
                if abs(F32(existing["fixed_angle_deg"]) - F32(sweep["fixed_angle_deg"])) > ANGLE_TOLERANCE:
                    continue
                matched = True
                gap = abs(sweep["start_s"] - existing["start_s"])
                if gap > MERGE_TIME_TOLERANCE_S or not rays_match(existing, sweep):
                    continue
                collides = any(name in existing["fields"] for name in sweep["fields"])
                if target is None or (collides, gap) < target[0]:
                    target = ((collides, gap), index)
            if target is None:
                if matched:
                    report["separate_sweeps"] += 1
                base["sweeps"].append(copy.deepcopy(sweep))
                continue
            existing = base["sweeps"][target[1]]
            for name, tag in sweep["fields"].items():
                if name in existing["fields"]:
                    report["field_collisions"] += 1
                else:
                    existing["fields"][name] = tag
                    report["merged_fields"] += 1
    base["sweeps"].sort(key=lambda c: F32(c["fixed_angle_deg"]))  # stable
    for index, sweep in enumerate(base["sweeps"]):
        sweep["elevation_number"] = index + 1
    return {"site": base["site"], "time": base["time"], "report": report, "parts": inputs,
            "sweeps": [{"fixed_angle_deg": c["fixed_angle_deg"], "elevation_number": c["elevation_number"],
                        "rays": len(c["azimuth_deg"]), "fields": c["fields"]} for c in base["sweeps"]]}


def reference_cycles(sweeps):
    """Reference implementation of recast_radar_core::model::scan_cycles.

    `sweeps` are in the reader's order: {"fixed_angle_deg", "gates", "first_gate_m",
    "gate_spacing_m", "fields", "start_s", "end_s"}. Returns the cycles as lists of
    reader sweep indices in collection order, each with what begins it and the
    time of its first ray.
    """
    order = sorted(range(len(sweeps)), key=lambda i: sweeps[i]["start_s"])  # stable
    cycles = []
    current = []
    latest = None  # (index, end)
    for i in order:
        sweep = sweeps[i]
        begins = None
        for j in current:
            earlier = sweeps[j]
            if (abs(F32(earlier["fixed_angle_deg"]) - F32(sweep["fixed_angle_deg"])) <= ANGLE_TOLERANCE
                    and earlier["gates"] == sweep["gates"]
                    and abs(earlier["first_gate_m"] - sweep["first_gate_m"]) <= 0.01
                    and abs(earlier["gate_spacing_m"] - sweep["gate_spacing_m"]) <= 0.01
                    and sorted(earlier["fields"]) == sorted(sweep["fields"])):
                begins = {"kind": "repeated_cut", "sweep": i, "earlier": j,
                          "seconds_apart": sweep["start_s"] - earlier["start_s"]}
                break
        if begins is None and latest is not None and sweep["start_s"] - latest[1] > MAX_SCAN_PAUSE_S:
            begins = {"kind": "pause", "sweep": i, "previous": latest[0],
                      "seconds": sweep["start_s"] - latest[1]}
        if begins is not None or not cycles:
            cycles.append({"sweeps": [], "begins": begins})
            current = []
            latest = None
        cycles[-1]["sweeps"].append(i)
        current.append(i)
        if latest is None or sweep["end_s"] > latest[1]:
            latest = (i, sweep["end_s"])
    for cycle in cycles:
        first = min(sweeps[i]["start_s"] for i in cycle["sweeps"])
        cycle["start"] = datetime.datetime.fromtimestamp(first, datetime.UTC).strftime("%Y-%m-%dT%H:%M:%SZ")
    return cycles


def jma_cycle_sweeps(summary, name):
    # The JMA reader's order (lowest elevation first, stable); every ray of a
    # sweep carries its observation start.
    sweeps = sorted(summary["sweeps_scan_order"], key=lambda s: F32(s["elevation_deg"]))
    return [{"fixed_angle_deg": s["elevation_deg"], "gates": s["gates"], "first_gate_m": s["range_start_m"],
             "gate_spacing_m": s["gate_spacing_m"], "fields": [name], "start_s": s["start_s"],
             "end_s": s["start_s"]} for s in sweeps]


def odim_cycle_sweeps(summary):
    return [{"fixed_angle_deg": s["elevation_deg"], "gates": s["gates"], "first_gate_m": s["first_gate_m"],
             "gate_spacing_m": s["gate_spacing_m"], "fields": [*s["quantities"], *s["quality_fields"]],
             "start_s": s["start_s"], "end_s": s["end_s"]} for s in summary["sweeps"]]


def odim_part(summary):
    """Merge part from an ODIM summary: fields are named by their quantity, then the
    quality groups (a plane's as `<quantity>_qualityK`, the dataset's as `qualityK`)."""
    return {"site": odim_site_id(summary), "time": summary["time"],
            "sweeps": [{"fixed_angle_deg": s["elevation_deg"], "azimuth_deg": s["azimuth_deg"],
                        "start_s": s["start_s"],
                        "fields": {q: summary["tag"] for q in [*s["quantities"], *s["quality_fields"]]}}
                       for s in summary["sweeps"]]}


def level2_part(summary, tag, site=None):
    return {"site": site if site is not None else summary["icao"], "time": summary["time_reference"],
            "sweeps": [{"fixed_angle_deg": s["fixed_angle_deg"], "azimuth_deg": s["azimuth_deg"],
                        "start_s": s["start_s"],
                        "fields": {NEXRAD_FIELD_NAMES[m]: tag for m in s["moments"]}}
                       for s in summary["sweeps"]]}


def jma_part(summary, name, tag):
    # The JMA reader keeps every sweep separate, sorted lowest elevation first
    # (stable) and renumbered.
    sweeps = sorted(summary["sweeps_scan_order"], key=lambda s: F32(s["elevation_deg"]))
    return {"site": summary["station_id"], "time": summary["time_reference"],
            "sweeps": [{"fixed_angle_deg": s["elevation_deg"], "azimuth_deg": s["azimuth_deg"],
                        "start_s": s["start_s"], "fields": {name: tag}} for s in sweeps]}


# ---------------------------------------------------------------- main ---

def strip_azimuths(part_result):
    return part_result


def main():
    golden = {}

    golden["grids"] = level2_grids()

    # Sweep layouts (moment names, counts, elevations) of the trimmed KTLX files.
    layouts = {}
    for entry_id in ("l2-ktlx-20240315-000217-trim", "l2-ktlx-20130520-201643-trim"):
        s = level2_summary(entry_id)
        layouts[entry_id] = {"icao": s["icao"], "first_radial_time": s["first_radial_time"],
                             "time_reference": s["time_reference"], "sweeps": [
            {k: v for k, v in sw.items() if k in ("rays", "elevation_deg", "moments")} | {
                "azimuth_first": sw["azimuth_deg"][0]} for sw in s["sweeps"]]}
    golden["level2_layouts"] = layouts

    golden["irene"] = irene_summary()

    # ODIM per-quantity parts.
    probes = [(0, 0), (10, 25), (200, 150), (359, 299)]
    odim = {}
    for key, entry_id in (("bejab_dbzh", "odim-bejab-20260612-1450-dbzh"), ("bejab_vrad", "odim-bejab-20260612-1450-vrad"),
                          ("nohur_dbzh", "odim-nohur-20260612-1445-dbzh"), ("nohur_th", "odim-nohur-20260612-1445-th"),
                          ("nohur_vradh", "odim-nohur-20260612-1446-vradh")):
        summary = odim_summary(entry_id, probes)
        summary["tag"] = key
        odim[key] = summary
    golden["odim"] = {key: {"site": odim_site_id(s), "time": s["time"], "sweeps": [
        {k: v for k, v in sw.items() if k not in ("azimuth_deg", "start_s", "end_s")} | {"azimuth_first": sw["azimuth_deg"][0]}
        for sw in s["sweeps"]]} for key, s in odim.items()}

    # JMA members.
    jma = {key: jma_summary(entry_id) for key, entry_id in
           (("n5", "jma-n5-20191012-090000-rs47773"), ("n6", "jma-n6-20191012-090000-rs47773"))}
    golden["jma"] = {key: {"station_id": s["station_id"], "reference_time": s["reference_time"],
                           "time_reference": s["time_reference"],
                           "sweeps_scan_order": [{k: v for k, v in sw.items()
                                                  if k not in ("azimuth_deg", "start_s", "range_start_m")}
                                                 for sw in s["sweeps_scan_order"]]} for key, s in jma.items()}

    # KIWA chunk parts: the start chunk plus one intermediate chunk each.
    start = corpus_bytes("l2chunk-kiwa-307-20260917-003629-001-s")
    kiwa = {}
    for number in (2, 3, 14, 26):
        entry_id = f"l2chunk-kiwa-307-20260917-003629-{number:03}-i"
        try:
            chunk = corpus_bytes(entry_id)
        except Exception as error:  # noqa: BLE001 - ephemeral chunk not cached
            print(f"skipping {entry_id}: {error}", file=sys.stderr)
            continue
        kiwa[number] = level2_summary(entry_id, data=start + chunk)
    kiwa_pair = None
    if 2 in kiwa and 3 in kiwa:
        kiwa_pair = level2_summary("pair", data=start + corpus_bytes("l2chunk-kiwa-307-20260917-003629-002-i")
                                   + corpus_bytes("l2chunk-kiwa-307-20260917-003629-003-i"))
    golden["kiwa_chunks"] = {str(n): {"icao": s["icao"], "sweeps": [
        {k: v for k, v in sw.items() if k in ("rays", "elevation_deg", "moments")} | {"azimuth_first": sw["azimuth_deg"][0]}
        for sw in s["sweeps"]]} for n, s in kiwa.items()}

    # KTLX 1999 full volume: sweep 5 (index 4) carries REF at 1 km and VEL/SW at
    # 250 m on the same radials.
    data = level2_bytes("l2-ktlx-19990504-002218")
    P = pyart_file(data)
    L = metpy_file(data)
    sweep4 = [P.radial_records[i] for i in P.scan_msgs[4]]
    hdr = sweep4[0]["msg_header"]
    golden["ktlx_1999_sweep4"] = {
        "sweep_index": 4, "rays": len(sweep4),
        "elevation_deg": f32(L.sweeps[4][0][0].el_angle),
        "ref": {"gates": int(hdr["sur_nbins"]), "first_gate_m": int(hdr["sur_range_first"]),
                "gate_spacing_m": int(hdr["sur_range_step"])},
        "vel": {"gates": int(hdr["doppler_nbins"]), "first_gate_m": int(np.uint16(hdr["doppler_range_first"]).astype(np.int16)),
                "gate_spacing_m": int(hdr["doppler_range_step"])},
        "nyquist_mps": f32(hdr["nyquist_vel"] / 100.0),
        "moments": ["REF", "VEL", "SW"],
    }

    # Reference merge outcomes.
    merges = {}
    ktlx_2024 = level2_summary("l2-ktlx-20240315-000217-trim")
    ktlx_2013 = level2_summary("l2-ktlx-20130520-201643-trim")
    merges["single_part_ktlx_2024"] = reference_merge([level2_part(ktlx_2024, "a")])
    merges["bejab_dbzh_vrad"] = reference_merge([odim_part(odim["bejab_dbzh"]), odim_part(odim["bejab_vrad"])])
    merges["nohur_dbzh_th"] = reference_merge([odim_part(odim["nohur_dbzh"]), odim_part(odim["nohur_th"])])
    # The TH part with its quantity renamed to DBZH: same name, other planes.
    th_as_dbzh = copy.deepcopy(odim["nohur_th"])
    for sweep in th_as_dbzh["sweeps"]:
        sweep["quantities"] = {"DBZH": sweep["quantities"]["TH"]}
    merges["nohur_dbzh_th_as_dbzh"] = reference_merge([odim_part(odim["nohur_dbzh"]), odim_part(th_as_dbzh)])
    merges["nohur_vradh_th_dbzh"] = reference_merge([odim_part(odim["nohur_vradh"]), odim_part(odim["nohur_th"]),
                                                    odim_part(odim["nohur_dbzh"])])
    merges["nohur_dbzh_vradh_th"] = reference_merge([odim_part(odim["nohur_dbzh"]), odim_part(odim["nohur_vradh"]),
                                                    odim_part(odim["nohur_th"])])
    merges["jma_n5_n6"] = reference_merge([jma_part(jma["n5"], "DBZH", "n5"), jma_part(jma["n6"], "VRADH", "n6")])
    merges["jma_n5_n6_n6"] = reference_merge([jma_part(jma["n5"], "DBZH", "n5"), jma_part(jma["n6"], "VRADH", "n6"),
                                              jma_part(jma["n6"], "VRADH", "n6-again")])
    merges["ktlx_2013_2024"] = reference_merge([level2_part(ktlx_2013, "2013"), level2_part(ktlx_2024, "2024")])
    if kiwa_pair is not None:
        merges["kiwa_002_vs_002_003"] = reference_merge([level2_part(kiwa[2], "002"), level2_part(kiwa_pair, "002+003")])
    if all(n in kiwa for n in (2, 14, 26)):
        merges["kiwa_026_002_014"] = reference_merge([level2_part(kiwa[26], "026"), level2_part(kiwa[2], "002"),
                                                      level2_part(kiwa[14], "014")])
    if 2 in kiwa:
        merges["kiwa_002_vs_ktlx_2024"] = reference_merge([level2_part(kiwa[2], "kiwa"), level2_part(ktlx_2024, "ktlx")])
    golden["merges"] = merges

    # Reference scan cycles of single files.
    cycles = {}
    for key, entry_id, name in (("jma_taka_n5", "jma-n5-20191012-090000-rs47773", "DBZH"),
                                ("jma_taka_n6", "jma-n6-20191012-090000-rs47773", "VRADH"),
                                ("jma_itok_n5", "jma-n5-20260924-210000-rs47937", "DBZH"),
                                ("jma_itok_n6", "jma-n6-20260924-210000-rs47937", "VRADH")):
        cycles[key] = reference_cycles(jma_cycle_sweeps(jma_summary(entry_id), name))
    for key in ("nohur_vradh", "nohur_dbzh", "bejab_dbzh", "bejab_vrad"):
        cycles[f"odim_{key}"] = reference_cycles(odim_cycle_sweeps(odim[key]))
    golden["scan_cycles"] = cycles

    OUT.parent.mkdir(parents=True, exist_ok=True)
    with open(OUT, "w", encoding="utf-8", newline="\n") as fh:
        json.dump(golden, fh, indent=1, sort_keys=True)
        fh.write("\n")
    print(f"wrote {OUT} ({OUT.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
