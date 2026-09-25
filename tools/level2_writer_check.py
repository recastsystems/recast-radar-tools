"""Independent readers on Level II files written by recast-radar-tools.

Usage:

    cargo run --release -p recast-radar-io --example level2_writer_check -- OUT
    python tools/level2_writer_check.py OUT/manifest.json

The Rust example writes Level II files from real corpus volumes (NEXRAD
Level II re-encoded with their own metadata, ODIM_H5, CfRadial 1, DORADE and
JMA volumes converted) and a manifest. This script reads every written file
with Py-ART (`pyart.io.read_nexrad_archive`), MetPy (`metpy.io.Level2File`)
and xradar (`xradar.io.open_nexradlevel2_datatree`) and checks:

- every reader opens the file and finds the sweeps and radials the writer
  reported (xradar 0.12 reads a compressed file whose cuts are not
  multiples of 120 radials wrong, by its record addressing: an uncompressed
  variant of the same source must then read in full);
- the readers agree with each other on every gate of every moment (xradar
  0.12's values of 16-bit moments must equal the true codes masked to the
  low bits it keeps);
- the values equal the source's as read by an independent reader of the
  source format:
  - NEXRAD sources: the same reader on the source file returns identical
    arrays (azimuths, elevations, fixed angles, location, every moment);
    for moments whose negative-range gates were dropped (Message 1 files),
    MetPy's per-radial arrays are compared from the first written gate;
  - ODIM_H5 sources: h5py's raw datasets scaled with their gain and offset,
    nodata and undetect as missing, rays in the order the writer reports
    (`written_rays`: from the earliest, where ODIM stores them from north);
  - CfRadial 1 sources: netCDF4's scaled and masked variables;
  - JMA sources (one product's tar): the standard-library GRIB2 walker and
    run-length decoder of tools/golden_io_formats.py, sweeps sorted by
    elevation as the decoder numbers them; the merged N5 and N6 volume has
    no single source file and is checked by the readers' agreement only;
  - DORADE sources, with `--external`: LROSE RadxConvert's CfRadial of the
    source (run.sh must be given the DORADE sources too), read as CfRadial 1;
  within the quantisation error the writer reported for each moment.

With `--nexrad-crate EXE` (tools/level2_writer_nexrad_crate, the Rust
`nexrad` crate), the crate reads every written file too: its sweeps, radial
counts, and per moment the count and sum of the gate values must equal
MetPy's; for NEXRAD sources the crate's reading of the source must equal its
reading of the written file.

With `--external DIR`, the readings of RSL and LROSE Radx made by
tools/level2_writer_external/run.sh (over the written files and the NEXRAD
sources, in the nexbench container) are checked too:

- RSL (`RSL_wsr88d_to_radar`, dumped by rsl_dump.c): every sweep, ray,
  azimuth, elevation, first gate, gate spacing and gate of every moment RSL
  keeps (it reads at most six moments, never CFP) equals MetPy's within
  half of RSL's own storage step (0.01 dB, m/s and correlation, 0.001 dB
  ZDR, 360/65534 degrees PHI; RSL has no range-folded code for PHI and
  RHO, and the value it gives such gates is taken as missing); for NEXRAD
  sources RSL's reading of the source equals its reading of the written
  file exactly;
- Radx (`RadxConvert` to CfRadial with float32 fields, sweeps as in the
  file): every sweep, ray, azimuth, elevation and gate equals MetPy's for
  every moment on the ray's range geometry (Radx remaps moments on other
  gates to it); for NEXRAD sources every variable of Radx's CfRadial of the
  source equals that of the written file.

Exit status 1 when any check fails. Versions used: printed at start.
"""

import argparse
import json
import os
import struct
import subprocess
import sys
import warnings

import h5py
import metpy
import netCDF4
import numpy as np
import pyart
import xradar
from metpy.io import Level2File

warnings.simplefilter("ignore")

PYART = {
    "REF": "reflectivity",
    "VEL": "velocity",
    "SW": "spectrum_width",
    "ZDR": "differential_reflectivity",
    "PHI": "differential_phase",
    "RHO": "cross_correlation_ratio",
    "CFP": "clutter_filter_power_removed",
}
XRADAR = {
    "REF": "DBZH",
    "VEL": "VRADH",
    "SW": "WRADH",
    "ZDR": "ZDR",
    "PHI": "PHIDP",
    "RHO": "RHOHV",
    "CFP": "CCORH",
}
METPY = {
    b"REF": "REF",
    b"VEL": "VEL",
    b"SW ": "SW",
    b"SW": "SW",
    b"ZDR": "ZDR",
    b"PHI": "PHI",
    b"RHO": "RHO",
    b"CFP": "CFP",
}

failures = []
notes = []


def fail(message):
    failures.append(message)
    print("  FAIL", message)


def note(message):
    notes.append(message)
    print("  note", message)


def as_float(array):
    return np.ma.filled(np.ma.asarray(array).astype(np.float64), np.nan)


def pyart_station_table(path):
    """Py-ART takes the location of every site whose ICAO starts with T
    (TDWR) from its station table, and fails on other T sites (the JMA
    Takayasu radar derives as TAKA). Enter such a site with the
    file's own VOL block location."""
    from pyart.io import nexrad_common

    file = pyart.io.nexrad_level2.NEXRADLevel2File(pyart.io.common.prepare_for_read(path))
    icao = file.volume_header["icao"].decode()
    if icao.startswith("T") and icao not in nexrad_common.NEXRAD_LOCATIONS:
        lat, lon, height = file.location()
        nexrad_common.NEXRAD_LOCATIONS[icao] = {"lat": lat, "lon": lon, "elev": height / 0.3048}
        note(f"{path.replace(chr(92), '/').rsplit('/', 1)[-1]}: Py-ART has no table entry for T site {icao}; entered from the file")


def read_pyart(path):
    pyart_station_table(path)
    radar = pyart.io.read_nexrad_archive(path)
    sweeps = []
    for index in range(radar.nsweeps):
        cut = radar.get_slice(index)
        fields = {}
        for moment, name in PYART.items():
            if name not in radar.fields:
                continue
            values = as_float(radar.fields[name]["data"][cut])
            if np.all(np.isnan(values)):
                continue
            fields[moment] = values
        sweeps.append(
            {
                "azimuth": np.asarray(radar.azimuth["data"][cut], dtype=np.float64),
                "elevation": np.asarray(radar.elevation["data"][cut], dtype=np.float64),
                "fixed": float(radar.fixed_angle["data"][index]),
                "range": np.asarray(radar.range["data"], dtype=np.float64),
                "fields": fields,
            }
        )
    location = (
        float(radar.latitude["data"][0]),
        float(radar.longitude["data"][0]),
        float(radar.altitude["data"][0]),
    )
    return sweeps, location


def read_metpy(path):
    file = Level2File(path)
    sweeps = []
    for sweep in file.sweeps:
        if not sweep:
            continue
        fields = {}
        geometry = {}
        for moment_key in sweep[0][4]:
            moment = METPY.get(moment_key)
            if moment is None:
                continue
            rows = []
            for radial in sweep:
                block = radial[4].get(moment_key)
                rows.append(None if block is None else np.asarray(block[1], dtype=np.float64))
            width = max(len(row) for row in rows if row is not None)
            grid = np.full((len(rows), width), np.nan)
            for ray, row in enumerate(rows):
                if row is not None:
                    grid[ray, : len(row)] = row
            header = sweep[0][4][moment_key][0]
            if np.all(np.isnan(grid)):
                continue
            fields[moment] = grid
            geometry[moment] = (header.first_gate * 1000.0, header.gate_width * 1000.0)
        sweeps.append(
            {
                "azimuth": np.array([radial[0].az_angle for radial in sweep]),
                "elevation": np.array([radial[0].el_angle for radial in sweep]),
                "elevation_number": int(sweep[0][0].el_num),
                "fields": fields,
                "geometry": geometry,
            }
        )
    return sweeps


def read_xradar(path):
    tree = xradar.io.open_nexradlevel2_datatree(path)
    sweeps = []
    for name in sorted(
        (child for child in tree.children if child.startswith("sweep_")),
        key=lambda child: int(child.split("_")[1]),
    ):
        ds = tree[name].ds
        fields = {}
        for moment, variable in XRADAR.items():
            if variable in ds:
                values = np.asarray(ds[variable].values, dtype=np.float64)
                # xradar 0.12 scales every code, 0 (below threshold) and 1
                # (range folded) included; mask them as the other readers do.
                encoding = ds[variable].encoding
                if "scale_factor" in encoding:
                    raw = np.rint((values - encoding.get("add_offset", 0.0)) / encoding["scale_factor"])
                    values[raw < 2] = np.nan
                if np.all(np.isnan(values)):
                    continue
                fields[moment] = values
        sweeps.append(
            {
                "azimuth": np.asarray(ds["azimuth"].values, dtype=np.float64),
                "elevation": np.asarray(ds["elevation"].values, dtype=np.float64),
                "fixed": float(ds["sweep_fixed_angle"].values),
                "range": np.asarray(ds["range"].values, dtype=np.float64),
                "fields": fields,
            }
        )
    return sweeps


def sorted_by_azimuth(sweep):
    """The sweep with rays in azimuth order (readers may sort or not)."""
    order = np.argsort(sweep["azimuth"], kind="stable")
    out = dict(sweep)
    out["azimuth"] = sweep["azimuth"][order]
    out["elevation"] = sweep["elevation"][order]
    out["fields"] = {moment: values[order] for moment, values in sweep["fields"].items()}
    return out


def compare_grid(label, a, b, tolerance):
    """Two (rays, gates) arrays: same shape on their common width, NaN where
    the other is NaN, values within `tolerance` (array or scalar); columns
    past the common width must be all NaN."""
    if a.shape[0] != b.shape[0]:
        fail(f"{label}: {a.shape[0]} rays vs {b.shape[0]}")
        return
    width = min(a.shape[1], b.shape[1])
    for extra in (a[:, width:], b[:, width:]):
        if extra.size and not np.all(np.isnan(extra)):
            fail(f"{label}: values past the common {width} gates")
            return
    a = a[:, :width]
    b = b[:, :width]
    nan_a = np.isnan(a)
    nan_b = np.isnan(b)
    if not np.array_equal(nan_a, nan_b):
        fail(f"{label}: {np.sum(nan_a != nan_b)} gates differ in missing/value")
        return
    diff = np.abs(np.where(nan_a, 0.0, a - b))
    limit = np.broadcast_to(tolerance, diff.shape) if np.ndim(tolerance) else tolerance
    bad = diff > limit
    if np.any(bad):
        fail(f"{label}: {np.sum(bad)} gates off, largest {diff.max():.6g}")


def value_tolerance(values, error):
    return error + 1e-4 * np.nan_to_num(np.abs(values)) + 1e-6


def moments_by_sweep(summary):
    """Source sweep index -> {moment: report}."""
    out = {}
    for report in summary["moments"]:
        out.setdefault(report["sweep"], {})[report["moment"]] = report
    return out


def written_sweeps(summary, nsource):
    skipped = set(summary["skipped_sweeps"])
    return [index for index in range(nsource) if index not in skipped]


def written_order(summary, source_index, nrays):
    """The source ray of each written radial of a source sweep: the writer
    writes a sweep stored from another azimuth (ODIM stores rays from north)
    from its earliest ray, and leaves out rays without data."""
    for entry in summary.get("written_rays", []):
        if entry["sweep"] == source_index:
            return np.asarray(entry["rays"], dtype=np.int64)
    return np.arange(nrays)


def same_gates(reader_range, geometry):
    """True when a reader's common range starts at the moment's first gate
    with its spacing (Py-ART and xradar put every moment of a volume or sweep
    on one range; moments on other gates are resampled or shifted there)."""
    if reader_range is None or len(reader_range) < 2:
        return False
    first, spacing = geometry
    return abs(reader_range[0] - first) < 1e-3 and abs(reader_range[1] - reader_range[0] - spacing) < 1e-3


# xradar 0.12 keeps the low bits of a moment's words
# (NexradLevel2ArrayWrapper._getitem): 11 of 16-bit ZDR, 10 of 16-bit PHI,
# 8 of every other moment.
XRADAR_CODE_MASK = {"ZDR": 0x7FF, "PHI": 0x3FF}


def xradar_misreads(moment, report, values):
    """True when xradar 0.12's mask drops bits of this moment's codes: any
    16-bit moment but ZDR and PHI, and ZDR or PHI codes above 11 or 10
    bits."""
    if report is None or report["word_size"] != 16:
        return False
    mask = XRADAR_CODE_MASK.get(moment)
    if mask is None:
        return True
    finite = values[np.isfinite(values)]
    if finite.size == 0:
        return False
    codes = np.rint(finite * report["scale"] + report["offset"])
    return bool(codes.max() > mask)


def xradar_masked(moment, report, values):
    """What xradar 0.12 returns for a 16-bit moment whose true values
    (MetPy's) are `values`: every code masked to its low bits, masked codes
    below 2 missing."""
    mask = XRADAR_CODE_MASK.get(moment, 0xFF)
    scale = np.float32(report["scale"])
    offset = np.float32(report["offset"])
    codes = np.rint(np.nan_to_num(values) * report["scale"] + report["offset"]).astype(np.int64)
    masked = np.where(np.isnan(values), 0, codes & mask)
    decoded = ((masked.astype(np.float32) - offset) / scale).astype(np.float64)
    return np.where(masked < 2, np.nan, decoded)


def check_readers_agree(label, pyart_sweeps, metpy_sweeps, xradar_sweeps, reports=None):
    """Every reader sees the same values on the same file, for every moment
    whose gates are the reader's range. `reports[index][moment]` is the
    writer's report of the moment in written sweep `index`."""
    for index, m in enumerate(metpy_sweeps):
        m = sorted_by_azimuth(m)
        others = []
        if pyart_sweeps is not None:
            others.append(("Py-ART", sorted_by_azimuth(pyart_sweeps[index])))
        if xradar_sweeps is not None:
            others.append(("xradar", sorted_by_azimuth(xradar_sweeps[index])))
        for moment, values in m["fields"].items():
            tol = 1e-4 * np.nan_to_num(np.abs(values)) + 1e-5
            geometry = m["geometry"][moment]
            report = (reports or {}).get(index, {}).get(moment)
            for reader, sweep in others:
                if reader == "xradar" and xradar_misreads(moment, report, values):
                    # xradar 0.12 keeps the low bits of the words: its values
                    # must be exactly the masked true codes.
                    if moment not in sweep["fields"]:
                        fail(f"{label} sweep {index}: xradar lacks {moment}")
                        continue
                    expected = xradar_masked(moment, report, values)
                    compare_grid(
                        f"{label} sweep {index} {moment} xradar vs MetPy's codes masked as xradar 0.12 masks them",
                        sweep["fields"][moment],
                        expected,
                        1e-4 * np.nan_to_num(np.abs(expected)) + 1e-5,
                    )
                    note(
                        f"{label} sweep {index} {moment}: xradar 0.12 masks the {report['word_size']}-bit codes "
                        f"to {XRADAR_CODE_MASK.get(moment, 0xFF).bit_length()} bits (its values equal the masked codes)"
                    )
                elif moment not in sweep["fields"]:
                    fail(f"{label} sweep {index}: {reader} lacks {moment}")
                elif same_gates(sweep.get("range"), geometry):
                    compare_grid(f"{label} sweep {index} {moment} {reader} vs MetPy", sweep["fields"][moment], values, tol)
                else:
                    note(f"{label} sweep {index} {moment}: {reader} resamples it to its common range; not compared")


def check_nexrad_source(label, entry, pyart_out, metpy_out, xradar_out, location_out):
    source = entry["source_path"]
    dropped = {
        (report["sweep"], report["moment"]): report["dropped_gates"]
        for report in entry["summary"]["moments"]
        if report["dropped_gates"]
    }
    try:
        metpy_src = read_metpy(source)
    except Exception as error:  # noqa: BLE001
        note(f"{label}: MetPy cannot read the source: {type(error).__name__}: {error}")
        return
    if len(metpy_src) != len(metpy_out):
        fail(f"{label}: MetPy {len(metpy_src)} sweeps in the source, {len(metpy_out)} written")
        return
    for index, (s, o) in enumerate(zip(metpy_src, metpy_out)):
        for moment, values in s["fields"].items():
            if moment not in o["fields"]:
                fail(f"{label} sweep {index}: MetPy lost {moment}")
                continue
            skip = dropped.get((index, moment), 0)
            src = values[:, skip:]
            compare_grid(f"{label} sweep {index} {moment} MetPy source vs written", src, o["fields"][moment], 0.0)
            first, spacing = s["geometry"][moment]
            if o["geometry"][moment] != (first + skip * spacing, spacing):
                fail(f"{label} sweep {index} {moment}: geometry {s['geometry'][moment]} -> {o['geometry'][moment]}")
        if not np.array_equal(s["azimuth"], o["azimuth"]) or not np.array_equal(s["elevation"], o["elevation"]):
            fail(f"{label} sweep {index}: MetPy azimuths or elevations differ")
    if dropped:
        note(f"{label}: gates before the radar were dropped; compared through MetPy only")
        return
    try:
        pyart_src, location_src = read_pyart(source)
    except Exception as error:  # noqa: BLE001
        note(f"{label}: Py-ART cannot read the source: {type(error).__name__}: {error}")
        pyart_src, location_src = None, location_out
    # Py-ART takes TDWR locations from its station table, whose entry it
    # converts from feet in place on every call: a second read in one
    # process returns another altitude.
    if location_src != location_out and not label.startswith("l2-t"):
        fail(f"{label}: Py-ART location {location_src} -> {location_out}")
    try:
        xradar_src = read_xradar(source)
    except Exception as error:  # noqa: BLE001
        xradar_src = None
        note(f"{label}: xradar cannot read the source: {type(error).__name__}: {error}")
    source_rays = [len(sweep["azimuth"]) for sweep in metpy_src]
    for reader, src_sweeps, out_sweeps in (
        ("Py-ART", pyart_src, pyart_out),
        ("xradar", xradar_src, xradar_out),
    ):
        if src_sweeps is None or out_sweeps is None:
            continue
        rays = [len(sweep["azimuth"]) for sweep in src_sweeps]
        if rays != source_rays:
            note(f"{label}: {reader} reads the source as {rays} rays per sweep, MetPy {source_rays}; not compared")
            continue
        if len(src_sweeps) != len(out_sweeps):
            fail(f"{label}: {reader} {len(src_sweeps)} sweeps -> {len(out_sweeps)}")
            continue
        for index, (s, o) in enumerate(zip(src_sweeps, out_sweeps)):
            for key in ("azimuth", "elevation", "range"):
                if not np.array_equal(s[key], o[key]):
                    fail(f"{label} sweep {index}: {reader} {key} differs")
            if s["fixed"] != o["fixed"]:
                fail(f"{label} sweep {index}: {reader} fixed angle {s['fixed']} -> {o['fixed']}")
            if sorted(s["fields"]) != sorted(o["fields"]):
                fail(f"{label} sweep {index}: {reader} moments {sorted(s['fields'])} -> {sorted(o['fields'])}")
            for moment in s["fields"]:
                if moment in o["fields"]:
                    compare_grid(f"{label} sweep {index} {moment} {reader} source vs written", s["fields"][moment], o["fields"][moment], 0.0)


def odim_reference(path):
    """Sweeps of an ODIM_H5 file read with h5py: {quantity: values} in
    stored row order, first gate centre and spacing in metres."""
    with h5py.File(path, "r") as file:
        datasets = sorted(
            (name for name in file if name.startswith("dataset")),
            key=lambda name: int(name[len("dataset"):]),
        )
        root_what = dict(file["what"].attrs) if "what" in file else {}
        sweeps = []
        for name in datasets:
            ds = file[name]
            where = dict(ds["where"].attrs)
            ds_what = dict(ds["what"].attrs) if "what" in ds else {}
            fields = {}
            for data_name in sorted(n for n in ds if n.startswith("data")):
                group = ds[data_name]
                what = dict(root_what)
                what.update(ds_what)
                if "what" in group:
                    what.update(dict(group["what"].attrs))
                quantity = what["quantity"]
                quantity = quantity.decode() if isinstance(quantity, bytes) else str(quantity)
                raw = group["data"][()]
                values = raw.astype(np.float64) * float(what.get("gain", 1.0)) + float(what.get("offset", 0.0))
                for sentinel in ("nodata", "undetect"):
                    if sentinel in what:
                        values[raw == what[sentinel]] = np.nan
                fields[quantity] = values
            rscale = float(where["rscale"])
            rstart = float(where["rstart"])
            # ODIM gives rstart in km; AEMET writes metres (200 for 0.2 km).
            start = rstart if rstart * 1000.0 > 100_000.0 else rstart * 1000.0
            first = start + rscale / 2.0
            sweeps.append({"fields": fields, "geometry": (first, rscale), "elangle": float(where["elangle"])})
    return sweeps


def cfradial_reference(path):
    """Sweeps of a CfRadial 1 file read with netCDF4: {variable: values}."""
    with netCDF4.Dataset(path) as nc:
        nc.set_auto_maskandscale(True)
        starts = np.asarray(nc["sweep_start_ray_index"][:])
        ends = np.asarray(nc["sweep_end_ray_index"][:])
        ranges = np.asarray(nc["range"][:], dtype=np.float64)
        variables = {
            name: var
            for name, var in nc.variables.items()
            if var.dimensions == ("time", "range")
        }
        sweeps = []
        for start, end in zip(starts, ends):
            fields = {name: as_float(var[start : end + 1, :]) for name, var in variables.items()}
            spacing = ranges[1] - ranges[0] if len(ranges) > 1 else 0.0
            sweeps.append({"fields": fields, "geometry": (float(ranges[0]), float(spacing))})
    return sweeps


def jma_reference(path):
    """Sweeps of a JMA polar GRIB2 tar (its first member) read with the
    standard-library GRIB2 walker and run-length decoder of
    tools/golden_io_formats.py: {DBZH or VRADH: values} per GRIB2 message
    (level 0 and levels whose table value is missing as NaN), the range
    start and gate spacing, sorted by elevation with the scan order kept
    among equal elevations (the order the decoder numbers them in)."""
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import golden_io_formats as walker

    with open(path, "rb") as handle:
        data = handle.read()
    member = walker.tar_members(data)[0]
    msg = data[member["data_offset"]:member["data_offset"] + member["size"]]
    msg = msg[:struct.unpack(">Q", msg[8:16])[0]]
    names = {(15, 1): "DBZH", (15, 2): "VRADH"}
    sweeps = []
    grid = product = levels = None
    pos = 16
    while pos < len(msg) and msg[pos:pos + 4] != b"7777":
        length = struct.unpack(">I", msg[pos:pos + 4])[0]
        body = msg[pos:pos + length]
        number = body[4]
        if number == 3:
            grid = {
                "points": struct.unpack(">I", body[6:10])[0],
                "gates": struct.unpack(">I", body[14:18])[0],
                "radials": struct.unpack(">I", body[18:22])[0],
                "spacing": struct.unpack(">I", body[30:34])[0] / 1000.0,
                "start": struct.unpack(">I", body[34:38])[0] / 1000.0,
            }
        elif number == 4:
            elevation = walker.sm16(struct.unpack(">H", body[41:43])[0])
            product = {
                "name": names[(body[9], body[10])],
                "elevation": 0.0 if elevation is None else elevation / 100.0,
            }
        elif number == 5:
            count = struct.unpack(">H", body[14:16])[0]
            factor = 10.0 ** (-body[16])
            table = [walker.sm16(struct.unpack(">H", body[17 + 2 * i:19 + 2 * i])[0]) for i in range(count)]
            levels = {
                "nbits": body[11],
                "max_value": struct.unpack(">H", body[12:14])[0],
                "table": np.array([np.nan] + [np.nan if v is None else v * factor for v in table]),
            }
        elif number == 7:
            codes = walker.jma_run_length(body[5:], levels["nbits"], levels["max_value"], grid["points"])
            values = levels["table"][np.asarray(codes)].reshape(grid["radials"], grid["gates"])
            sweeps.append({
                "fields": {product["name"]: values},
                "geometry": (grid["start"], grid["spacing"]),
                "elevation": product["elevation"],
            })
        pos += length
    sweeps.sort(key=lambda sweep: sweep["elevation"])
    return sweeps


def radx_reference(label, entry, directory):
    """RadxConvert's CfRadial of a DORADE source (run.sh reads the sources
    it is given too), read as a CfRadial 1 file; None when there is none."""
    name = os.path.basename(entry["source_path"])
    path = os.path.join(directory, "radx", name + ".nc")
    if not os.path.exists(path):
        fail(f"{label}: no RadxConvert reading of the source {name} (pass it to run.sh)")
        return None
    sweeps = cfradial_reference(path)
    with netCDF4.Dataset(path) as nc:
        if "antenna_transition" not in nc.variables:
            return sweeps
        transition = np.asarray(nc["antenna_transition"][:]).astype(bool)
        starts = np.asarray(nc["sweep_start_ray_index"][:])
        ends = np.asarray(nc["sweep_end_ray_index"][:])
    if transition.any():
        # The DORADE decoder leaves out rays flagged in transition (the
        # RYIB ray status); RadxConvert keeps and flags them.
        note(f"{label}: RadxConvert keeps {int(transition.sum())} transition rays the decoder leaves out; left out of the reference too")
        for sweep, start, end in zip(sweeps, starts, ends):
            keep = ~transition[start:end + 1]
            sweep["fields"] = {name: values[keep] for name, values in sweep["fields"].items()}
    return sweeps


def check_reference(label, entry, reference, metpy_out, pyart_out):
    """Values of the written file (MetPy's per-radial arrays and Py-ART's
    grid, both in file order) against the independent source reading."""
    summary = entry["summary"]
    reports = moments_by_sweep(summary)
    written = written_sweeps(summary, len(reference))
    if len(written) != len(metpy_out):
        fail(f"{label}: {len(written)} sweeps expected, MetPy finds {len(metpy_out)}")
        return
    for out_index, source_index in enumerate(written):
        src = reference[source_index]
        for moment, report in reports.get(source_index, {}).items():
            values = src["fields"].get(report["field"])
            if values is None:
                fail(f"{label} sweep {source_index}: source lacks {report['field']}")
                continue
            # The source's rays in the order they were written.
            values = values[written_order(summary, source_index, values.shape[0])]
            tol = value_tolerance(values, report["max_abs_error"])
            m = metpy_out[out_index]
            if moment not in m["fields"]:
                if not np.all(np.isnan(values)):
                    fail(f"{label} sweep {source_index}: MetPy lacks {moment}")
                continue
            compare_grid(f"{label} sweep {source_index} {moment} MetPy vs source", values, m["fields"][moment], tol)
            first, spacing = src["geometry"]
            got = m["geometry"][moment]
            if abs(got[0] - round(first)) > 1e-3 or abs(got[1] - round(spacing)) > 1e-3:
                fail(f"{label} sweep {source_index} {moment}: geometry {src['geometry']} -> {got}")
            if pyart_out is None:
                continue
            p = pyart_out[out_index]
            if moment in p["fields"] or np.all(np.isnan(values)):
                compare_grid(f"{label} sweep {source_index} {moment} Py-ART vs source", values, p["fields"][moment], tol)
            else:
                fail(f"{label} sweep {source_index}: Py-ART lacks {moment}")


def nexrad_crate(exe, paths):
    """The crate's reading of each path: {path: json}."""
    out = subprocess.run([exe, *paths], capture_output=True, text=True, check=True).stdout
    readings = {}
    for line in out.splitlines():
        reading = json.loads(line)
        readings[reading["path"]] = reading
    return readings


def check_nexrad_crate(label, reading, metpy_out, source_reading):
    if "error" in reading:
        if source_reading is not None and source_reading.get("error") == reading["error"]:
            # The file carries the source's metadata record, and the crate
            # fails on both alike (KVWX 2008 has no Message 5).
            note(f"{label}: the nexrad crate fails on the source and the file alike: {reading['error']}")
        else:
            fail(f"{label}: the nexrad crate fails: {reading['error']}")
        return
    sweeps = reading["sweeps"]
    if len(sweeps) != len(metpy_out):
        fail(f"{label}: the nexrad crate reads {len(sweeps)} sweeps, MetPy {len(metpy_out)}")
        return
    for index, (crate, m) in enumerate(zip(sweeps, metpy_out)):
        if crate["radials"] != len(m["azimuth"]):
            fail(f"{label} sweep {index}: the nexrad crate reads {crate['radials']} radials, MetPy {len(m['azimuth'])}")
        for moment, values in m["fields"].items():
            stats = crate["moments"].get(moment)
            if stats is None:
                if moment != "CFP":
                    fail(f"{label} sweep {index}: the nexrad crate lacks {moment}")
                continue
            finite = values[np.isfinite(values)]
            if stats["values"] != finite.size:
                fail(f"{label} sweep {index} {moment}: the nexrad crate {stats['values']} values, MetPy {finite.size}")
            elif abs(stats["sum"] - finite.sum()) > 1e-5 * max(1.0, np.abs(finite).sum()):
                fail(f"{label} sweep {index} {moment}: the nexrad crate sums {stats['sum']}, MetPy {finite.sum()}")
    if source_reading is not None:
        written = [len(m["azimuth"]) for m in metpy_out]
        if "error" in source_reading:
            note(f"{label}: the nexrad crate cannot read the source: {source_reading['error']}")
        elif [sweep["radials"] for sweep in source_reading["sweeps"]] != written:
            # MetPy reads these radials from the source and the file alike.
            counts = [sweep["radials"] for sweep in source_reading["sweeps"]]
            note(f"{label}: the nexrad crate misreads the source ({counts} radials per sweep); not compared")
        elif source_reading["sweeps"] != sweeps:
            fail(f"{label}: the nexrad crate reads the source and the written file differently")


# RSL 1.50 stores gates as 16-bit codes (USE_TWO_BYTE_PRECISION): its step
# and the range it can hold per moment (volume.c, XX_F and XX_INVF).
RSL_STEP = {"REF": 0.01, "VEL": 0.01, "SW": 0.01, "ZDR": 0.001, "PHI": 360.0 / 65534.0, "RHO": 0.01}
RSL_RANGE = {
    "REF": (-50.0, (65535 - 4) / 100.0 - 50.0),
    "VEL": (-127.0, (65535 - 4) / 100.0 - 127.0),
    "SW": (-127.0, (65535 - 4) / 100.0 - 127.0),
    "ZDR": (-12.0, (65535 - 4) / 1000.0 - 12.0),
    "PHI": (0.0, 360.0 - 360.0 / 65534.0),
    "RHO": (0.0, (65535 - 2) / 100.0),
}


def read_rsl(path):
    """rsl_dump's output: {elevation number: {moment: arrays}}. RSL keeps
    each moment's sweeps in a volume of its own and drops the empty ones, so
    a cut is found by its elevation number, not its index."""
    with open(path, "rb") as handle:
        data = handle.read()
    if data[:8] != b"RSLDUMP2":
        raise ValueError(f"{path}: not an rsl_dump file")
    at = 8
    sweeps = {}
    ray_type = np.dtype(
        [
            ("azimuth", "<f4"),
            ("elevation", "<f4"),
            ("nyquist", "<f4"),
            ("first", "<i4"),
            ("spacing", "<i4"),
            ("nbins", "<i4"),
            ("present", "<i4"),
        ]
    )
    while data[at : at + 4] != b"END\0":
        name = data[at : at + 4].rstrip(b"\0").decode()
        _, number, nrays, nbins = (int(v) for v in np.frombuffer(data, "<i4", 4, at + 4))
        folded_as = float(np.frombuffer(data, "<f4", 1, at + 20)[0])
        at += 24
        rays = np.frombuffer(data, ray_type, nrays, at)
        at += ray_type.itemsize * nrays
        values = np.frombuffer(data, "<f4", nrays * nbins, at).reshape(nrays, nbins).astype(np.float64)
        at += 4 * nrays * nbins
        sweeps.setdefault(number, {})[name] = {"rays": rays, "values": values, "folded_as": folded_as}
    return sweeps


def check_rsl(label, sweeps, metpy_out):
    """RSL's reading of a written file against MetPy's, cut by cut."""
    numbers = [m["elevation_number"] for m in metpy_out]
    if sorted(sweeps) != sorted(numbers):
        fail(f"{label}: RSL reads cuts {sorted(sweeps)}, MetPy {numbers}")
        return
    unrepresentable = 0
    folded = {}
    for index, m in enumerate(metpy_out):
        cut = sweeps[m["elevation_number"]]
        for moment, values in m["fields"].items():
            if moment == "CFP":
                continue
            got = cut.get(moment)
            if got is None:
                fail(f"{label} sweep {index}: RSL lacks {moment}")
                continue
            rays = got["rays"]
            where = f"{label} sweep {index} {moment} RSL vs MetPy"
            if len(rays) != len(m["azimuth"]) or not np.all(rays["present"] == 1):
                fail(f"{where}: {int(np.sum(rays['present']))} of {len(rays)} ray slots, MetPy {len(m['azimuth'])} rays")
                continue
            if not np.array_equal(rays["azimuth"], m["azimuth"].astype(np.float32)) or not np.array_equal(
                rays["elevation"], m["elevation"].astype(np.float32)
            ):
                fail(f"{where}: azimuths or elevations differ")
            first, spacing = m["geometry"][moment]
            if np.any(rays["first"] != round(first)) or np.any(rays["spacing"] != round(spacing)):
                fail(f"{where}: geometry {sorted(set(zip(rays['first'], rays['spacing'])))}, MetPy {(first, spacing)}")
            width = max(values.shape[1], got["values"].shape[1])
            rsl = np.full((len(rays), width), np.nan)
            rsl[:, : got["values"].shape[1]] = np.where(np.isneginf(got["values"]), np.nan, got["values"])
            reference = np.full((len(rays), width), np.nan)
            reference[:, : values.shape[1]] = values
            if np.isfinite(got["folded_as"]):
                # RSL's PHI and RHO storage has no range-folded code: RSL
                # gives such gates this value; MetPy has them missing.
                stand_in = np.isnan(reference) & (rsl == np.float32(got["folded_as"]))
                folded[moment] = folded.get(moment, 0) + int(np.sum(stand_in))
                rsl[stand_in] = np.nan
            low, high = RSL_RANGE[moment]
            outside = np.isfinite(reference) & ((reference < low) | (reference > high))
            unrepresentable += int(np.sum(outside))
            reference[outside] = np.nan
            rsl[outside] = np.nan
            tol = RSL_STEP[moment] / 2.0 + 1e-5 * np.nan_to_num(np.abs(reference)) + 1e-6
            compare_grid(where, rsl, reference, tol)
    if unrepresentable:
        note(f"{label}: {unrepresentable} gates lie outside the range RSL can store; not compared")
    for moment, count in folded.items():
        if count:
            note(f"{label}: RSL gives {count} range-folded {moment} gates a value (it has no range-folded {moment} code)")


def check_rsl_source(label, source, written):
    """RSL's reading of a NEXRAD source against its reading of the file:
    every ray header and gate, range folding included."""
    if sorted(source) != sorted(written):
        fail(f"{label}: RSL reads cuts {sorted(source)} from the source, {sorted(written)} from the file")
        return
    for number in sorted(source):
        if sorted(source[number]) != sorted(written[number]):
            fail(f"{label} cut {number}: RSL moments {sorted(source[number])} -> {sorted(written[number])}")
            continue
        for moment, s in source[number].items():
            o = written[number][moment]
            if s["rays"].tobytes() != o["rays"].tobytes():
                fail(f"{label} cut {number} {moment}: RSL ray headers differ between the source and the file")
            elif s["values"].shape != o["values"].shape or not np.array_equal(s["values"], o["values"], equal_nan=True):
                fail(f"{label} cut {number} {moment}: RSL values differ between the source and the file")


def read_radx(path):
    """RadxConvert's CfRadial: per sweep {moment: (rays, gates)}, ray
    geometry, azimuths and elevations."""
    with netCDF4.Dataset(path) as nc:
        nc.set_auto_maskandscale(True)
        starts = np.asarray(nc["sweep_start_ray_index"][:])
        ends = np.asarray(nc["sweep_end_ray_index"][:])
        ngates = np.asarray(nc["ray_n_gates"][:]) if "ray_n_gates" in nc.variables else None
        offsets = np.asarray(nc["ray_start_index"][:]) if "ray_start_index" in nc.variables else None
        first = np.asarray(nc["ray_start_range"][:], dtype=np.float64)
        spacing = np.asarray(nc["ray_gate_spacing"][:], dtype=np.float64)
        azimuth = np.asarray(nc["azimuth"][:], dtype=np.float64)
        elevation = np.asarray(nc["elevation"][:], dtype=np.float64)
        fields = {name: as_float(nc[name][:]) for name in METPY.values() if name in nc.variables}
    sweeps = []
    for start, end in zip(starts, ends):
        rays = range(start, end + 1)
        grids = {}
        for name, flat in fields.items():
            if offsets is None:
                grids[name] = flat[start : end + 1]
                continue
            width = int(max(ngates[ray] for ray in rays))
            grid = np.full((len(rays), width), np.nan)
            for row, ray in enumerate(rays):
                grid[row, : ngates[ray]] = flat[offsets[ray] : offsets[ray] + ngates[ray]]
            grids[name] = grid
        sweeps.append(
            {
                "azimuth": azimuth[start : end + 1],
                "elevation": elevation[start : end + 1],
                "first": first[start : end + 1],
                "spacing": spacing[start : end + 1],
                "fields": {name: grid for name, grid in grids.items() if not np.all(np.isnan(grid))},
            }
        )
    return sweeps


def check_radx(label, sweeps, metpy_out):
    """Radx's reading of a written file against MetPy's."""
    if len(sweeps) != len(metpy_out):
        fail(f"{label}: Radx reads {len(sweeps)} sweeps, MetPy {len(metpy_out)}")
        return
    remapped = []
    for index, (r, m) in enumerate(zip(sweeps, metpy_out)):
        if len(r["azimuth"]) != len(m["azimuth"]):
            fail(f"{label} sweep {index}: Radx reads {len(r['azimuth'])} rays, MetPy {len(m['azimuth'])}")
            continue
        if not np.allclose(r["azimuth"], m["azimuth"], atol=1e-4) or not np.allclose(r["elevation"], m["elevation"], atol=1e-4):
            fail(f"{label} sweep {index}: Radx azimuths or elevations differ from MetPy's")
        for moment, values in m["fields"].items():
            if moment not in r["fields"]:
                if moment == "CFP":
                    # Radx leaves CFP out of some cuts (here the ones that
                    # carry velocity too); it does so in the sources alike.
                    continue
                fail(f"{label} sweep {index}: Radx lacks {moment}")
                continue
            first, spacing = m["geometry"][moment]
            if np.any(np.abs(r["first"] - first) > 1e-3) or np.any(np.abs(r["spacing"] - spacing) > 1e-3):
                remapped.append(f"{index} {moment}")
                continue
            tol = 1e-4 * np.nan_to_num(np.abs(values)) + 1e-5
            compare_grid(f"{label} sweep {index} {moment} Radx vs MetPy", r["fields"][moment], values, tol)
    if remapped:
        note(f"{label}: Radx remaps moments to the ray's range geometry; not compared: {', '.join(remapped)}")


def check_radx_source(label, source_path, written_path, replaced):
    """Every variable of Radx's CfRadial of a NEXRAD source against that of
    the written file. Variables Radx takes from metadata messages the writer
    replaced (`replaced`: the summary's notes on them) may differ."""
    differ = []
    with netCDF4.Dataset(source_path) as a, netCDF4.Dataset(written_path) as b:
        a.set_auto_maskandscale(False)
        b.set_auto_maskandscale(False)
        differ += sorted(set(a.variables) ^ set(b.variables))
        for name in a.variables:
            if name not in b.variables:
                continue
            x = a[name][:]
            y = b[name][:]
            if x.shape != y.shape or not np.array_equal(np.asarray(x), np.asarray(y), equal_nan=x.dtype.kind == "f"):
                differ.append(name)
    if not differ:
        return
    if replaced:
        note(f"{label}: Radx variables {differ} differ from the source's, the writer having replaced metadata messages")
    else:
        fail(f"{label}: Radx variables {differ} differ between the source and the file")


def external_readings(directory):
    """index.tsv of tools/level2_writer_external/run.sh: {name: (site, rsl, radx)}."""
    index = {}
    with open(f"{directory}/index.tsv", encoding="utf-8") as handle:
        for line in handle:
            name, site, rsl, radx = line.rstrip("\n").split("\t")
            index[name] = (site, int(rsl), int(radx))
    return index


def external_error(directory, reader, name):
    with open(f"{directory}/{reader}/{name}.log", encoding="utf-8", errors="replace") as handle:
        lines = [line.strip() for line in handle if line.strip()]
    errors = [line for line in lines if "rror" in line or "failed" in line or "ERROR" in line]
    return (errors or lines or ["no output"])[-1]


def check_external(label, entry, metpy_out, directory, index):
    notes_ = entry["summary"].get("notes", [])
    replaced = [text for text in notes_ if "header version" not in text]
    relabelled = any("header version" in text for text in notes_)
    # RSL gunzips a file and then reads its records as uncompressed, as in
    # NOAA's gzip archives ("gzip"), so it reads LDM records in gzip
    # ("bzip2-gzip") only unwrapped; RadxConvert reads no gzip file here.
    # run.sh reads the file each gzip wraps too, as NAME.unwrapped.
    rsl_name = radx_name = label
    if entry["variant"] == "bzip2-gzip":
        note(f"{label}: RSL reads gzip-wrapped LDM records only unwrapped; reading the file the gzip wraps")
        rsl_name = f"{label}.unwrapped"
    if entry["variant"].endswith("gzip"):
        note(f"{label}: RadxConvert reads no gzip file; reading the file the gzip wraps")
        radx_name = f"{label}.unwrapped"
    source = None
    if entry["source_format"] == "nexrad-level2" and not any(m["dropped_gates"] for m in entry["summary"]["moments"]):
        source = entry["source_path"].replace("\\", "/").rsplit("/", 1)[-1]
        if f"{source}.unwrapped" in index:
            # A gzip source (a cached download): read unwrapped, as above.
            source = f"{source}.unwrapped"
    site, rsl_status, _ = index[rsl_name]
    radx_status = index[radx_name][2]
    if site != entry["summary"]["icao"]:
        note(f"{label}: RSL has no site table entry for {entry['summary']['icao']}; read as {site}")
    if rsl_status != 0:
        fail(f"{label}: RSL fails: {external_error(directory, 'rsl', rsl_name)}")
    else:
        written = read_rsl(f"{directory}/rsl/{rsl_name}.bin")
        check_rsl(label, written, metpy_out)
        if source is not None and relabelled:
            # RSL takes an AR2V0001 file for Message 1 radials.
            note(f"{label}: RSL reads the source's Message 31 radials as Message 1 (AR2V0001 header); not compared")
        elif source is not None:
            if index[source][1] != 0:
                note(f"{label}: RSL cannot read the source: {external_error(directory, 'rsl', source)}")
            else:
                check_rsl_source(label, read_rsl(f"{directory}/rsl/{source}.bin"), written)
    if radx_status != 0:
        fail(f"{label}: RadxConvert fails: {external_error(directory, 'radx', radx_name)}")
    else:
        check_radx(label, read_radx(f"{directory}/radx/{radx_name}.nc"), metpy_out)
        if source is not None:
            if index[source][2] != 0:
                note(f"{label}: RadxConvert cannot read the source: {external_error(directory, 'radx', source)}")
            else:
                check_radx_source(label, f"{directory}/radx/{source}.nc", f"{directory}/radx/{radx_name}.nc", replaced)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("manifest")
    parser.add_argument("--nexrad-crate", help="tools/level2_writer_nexrad_crate executable")
    parser.add_argument("--external", help="output directory of tools/level2_writer_external/run.sh")
    args = parser.parse_args()
    external = external_readings(args.external) if args.external else None
    print(
        f"Py-ART {pyart.__version__}, MetPy {metpy.__version__}, xradar {xradar.__version__}, "
        f"h5py {h5py.__version__}, netCDF4 {netCDF4.__version__}, numpy {np.__version__}"
    )
    with open(args.manifest, encoding="utf-8") as handle:
        manifest = json.load(handle)
    # Sources whose compressed files xradar cannot read, and sources with an
    # uncompressed file xradar reads in full.
    needs_uncompressed = {}
    xradar_in_full = set()
    crate = {}
    if args.nexrad_crate:
        paths = [entry["output"] for entry in manifest["outputs"]]
        paths += [entry["source_path"] for entry in manifest["outputs"] if entry["source_format"] == "nexrad-level2"]
        crate = nexrad_crate(args.nexrad_crate, sorted(set(paths)))
    for entry in manifest["outputs"]:
        path = entry["output"]
        label = path.replace("\\", "/").rsplit("/", 1)[-1]
        before = len(failures)
        summary = entry["summary"]
        try:
            metpy_out = read_metpy(path)
        except Exception as error:  # noqa: BLE001 - report any reader failure
            fail(f"{label}: MetPy failed: {type(error).__name__}: {error}")
            continue
        try:
            pyart_out, location = read_pyart(path)
        except ValueError as error:
            if "Gate spacing is neither" not in str(error):
                fail(f"{label}: Py-ART failed: {error}")
                continue
            # Py-ART puts every moment of a volume on one range and can only
            # widen gates by 2 or 4 from a common first gate.
            note(f"{label}: Py-ART cannot put moments with other first gates on one range: {error}")
            pyart_out, location = None, None
        except Exception as error:  # noqa: BLE001
            fail(f"{label}: Py-ART failed: {type(error).__name__}: {error}")
            continue
        if entry["variant"].endswith("gzip"):
            # xradar 0.12 does not unwrap gzip (it fails on NOAA's gzip
            # archives too); read the file it wraps.
            import gzip as gzip_module
            import tempfile

            with gzip_module.open(path, "rb") as handle, tempfile.NamedTemporaryFile(delete=False, suffix=".ar2v") as inner:
                inner.write(handle.read())
            note(f"{label}: xradar 0.12 does not read gzip files; reading the file the gzip wraps")
            xradar_path = inner.name
        else:
            xradar_path = path
        radials = [len(sweep["azimuth"]) for sweep in metpy_out]
        aligned = all(sum(radials[:index]) % 120 == 0 for index in range(len(radials)))
        compressed = entry["variant"] not in ("none", "gzip")
        # xradar 0.12 addresses the messages of LDM records as if every
        # record after the metadata held 120 of them (message n in record
        # (n - 134) // 120 + 1), and finds a message inside a record by
        # walking from the one it read last, so it reads a compressed sweep
        # right only when the sweep starts a record and every record before
        # it is full: only cuts that are multiples of 120 radials (NOAA's
        # 360 and 720) give both. Records running across cuts (the default)
        # fail on the data of a sweep that starts inside a record; records
        # ending with each cut stop its header pass after the first cut. It
        # reads uncompressed files by file position, whatever the cuts: an
        # uncompressed variant of the same source must read in full.
        unreadable = None
        try:
            xradar_out = read_xradar(xradar_path)
        except (IndexError, EOFError) as error:
            if aligned or not compressed:
                fail(f"{label}: xradar failed: {type(error).__name__}: {error}")
                continue
            unreadable = f"fails on the data of sweeps that start inside an LDM record ({type(error).__name__})"
            xradar_out = None
        except Exception as error:  # noqa: BLE001
            fail(f"{label}: xradar failed: {type(error).__name__}: {error}")
            continue
        if xradar_out is not None and not aligned and compressed and len(xradar_out) < summary["sweeps"]:
            unreadable = f"reads {len(xradar_out)} of {summary['sweeps']} sweeps"
            xradar_out = None
        if unreadable is not None:
            note(
                f"{label}: xradar 0.12 {unreadable} of a compressed file whose cuts are not multiples of "
                f"120 radials ({radials}); an uncompressed variant must read in full"
            )
            needs_uncompressed.setdefault(entry["source_id"], []).append(label)
        xradar_agreement = xradar_out
        if xradar_out is not None and entry["source_format"] == "nexrad-level2":
            rays_out = [len(sweep["azimuth"]) for sweep in xradar_out]
            if sum(rays_out) != summary["radials"]:
                # xradar 0.12 counts every message, a mid-volume Message 2
                # among them, towards a record's 120, and so loses as many
                # radials; NOAA's own files lose them too. When it reads the
                # source the same way, the written file is as NOAA's is to
                # it: its readings of both are compared in
                # check_nexrad_source, not its counts.
                try:
                    source_rays = [len(sweep["azimuth"]) for sweep in read_xradar(entry["source_path"])]
                except Exception:  # noqa: BLE001 - any reader failure
                    source_rays = None
                if source_rays == rays_out:
                    note(
                        f"{label}: xradar 0.12 reads {sum(rays_out)} of {summary['radials']} radials, as it reads "
                        "the source (it counts mid-volume messages among a record's 120)"
                    )
                    xradar_agreement = None
        counts = [len(metpy_out)] + [len(sweeps) for sweeps in (xradar_out, pyart_out) if sweeps is not None]
        if counts != [summary["sweeps"]] * len(counts):
            fail(f"{label}: sweeps MetPy/xradar/Py-ART {counts}, written {summary['sweeps']}")
            continue
        for reader, sweeps in (("MetPy", metpy_out), ("xradar", xradar_agreement or []), ("Py-ART", pyart_out or [])):
            radials = sum(len(sweep["azimuth"]) for sweep in sweeps)
            if sweeps and radials != summary["radials"]:
                fail(f"{label}: {reader} {radials} radials, written {summary['radials']}")
        if (
            not compressed
            and xradar_agreement is not None
            and sum(len(sweep["azimuth"]) for sweep in xradar_agreement) == summary["radials"]
        ):
            # Every sweep and radial; its values are checked below.
            xradar_in_full.add(entry["source_id"])
        by_source = moments_by_sweep(summary)
        nsource = summary["sweeps"] + len(summary["skipped_sweeps"])
        reports = {
            index: by_source.get(source, {})
            for index, source in enumerate(written_sweeps(summary, nsource))
        }
        check_readers_agree(label, pyart_out, metpy_out, xradar_agreement, reports)
        fmt = entry["source_format"]
        if crate:
            source_reading = None
            if fmt == "nexrad-level2" and not any(m["dropped_gates"] for m in summary["moments"]):
                source_reading = crate[entry["source_path"].replace("\\", "/")]
            check_nexrad_crate(label, crate[path.replace("\\", "/")], metpy_out, source_reading)
        if external is not None:
            check_external(label, entry, metpy_out, args.external, external)
        if fmt == "nexrad-level2":
            check_nexrad_source(label, entry, pyart_out, metpy_out, xradar_out, location)
        elif fmt == "odim-h5":
            check_reference(label, entry, odim_reference(entry["source_path"]), metpy_out, pyart_out)
        elif fmt == "cfradial1":
            check_reference(label, entry, cfradial_reference(entry["source_path"]), metpy_out, pyart_out)
        elif fmt == "jma-grib2-tar" and entry["source_path"]:
            check_reference(label, entry, jma_reference(entry["source_path"]), metpy_out, pyart_out)
        elif fmt == "dorade" and args.external:
            reference = radx_reference(label, entry, args.external)
            if reference is not None:
                check_reference(label, entry, reference, metpy_out, pyart_out)
        elif fmt in ("jma-grib2-tar", "dorade"):
            note(f"{label}: no independent reading of the source ({fmt}); readers' agreement only")
        status = "ok" if len(failures) == before else "FAILED"
        print(f"{status:6} {label}: {summary['sweeps']} sweeps, {summary['radials']} radials ({fmt})")
    for source_id, labels in sorted(needs_uncompressed.items()):
        if source_id not in xradar_in_full:
            fail(f"{', '.join(labels)}: xradar reads no uncompressed variant of {source_id} in full")
    print(f"{len(failures)} failure(s), {len(notes)} note(s)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
