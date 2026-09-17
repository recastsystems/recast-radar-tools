#!/usr/bin/env python3
"""Golden values for the io-formats real-data tests (plan stream C, group io-formats).

Every value printed here comes from a reader that does not share code with the
Rust crates under test:

- CfRadial: netCDF4-python (raw variables, no mask/scale), Py-ART
  ``read_cfradial`` and xradar ``open_cfradial1_datatree``.
- ODIM_H5 / HDF5: h5py (attributes, raw planes, object-header addresses and
  header info from ``h5py.h5o.get_info``), xradar ``open_odim_datatree``, and a
  small HDF5 version-1 object-header / B-tree reader written from the HDF5 file
  format specification (section III.A/III.B, IV.A.1) for byte offsets h5py does
  not expose.
- DORADE: a block walker and HRD run-length decoder written from the DORADE
  format document (Oye and Case 1995, lrose-core ``DoradeData.hh`` offsets).
- JMA: a GRIB2 section walker and DRT 5.200 run-length decoder written from the
  JMA GRIB2 radar template documentation (templates 3.50120, 4.51022, 5.200).
- Router / NEXRAD Level II: Py-ART ``read_nexrad_archive`` and MetPy
  ``Level2File`` sweep and radial counts; the NCI THREDDS zip response is
  unwrapped with ``struct`` + ``zlib`` from the PKWARE APPNOTE local-header
  layout and opened with h5py.

Run with the venv that has arm_pyart 2.2.5, metpy 1.7.1, xradar 0.12, h5py and
netCDF4:

    python tools/golden_io_formats.py [--section NAME ...] [--json out.json] [--no-download]

Files come from the committed corpus (testdata/files/...) or the shared
download cache (the same lookup as recast-radar-testdata). The Rust tests quote
these values; the key of each value is named in the test comments.
"""

import argparse
import gzip
import hashlib
import io
import json
import math
import os
import struct
import sys
import tomllib
import urllib.request
import warnings
import zlib
from pathlib import Path

import numpy as np

warnings.filterwarnings("ignore")

ROOT = Path(__file__).resolve().parent.parent
TESTDATA = ROOT / "testdata"


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
ALLOW_DOWNLOAD = True


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
        rel = entry["committed"].removeprefix("testdata/")
        path = TESTDATA / rel
    else:
        path = cache_dir() / entry_id
        if not path.is_file():
            if not ALLOW_DOWNLOAD:
                raise FileNotFoundError(f"{entry_id} is not cached at {path}")
            path.parent.mkdir(parents=True, exist_ok=True)
            with urllib.request.urlopen(entry["urls"][0], timeout=600) as r:
                data = r.read()
            path.write_bytes(data)
    data = path.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    if digest != entry["sha256"] or len(data) != entry["size"]:
        raise ValueError(f"{entry_id}: sha256/size mismatch ({digest}, {len(data)})")
    return path


def corpus_bytes(entry_id):
    return corpus_path(entry_id).read_bytes()


def f32(x):
    """Round-trip a float through float32 (what the Rust side stores)."""
    return float(np.float32(x))


def masked_value(arr, ray, gate):
    value = arr[ray, gate]
    return None if np.ma.is_masked(value) else float(value)


# ---------------------------------------------------------------- CfRadial ---

def cfradial():
    import netCDF4
    import pyart
    import xradar

    out = {}

    # --- Irene SMART-R2, classic CfRadial 1.3, int8 packed DBZ/VEL.
    irene_id = "cfrad1-irene-sr2-20110827-120420-sur-sweeps01"
    path = str(corpus_path(irene_id))
    nc = netCDF4.Dataset(path)
    nc.set_auto_maskandscale(False)
    raw = corpus_bytes(irene_id)
    radar = pyart.io.read_cfradial(path)
    tree = xradar.io.open_cfradial1_datatree(path)
    dbz = radar.fields["DBZ"]["data"]
    vel = radar.fields["VEL"]["data"]
    starts = [int(x) for x in radar.sweep_start_ray_index["data"]]
    ends = [int(x) for x in radar.sweep_end_ray_index["data"]]
    rng = nc.variables["range"][:]
    samples = [(0, 0), (0, 100), (10, 200), (180, 50), (359, 1106), (360, 10), (500, 300), (718, 700)]
    out["irene"] = {
        "magic": raw[:4].hex(),
        "numrecs_field": struct.unpack(">I", raw[4:8])[0],
        "dims": [(name, len(dim), dim.isunlimited()) for name, dim in nc.dimensions.items()],
        "instrument_name": nc.getncattr("instrument_name"),
        "site_name": nc.getncattr("site_name"),
        "version": nc.getncattr("version"),
        "time_coverage_start": nc.getncattr("time_coverage_start"),
        "latitude": float(radar.latitude["data"][0]),
        "longitude": float(radar.longitude["data"][0]),
        "altitude": float(radar.altitude["data"][0]),
        "sweep_mode": [b"".join(row).decode().strip("\x00 ") for row in nc.variables["sweep_mode"][:]],
        "fixed_angle": [f32(a) for a in radar.fixed_angle["data"]],
        "sweep_start_ray_index": starts,
        "sweep_end_ray_index": ends,
        "xradar_sweep_sizes": [dict(tree[f"sweep_{i}"].ds.sizes) for i in range(2)],
        "xradar_fixed_angle": [float(tree[f"sweep_{i}"].ds.sweep_fixed_angle) for i in range(2)],
        "nrays": int(radar.nrays),
        "ngates": int(radar.ngates),
        "range_first_two": [float(rng[0]), float(rng[1])],
        "has_pulse_count": "pulse_count" in nc.variables,
        "has_independent_samples": "independent_samples" in nc.variables,
        "ray": {
            str(ray): {
                "azimuth": f32(nc.variables["azimuth"][ray]),
                "elevation": f32(nc.variables["elevation"][ray]),
                "time_s": float(nc.variables["time"][ray]),
                "prt_s": f32(nc.variables["prt"][ray]),
                "unambiguous_range_m": f32(nc.variables["unambiguous_range"][ray]),
                "nyquist": f32(nc.variables["nyquist_velocity"][ray]),
                "n_samples": int(nc.variables["n_samples"][ray]),
            }
            for ray in (0, 3, 359, 360, 718)
        },
        "dbz_attrs": {k: float(nc.variables["DBZ"].getncattr(k)) for k in ("scale_factor", "add_offset", "_FillValue")},
        "vel_attrs": {k: float(nc.variables["VEL"].getncattr(k)) for k in ("scale_factor", "add_offset", "_FillValue")},
        "dbz_raw": {f"{r},{g}": int(nc.variables["DBZ"][r, g]) for r, g in samples},
        "dbz_pyart": {f"{r},{g}": masked_value(dbz, r, g) for r, g in samples},
        "vel_pyart": {f"{r},{g}": masked_value(vel, r, g) for r, g in samples},
        "dbz_valid_per_sweep": [int(dbz[s:e + 1].count()) for s, e in zip(starts, ends)],
        "vel_valid_per_sweep": [int(vel[s:e + 1].count()) for s, e in zip(starts, ends)],
        "dbz_max": [float(dbz.max()), [int(i) for i in np.unravel_index(dbz.argmax(), dbz.shape)]],
    }

    # --- X-SAPR classic conversion: UNLIMITED time (record variables).
    xsapr_id = "cfrad1-xsapr-sgp-20110520-ppi-classic"
    path = str(corpus_path(xsapr_id))
    nc = netCDF4.Dataset(path)
    nc.set_auto_maskandscale(False)
    raw = corpus_bytes(xsapr_id)
    refl = nc.variables["reflectivity_horizontal"]
    out["xsapr_classic"] = {
        "magic": raw[:4].hex(),
        "numrecs_field": struct.unpack(">I", raw[4:8])[0],
        "dim_list_tag": struct.unpack(">I", raw[8:12])[0],
        "dim_count": struct.unpack(">I", raw[12:16])[0],
        "dims": [(name, len(dim), dim.isunlimited()) for name, dim in nc.dimensions.items()],
        "instrument_name": nc.getncattr("instrument_name"),
        "refl_dtype": str(refl.dtype),
        "refl_dims": list(refl.dimensions),
        "refl_attrs": {k: (float(v) if np.ndim(v) == 0 and not isinstance(v, str) else v)
                       for k, v in ((k, refl.getncattr(k)) for k in refl.ncattrs())},
        "refl_raw": {f"{r},{g}": f32(refl[r, g]) for r, g in [(0, 0), (0, 21), (10, 14), (39, 41)]},
        "refl_fill_count": int((refl[:] == -9999.0).sum()),
        "refl_fill_first": [int(i) for i in np.argwhere(refl[:] == -9999.0)[0]],
        "ray": {
            str(ray): {
                "prt_s": f32(nc.variables["prt"][ray]),
                "unambiguous_range_m": f32(nc.variables["unambiguous_range"][ray]),
                "nyquist": f32(nc.variables["nyquist_velocity"][ray]),
                "time_s": float(nc.variables["time"][ray]),
            }
            for ray in (0, 3, 39)
        },
        "prt_is_record_variable": nc.variables["prt"].dimensions == ("time",)
        and nc.dimensions["time"].isunlimited(),
    }

    netcdf4_raw = corpus_bytes("cfrad1-xsapr-sgp-20110520-ppi-netcdf4")
    out["xsapr_netcdf4_magic"] = netcdf4_raw[:8].hex()
    return out


# -------------------------------------------------------------------- ODIM ---

def h5_attrs(group):
    result = {}
    for key, value in group.attrs.items():
        if isinstance(value, (bytes, np.bytes_)):
            result[key] = value.decode()
        elif np.ndim(value) == 0:
            result[key] = value.item()
        else:
            result[key] = np.asarray(value).tolist()
    return result


def lookup3(data, initval=0):
    """Bob Jenkins lookup3 hashlittle (the HDF5 metadata checksum)."""
    mask = 0xFFFFFFFF

    def rot(x, k):
        return ((x << k) | (x >> (32 - k))) & mask

    length = len(data)
    a = b = c = (0xDEADBEEF + length + initval) & mask
    i = 0
    while length > 12:
        a = (a + int.from_bytes(data[i:i + 4], "little")) & mask
        b = (b + int.from_bytes(data[i + 4:i + 8], "little")) & mask
        c = (c + int.from_bytes(data[i + 8:i + 12], "little")) & mask
        a = (a - c) & mask; a ^= rot(c, 4); c = (c + b) & mask
        b = (b - a) & mask; b ^= rot(a, 6); a = (a + c) & mask
        c = (c - b) & mask; c ^= rot(b, 8); b = (b + a) & mask
        a = (a - c) & mask; a ^= rot(c, 16); c = (c + b) & mask
        b = (b - a) & mask; b ^= rot(a, 19); a = (a + c) & mask
        c = (c - b) & mask; c ^= rot(b, 4); b = (b + a) & mask
        i += 12
        length -= 12
    if length == 0:
        return c
    tail = data[i:i + length] + b"\0" * (12 - length)
    a = (a + int.from_bytes(tail[0:4], "little")) & mask
    b = (b + int.from_bytes(tail[4:8], "little")) & mask
    c = (c + int.from_bytes(tail[8:12], "little")) & mask
    c ^= b; c = (c - rot(b, 14)) & mask
    a ^= c; a = (a - rot(c, 11)) & mask
    b ^= a; b = (b - rot(a, 25)) & mask
    c ^= b; c = (c - rot(b, 16)) & mask
    a ^= c; a = (a - rot(c, 4)) & mask
    b ^= a; b = (b - rot(a, 14)) & mask
    c ^= b; c = (c - rot(b, 24)) & mask
    return c


def v1_header_messages(raw, address):
    """(type, body_offset, size) of every message of a version-1 object header
    (HDF5 spec IV.A.1.a), following continuation messages (type 0x0010)."""
    version, _, nmesgs, _, size = struct.unpack("<BBHII", raw[address:address + 12])
    assert version == 1
    blocks = [(address + 16, size)]
    messages = []
    while blocks and len(messages) < nmesgs:
        start, length = blocks.pop(0)
        cursor = start
        while cursor + 8 <= start + length and len(messages) < nmesgs:
            kind, msize = struct.unpack("<HH", raw[cursor:cursor + 4])
            messages.append((kind, cursor + 8, msize))
            if kind == 0x0010:
                offset, clen = struct.unpack("<QQ", raw[cursor + 8:cursor + 24])
                blocks.append((offset, clen))
            cursor += 8 + msize
    return messages


def v2_chunk0_span(raw, address):
    """(start, end_exclusive_of_checksum) of chunk 0 of a v2 object header."""
    assert raw[address:address + 4] == b"OHDR"
    flags = raw[address + 5]
    cursor = address + 6
    if flags & 0x20:
        cursor += 16
    if flags & 0x10:
        cursor += 4
    size_len = 1 << (flags & 0x03)
    chunk_size = int.from_bytes(raw[cursor:cursor + size_len], "little")
    cursor += size_len
    return address, cursor + chunk_size


def odim():
    import h5py
    import xradar

    out = {}

    # --- iesha PVOL (real replacement for odim_pvol_synth.h5).
    iesha_id = "odim-iesha-20260305-0115-pvol"
    path = corpus_path(iesha_id)
    h5 = h5py.File(path, "r")
    datasets = sorted((k for k in h5 if k.startswith("dataset")), key=lambda s: int(s[7:]))
    sweeps = []
    for name in datasets:
        group = h5[name]
        where = h5_attrs(group["where"])
        how = h5_attrs(group["how"]) if "how" in group else {}
        planes = {}
        for plane in sorted((k for k in group if k.startswith("data")), key=lambda s: int(s[4:])):
            what = h5_attrs(group[plane]["what"])
            data = group[plane]["data"][()]
            gain, offset = what["gain"], what["offset"]
            valid = (data != what["nodata"]) & (data != what["undetect"])
            probes = {}
            for ray, gate in [(0, 0), (45, 10), (90, 50), (180, min(100, data.shape[1] - 1)), (270, 20), (359, data.shape[1] - 1)]:
                code = int(data[ray, gate])
                probes[f"{ray},{gate}"] = [code, None if code in (what["nodata"], what["undetect"]) else f32(gain * code + offset)]
            phys = np.where(valid, gain * data.astype(np.float64) + offset, np.nan)
            planes[what["quantity"]] = {
                "gain": gain, "offset": offset, "nodata": what["nodata"], "undetect": what["undetect"],
                "dtype": data.dtype.str, "shape": list(data.shape), "valid": int(valid.sum()),
                "max": f32(np.nanmax(phys)) if valid.any() else None,
                "probes": probes,
            }
        sweeps.append({
            "dataset": name, "elangle": where["elangle"], "nbins": where["nbins"], "nrays": where["nrays"],
            "rstart": where["rstart"], "rscale": where["rscale"], "NI": how.get("NI"), "planes": planes,
        })
    tree = xradar.io.open_odim_datatree(str(path))
    xr_sweeps = sorted(
        [(float(tree[k].ds.sweep_fixed_angle), dict(tree[k].ds.sizes), float(tree[k].ds.range.values[0]),
          float(tree[k].ds.range.values[1] - tree[k].ds.range.values[0]),
          [float(a) for a in tree[k].ds.azimuth.values[:2]])
         for k in tree.children if k.startswith("sweep_")])
    out["iesha"] = {
        "what": h5_attrs(h5["what"]), "where": h5_attrs(h5["where"]),
        "how_wavelength_cm": h5_attrs(h5["how"]).get("wavelength"),
        "frequency_mhz_from_wavelength": round(299.792458 / (h5_attrs(h5["how"])["wavelength"] / 100.0)),
        "sweeps": sweeps,
        "xradar_sweeps": xr_sweeps,
    }

    # --- `how` metadata (ODIM_H5 v2.4 Table 8) of four PVOLs: the site
    # constants of the root `how` and of the first dataset's, and per dataset
    # its constants, every attribute that is not a per-ray array (name and
    # value), and the names of the per-ray arrays.
    typed = ("beamwH", "beamwV", "beamwidth", "antgainH", "antgainV", "RXbandwidth",
             "radconstH", "radconstV", "rpm", "antspeed", "pulsewidth")
    constants = {}
    for key, entry in (("iesha", "odim-iesha-20260305-0115-pvol"), ("dkrom", "odim-dkrom-20260820-1130-pvol"),
                       ("espdg", "odim-espdg-20260707-1927-pvol-dbzh-vradh"), ("norst", "odim-norst-20170421-0908-pvol")):
        h5 = h5py.File(corpus_path(entry), "r")
        datasets = sorted((k for k in h5 if k.startswith("dataset")), key=lambda s: int(s[7:]))
        root = h5_attrs(h5["how"]) if "how" in h5 else {}
        first = h5_attrs(h5[datasets[0]]["how"]) if "how" in h5[datasets[0]] else {}
        per_dataset = []
        for name in datasets:
            how = h5_attrs(h5[name]["how"]) if "how" in h5[name] else {}
            nrays = h5_attrs(h5[name]["where"])["nrays"]
            per_ray = sorted(k for k, v in how.items() if isinstance(v, list) and len(v) == nrays)
            per_dataset.append({
                "constants": {k: how[k] for k in typed if k in how},
                "attrs": {k: v for k, v in how.items() if k not in per_ray},
                "per_ray": per_ray,
            })
        constants[key] = {
            "id": entry,
            "root": {k: root[k] for k in typed if k in root},
            "root_attrs": root,
            "first_dataset": {k: first[k] for k in typed if k in first},
            "datasets": per_dataset,
        }
    out["how_constants"] = constants

    # --- espdg: copied what-group sentinels on VRADH (float64 planes).
    espdg_id = "odim-espdg-20260707-1927-pvol-dbzh-vradh"
    path = corpus_path(espdg_id)
    raw = corpus_bytes(espdg_id)
    h5 = h5py.File(path, "r")
    recovery = {}
    for name in ("dataset1", "dataset2"):
        group = h5[name]
        planes = {h5_attrs(group[p]["what"])["quantity"]: p for p in group if p.startswith("data")}
        zw = h5_attrs(group[planes["DBZH"]]["what"])
        vw = h5_attrs(group[planes["VRADH"]]["what"])
        z = group[planes["DBZH"]]["data"][()]
        v = group[planes["VRADH"]]["data"][()]
        z_no_echo = (z == zw["nodata"]) | (z == zw["undetect"])
        v_sentinel = (v == vw["nodata"]) | (v == vw["undetect"])
        v_phys = vw["gain"] * v + vw["offset"]
        on_offset = (~v_sentinel) & (np.abs(v_phys - vw["offset"]) <= 1e-6)
        fill = z_no_echo & on_offset
        genuine_zero = (~z_no_echo) & on_offset
        masked_recovered = v_sentinel | fill
        flat = np.flatnonzero(fill.ravel()).astype(np.uint64)
        flat_kept_zero = np.flatnonzero(genuine_zero.ravel()).astype(np.uint64)
        first_fill = np.argwhere(fill)[:3].tolist()
        first_zero = np.argwhere(genuine_zero)[:3].tolist()
        recovery[name] = {
            "elangle": h5_attrs(group["where"])["elangle"],
            "dbzh_what": zw, "vradh_what": vw,
            "shape": list(v.shape),
            "vradh_sentinel_gates": int(v_sentinel.sum()),
            "fill_gates": int(fill.sum()),
            "fill_index_sum": int(flat.sum()),
            "fill_index_square_sum": int((flat * flat).sum()),
            "genuine_zero_gates": int(genuine_zero.sum()),
            "genuine_zero_index_sum": int(flat_kept_zero.sum()),
            "valid_after_recovery": int((~masked_recovered).sum()),
            "valid_without_recovery": int((~v_sentinel).sum()),
            "first_fill_gates": first_fill,
            "first_genuine_zero_gates": first_zero,
        }
    out["espdg_recovery"] = recovery

    # Mutation for the distinct-sentinel case: rewrite dataset2 VRADH
    # what/nodata (95.5) to a value no gate holds, fixing the v2 object-header
    # checksum, and prove libhdf5 reads the edited file.
    new_nodata = -9999.0
    pattern = struct.pack("<d", 95.5)
    candidates = [i for i in range(len(raw)) if raw.startswith(pattern, i)]
    ohdrs = [i for i in range(len(raw)) if raw.startswith(b"OHDR", i)]
    target_path = ("dataset2", "VRADH")
    found = None
    for offset in candidates:
        owner = max(a for a in ohdrs if a < offset)
        start, end = v2_chunk0_span(raw, owner)
        if not (start <= offset < end):
            continue
        edited = bytearray(raw)
        edited[offset:offset + 8] = struct.pack("<d", new_nodata)
        checksum = lookup3(bytes(edited[start:end]))
        edited[end:end + 4] = struct.pack("<I", checksum)
        try:
            test = h5py.File(io.BytesIO(bytes(edited)), "r")
            group = test[target_path[0]]
            for plane in (p for p in group if p.startswith("data")):
                what = h5_attrs(group[plane]["what"])
                if what["quantity"] == target_path[1] and what["nodata"] == new_nodata:
                    v = group[plane]["data"][()]
                    assert not (v == new_nodata).any()
                    found = {
                        "nodata_value_offset": offset, "checksum_offset": end,
                        "original_checksum": struct.unpack("<I", raw[end:end + 4])[0],
                        "original_checksum_recomputed": lookup3(raw[start:end]),
                        "new_nodata": new_nodata, "new_checksum": checksum,
                        "valid_gates_with_distinct_sentinels": int(((v != new_nodata) & (v != what["undetect"])).sum()),
                        "zero_gates_kept": int(((v != new_nodata) & (v != what["undetect"]) & (v == 0.0)).sum()),
                    }
        except Exception:  # noqa: BLE001 - wrong candidate: libhdf5 rejects or reads another attribute
            continue
    out["espdg_distinct_sentinel_mutation"] = found

    # --- dkrom: velocity sentinels equal the DBZH ones on u8 planes (no-op recovery).
    dkrom_id = "odim-dkrom-20260820-1130-pvol"
    h5 = h5py.File(corpus_path(dkrom_id), "r")
    group = h5["dataset1"]
    planes = {h5_attrs(group[p]["what"])["quantity"]: p for p in group if p.startswith("data")}
    vw = h5_attrs(group[planes["VRAD"]]["what"])
    zw = h5_attrs(group[planes["DBZH"]]["what"])
    v = group[planes["VRAD"]]["data"][()]
    out["dkrom_dataset1"] = {
        "elangle": h5_attrs(group["where"])["elangle"],
        "vrad_what": vw, "dbzh_what": zw,
        "vrad_valid": int(((v != vw["nodata"]) & (v != vw["undetect"])).sum()),
        "vrad_probe": {f"{r},{g}": [int(v[r, g]), f32(vw["gain"] * int(v[r, g]) + vw["offset"])]
                       for r, g in [(0, 0), (100, 100), (200, 50)]},
    }

    # --- bejab: HDF5 v1 object header continuation and B-tree node offsets.
    bejab_id = "odim-bejab-20190606-0000-pvol"
    path = corpus_path(bejab_id)
    raw = corpus_bytes(bejab_id)
    h5 = h5py.File(path, "r")
    root_info = h5py.h5o.get_info(h5.id)
    root = root_info.addr
    root_messages = v1_header_messages(raw, root)
    continuation = [m for m in root_messages if m[0] == 0x0010]
    symbol_table = [m for m in root_messages if m[0] == 0x0011]
    group_btree = struct.unpack("<Q", raw[symbol_table[0][1]:symbol_table[0][1] + 8])[0]
    dataset = h5["dataset1/data1/data"]
    dataset_addr = h5py.h5o.get_info(dataset.id).addr
    layout = [m for m in v1_header_messages(raw, dataset_addr) if m[0] == 0x0008][0]
    body = raw[layout[1]:layout[1] + layout[2]]
    assert body[0] == 3 and body[1] == 2
    dimensionality = body[2]
    chunk_btree = struct.unpack("<Q", body[3:11])[0]
    chunk_key = 8 + 8 * dimensionality
    group_child0 = group_btree + 24 + 8
    chunk_child0 = chunk_btree + 24 + chunk_key
    out["bejab_hdf5"] = {
        "superblock_version": raw[8],
        "offset_size": raw[13], "length_size": raw[14],
        "root_header_address": root,
        "root_header_nmesgs": root_info.hdr.nmesgs,
        "root_header_nchunks": root_info.hdr.nchunks,
        "root_messages_excluding_continuation": len([m for m in root_messages if m[0] != 0x0010]),
        "root_first_block_start": root + 16,
        "root_continuation_body_offset": continuation[0][1],
        "root_continuation_target": struct.unpack("<Q", raw[continuation[0][1]:continuation[0][1] + 8])[0],
        "group_btree_address": group_btree,
        "group_btree_node": list(raw[group_btree:group_btree + 8]),
        "group_btree_level_offset": group_btree + 5,
        "group_btree_child0_offset": group_child0,
        "group_btree_child0_signature": raw[struct.unpack("<Q", raw[group_child0:group_child0 + 8])[0]:][:4].decode(),
        "chunk_btree_address": chunk_btree,
        "chunk_btree_node": list(raw[chunk_btree:chunk_btree + 8]),
        "chunk_key_dims": dimensionality,
        "chunk_btree_level_offset": chunk_btree + 5,
        "chunk_btree_child0_offset": chunk_child0,
        "chunk_btree_child0": struct.unpack("<Q", raw[chunk_child0:chunk_child0 + 8])[0],
        "h5py_chunk_byte_offset": dataset.id.get_chunk_info(0).byte_offset,
        "h5py_chunk_size": dataset.id.get_chunk_info(0).size,
        "root_children": len(h5),
        "h5py_chunk_count": dataset.id.get_num_chunks(),
    }
    out["signatures"] = {
        "bejab_first8": raw[:8].hex(),
        "cfradial_classic_first8": corpus_bytes("cfrad1-xsapr-sgp-20110520-ppi-classic")[:8].hex(),
        "level2_trim_first8": corpus_bytes("l2-ktlx-20240315-000217-trim")[:8].hex(),
        "netcdf4_superblock_version": corpus_bytes("cfrad1-xsapr-sgp-20110520-ppi-netcdf4")[8],
        "imgw_kdp_object": h5_attrs(h5py.File(corpus_path("odim-imgw-ram-20260711-0015-kdp-max"), "r")["what"])["object"],
    }
    return out


# ------------------------------------------------------------------ DORADE ---

def dorade_walk(data):
    le = struct.unpack("<i", data[4:8])[0]
    be = struct.unpack(">i", data[4:8])[0]
    le_ok = 8 <= le <= len(data)
    be_ok = 8 <= be <= len(data)
    endian = "<" if le_ok and not be_ok else ">"
    blocks = []
    pos = 0
    while pos + 8 <= len(data):
        ident = data[pos:pos + 4].decode("latin1")
        size = struct.unpack(endian + "i", data[pos + 4:pos + 8])[0]
        if size < 8 or pos + size > len(data):
            break
        blocks.append((ident, pos, size))
        pos += size
    return endian, blocks, pos


def hrd_rle(words, gates, bad):
    out = []
    i = 0
    while i < len(words) and len(out) < gates:
        word = words[i] & 0xFFFF
        if word == 1 or word == 0:
            break
        count = word & 0x7FFF
        if word & 0x8000:
            out.extend(words[i + 1:i + 1 + count])
            i += 1 + count
        else:
            out.extend([bad] * count)
            i += 1
    out.extend([bad] * (gates - len(out)))
    return out[:gates]


def dorade_sweep(entry_id=None, data=None, probe_rays=(), probe_gates=()):
    if data is None:
        data = corpus_bytes(entry_id)
    e, blocks, end = dorade_walk(data)
    info = {"endian": "little" if e == "<" else "big", "block_bytes": end, "file_bytes": len(data)}
    params = []
    rays = []
    current = None
    gates = None
    for ident, pos, size in blocks:
        block = data[pos:pos + size]
        if ident == "SSWB":
            info["sswb_start_unix"] = struct.unpack(e + "i", block[12:16])[0]
            info["sswb_stop_unix"] = struct.unpack(e + "i", block[16:20])[0]
        elif ident == "VOLD":
            info["vold_volume_number"] = struct.unpack(e + "h", block[10:12])[0]
            info["vold_date"] = list(struct.unpack(e + "hhhhhh", block[36:48]))
            text = lambda raw: raw.split(b"\0")[0].decode("latin1").strip()
            info["vold_text"] = {"proj_name": text(block[16:36]), "flight_num": text(block[48:56]),
                                 "gen_facility": text(block[56:64])}
        elif ident == "RADD":
            info["radar_name"] = block[8:16].split(b"\0")[0].decode("latin1").strip()
            info["scan_mode"] = struct.unpack(e + "h", block[50:52])[0]
            info["data_compress"] = struct.unpack(e + "h", block[68:70])[0]
            info["longitude"], info["latitude"], info["altitude_km"], info["eff_unamb_vel"] = \
                struct.unpack(e + "ffff", block[80:96])
            info["radd_bytes"] = size
            names = ("radar_const", "peak_power", "noise_power", "receiver_gain", "antenna_gain",
                     "system_gain", "horz_beam_width", "vert_beam_width")
            info["radd_constants"] = dict(zip(names, struct.unpack(e + "8f", block[16:48])))
            info["radd_constants"]["req_rotat_vel"] = struct.unpack(e + "f", block[52:56])[0]
            info["radd_constants"]["eff_unamb_range"] = struct.unpack(e + "f", block[96:100])[0]
        elif ident == "PARM":
            name = block[8:16].split(b"\0")[0].decode("latin1").strip()
            fmt = struct.unpack(e + "h", block[78:80])[0]
            scale, bias = struct.unpack(e + "ff", block[92:100])
            bad = struct.unpack(e + "i", block[100:104])[0]
            cells = struct.unpack(e + "i", block[200:204])[0] if size >= 212 else None
            description = block[16:56].split(b"\0")[0].decode("latin1").strip()
            units = block[56:64].split(b"\0")[0].decode("latin1").strip()
            bandwidth = struct.unpack(e + "f", block[68:72])[0]
            pulse_width, polarization, samples = struct.unpack(e + "hhh", block[72:78])
            params.append({"name": name, "binary_format": fmt, "scale": scale, "bias": bias,
                           "bad_data": bad, "number_cells": cells, "offset": pos, "size": size,
                           "description": description, "units": units, "recvr_bandwidth": bandwidth,
                           "pulse_width": pulse_width, "num_samples": samples})
        elif ident == "CFAC":
            names = ("azimuth", "elevation", "range_delay_m", "longitude", "latitude", "pressure_alt_km", "radar_alt_km")
            info["cfac"] = dict(zip(names, struct.unpack(e + "7f", block[8:36])))
        elif ident == "CELV":
            count = struct.unpack(e + "i", block[8:12])[0]
            first, second = struct.unpack(e + "ff", block[12:20])
            info["celv"] = {"cells": count, "first_m": first, "spacing_m": second - first}
            gates = count
        elif ident == "CSFD":
            segments = struct.unpack(e + "i", block[8:12])[0]
            first, spacing = struct.unpack(e + "ff", block[12:20])
            cells = struct.unpack(e + "8h", block[48:64])
            info["csfd"] = {"segments": segments, "first_m": first, "spacing_m": spacing,
                            "cells": sum(cells[:segments])}
            gates = sum(cells[:segments])
        elif ident == "SWIB":
            info["sweep_number"] = struct.unpack(e + "i", block[16:20])[0]
            info["swib_num_rays"] = struct.unpack(e + "i", block[20:24])[0]
            info["fixed_angle"] = struct.unpack(e + "f", block[32:36])[0]
        elif ident == "RYIB":
            sweep, jday, hh, mm, ss, ms = struct.unpack(e + "iihhhh", block[8:24])
            az, el, peak_power, true_scan_rate = struct.unpack(e + "ffff", block[24:40])
            status = struct.unpack(e + "i", block[40:44])[0]
            current = {"offset": pos, "julian_day": jday, "time": [hh, mm, ss, ms], "azimuth": az,
                       "elevation": el, "peak_power": peak_power, "true_scan_rate": true_scan_rate,
                       "status": status, "fields": {}}
            rays.append(current)
        elif ident == "RDAT" and current is not None:
            name = block[8:16].split(b"\0")[0].decode("latin1").strip()
            param = next(p for p in params if p["name"] == name)
            payload = block[16:]
            if param["binary_format"] == 2:
                words = list(struct.unpack(e + f"{len(payload) // 2}h", payload[:len(payload) // 2 * 2]))
                n = gates if gates is not None else param["number_cells"]
                if info["data_compress"] == 1:
                    words = hrd_rle(words, n, param["bad_data"])
                else:
                    words = words[:n]
                current["fields"][name] = words
    info["params"] = [{k: v for k, v in p.items()} for p in params]
    info["gates"] = gates
    info["ray_count"] = len(rays)
    info["ray_status"] = [r["status"] for r in rays]
    info["first_ray_offset"] = rays[0]["offset"] if rays else None
    info["ray_offsets"] = [r["offset"] for r in rays]
    kept = [r for r in rays if r["status"] == 0] or rays
    info["kept_ray_count"] = len(kept)
    # Distinct RYIB peak_power (kW) and true_scan_rate (deg/s) of the kept
    # rays; -999, -9999 and -32768 are missing values.
    info["ryib_peak_power"] = sorted({r["peak_power"] for r in kept})
    info["ryib_true_scan_rate"] = sorted({r["true_scan_rate"] for r in kept})

    def ms_of_day(t):
        return ((t[0] * 60 + t[1]) * 60 + t[2]) * 1000 + t[3]

    import datetime
    start = datetime.datetime.fromtimestamp(info["sswb_start_unix"], datetime.timezone.utc)
    info["sswb_start_iso"] = start.strftime("%Y-%m-%dT%H:%M:%SZ")
    start_ms = ms_of_day([start.hour, start.minute, start.second, 0])
    cfac = info.get("cfac", {})
    info["kept"] = []
    for index, ray in enumerate(kept):
        entry = {"azimuth": ray["azimuth"] + cfac.get("azimuth", 0.0),
                 "elevation": ray["elevation"] + cfac.get("elevation", 0.0),
                 "time_offset_ms": ms_of_day(ray["time"]) - start_ms, "julian_day": ray["julian_day"],
                 "peak_power": ray["peak_power"], "true_scan_rate": ray["true_scan_rate"]}
        if index in probe_rays:
            entry["physical"] = {}
            entry["bad_count"] = {}
            for p in params:
                words = ray["fields"].get(p["name"])
                if words is None:
                    continue
                entry["bad_count"][p["name"]] = sum(1 for w in words if w == p["bad_data"])
                entry["physical"][p["name"]] = {
                    str(g): (None if words[g] == p["bad_data"] else (words[g] - p["bias"]) / p["scale"])
                    for g in probe_gates if g < len(words)
                }
        info["kept"].append(entry)
    return info


def dorade():
    out = {}
    out["cow2"] = dorade_sweep("dorade-cow2-20260521-225514-sur-head24", probe_rays=(0, 20), probe_gates=(0, 1, 50, 100, 374))
    out["dow6_rhi"] = dorade_sweep("dorade-dow6-20211230-222139-rhi-head41", probe_rays=(0, 34), probe_gates=(0, 10, 100, 500, 999))
    out["noxp_190244"] = dorade_sweep("dorade-noxp-20090501-190244-ppi")
    out["noxp_190324"] = dorade_sweep("dorade-noxp-20090501-190324-ppi")
    sector = dorade_sweep("dorade-noxp-20090525-203211-sector", probe_rays=tuple(range(100)), probe_gates=tuple(range(0, 1001, 50)))
    best = max(range(len(sector["kept"])), key=lambda i: -sector["kept"][i]["bad_count"].get("DZ", 1001))
    for i, ray in enumerate(sector["kept"]):
        if i not in (0, best, len(sector["kept"]) - 1):
            ray.pop("physical", None)
            ray.pop("bad_count", None)
    sector["best_ray"] = best
    sector["kept"] = {str(i): r for i, r in enumerate(sector["kept"]) if i in (0, 1, best, len(sector["kept"]) - 1)}
    out["noxp_sector"] = sector
    for key in ("noxp_190244", "noxp_190324"):
        for ray in out[key]["kept"]:
            ray.pop("physical", None)
        out[key]["kept"] = out[key]["kept"][:3] + out[key]["kept"][-1:]
    for suffix in ("003210-ppi-head6", "003222-ppi-head6", "003226-ppi-head6"):
        entry_id = f"dorade-noxp-20090610-{suffix}"
        if entry_id in MANIFEST:
            out[f"noxp_0610_{suffix}"] = dorade_sweep(entry_id, probe_rays=(0, 5), probe_gates=(0, 100, 500, 1000))
    for sweep in out.values():
        sweep.pop("ray_offsets", None) if sweep.get("ray_count", 0) > 60 else None
    return out


# --------------------------------------------------------------------- JMA ---

def tar_members(data):
    pos = 0
    members = []
    while pos + 512 <= len(data):
        header = data[pos:pos + 512]
        if header == b"\0" * 512:
            break
        name = header[:100].split(b"\0")[0].decode()
        size = int(header[124:136].split(b"\0")[0].strip() or b"0", 8)
        members.append({"name": name, "header_offset": pos, "data_offset": pos + 512, "size": size,
                        "magic": header[257:263].decode("latin1"), "typeflag": header[156:157].decode("latin1")})
        pos += 512 + (size + 511) // 512 * 512
    return members


def sm16(raw):
    return None if raw == 0xFFFF else (-(raw & 0x7FFF) if raw & 0x8000 else raw)


def sm32(raw):
    return None if raw == 0xFFFFFFFF else (-(raw & 0x7FFFFFFF) if raw & 0x80000000 else raw)


def jma_run_length(data, nbits, max_value, points):
    lngu = (1 << nbits) - 1 - max_value
    out = []
    previous = None
    digits = []

    def flush():
        if previous is None:
            return
        count = 1 + sum(d * lngu ** i for i, d in enumerate(digits))
        out.extend([previous] * count)

    assert nbits == 8
    for value in data:
        if value <= max_value:
            flush()
            previous = value
            digits = []
        else:
            digits.append(value - max_value - 1)
    flush()
    return out[:points]


def jma_member(data, decode_gates=False):
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
    result = {"sections": len(sections), "sweeps": []}
    grid = None
    product = None
    repr5 = None
    for number, offset, length in sections:
        body = msg[offset:offset + length]
        if number == 1:
            year = struct.unpack(">H", body[12:14])[0]
            result["reference_time"] = f"{year:04}-{body[14]:02}-{body[15]:02}T{body[16]:02}:{body[17]:02}:{body[18]:02}Z"
        elif number == 3:
            grid = {
                "section3_offset": offset, "template": struct.unpack(">H", body[12:14])[0],
                "points": struct.unpack(">I", body[6:10])[0],
                "gates": struct.unpack(">I", body[14:18])[0], "radials": struct.unpack(">I", body[18:22])[0],
                "gate_spacing_m": struct.unpack(">I", body[30:34])[0] / 1000.0,
                "range_start_m": struct.unpack(">I", body[34:38])[0] / 1000.0,
                "scan_mode": body[38], "start_azimuth_deg": struct.unpack(">H", body[39:41])[0] / 100.0,
            }
        elif number == 4:
            radials = grid["radials"]
            product = {
                "template": struct.unpack(">H", body[7:9])[0], "category": body[9], "number": body[10],
                "latitude": sm32(struct.unpack(">I", body[14:18])[0]) / 1e6,
                "longitude": struct.unpack(">I", body[18:22])[0] / 1e6,
                "altitude_m": (lambda v: None if v is None else v / 10.0)(sm16(struct.unpack(">H", body[22:24])[0])),
                "station_id": body[24:28].decode("ascii").strip(),
                "station_number": struct.unpack(">H", body[28:30])[0],
                "elevation_deg": (lambda v: None if v is None else v / 100.0)(sm16(struct.unpack(">H", body[41:43])[0])),
                "ray_elevation_deg": [
                    (lambda v: None if v is None else v / 100.0)(sm16(struct.unpack(">H", body[60 + 4 * r:62 + 4 * r])[0]))
                    for r in range(radials)
                ],
            }
        elif number == 5:
            nlevels = struct.unpack(">H", body[14:16])[0]
            repr5 = {
                "points": struct.unpack(">I", body[5:9])[0], "template": struct.unpack(">H", body[9:11])[0],
                "nbits": body[11], "max_value": struct.unpack(">H", body[12:14])[0], "levels": nlevels,
                "decimal_scale": body[16],
                "level_values": [sm16(struct.unpack(">H", body[17 + 2 * i:19 + 2 * i])[0]) for i in range(nlevels)],
            }
        elif number == 7:
            sweep = {"grid": grid, "product": {k: v for k, v in product.items() if k != "ray_elevation_deg"},
                     "ray_elevation_first3": product["ray_elevation_deg"][:3], "repr": {
                         k: v for k, v in repr5.items() if k != "level_values"}}
            if decode_gates:
                levels = jma_run_length(body[5:], repr5["nbits"], repr5["max_value"], grid["points"])
                factor = 10.0 ** (-repr5["decimal_scale"])
                table = [None] + [None if v is None else v * factor for v in repr5["level_values"]]
                values = [table[level] for level in levels]
                gates = grid["gates"]
                sweep["valid_gates"] = sum(1 for v in values if v is not None)
                sweep["gate_probe"] = {}
                for ray, gate in [(0, 0), (0, 50), (100, 20), (256, 100), (511, 299)]:
                    sweep["gate_probe"][f"{ray},{gate}"] = values[ray * gates + gate]
                first_valid = next((i for i, v in enumerate(values) if v is not None), None)
                if first_valid is not None:
                    sweep["first_valid"] = [first_valid // gates, first_valid % gates, values[first_valid]]
            result["sweeps"].append(sweep)
    return result


def jma():
    out = {}
    for key, entry_id in (("n5_rs47773", "jma-n5-20191012-090000-rs47773"),
                          ("n6_rs47773", "jma-n6-20191012-090000-rs47773")):
        data = corpus_bytes(entry_id)
        members = tar_members(data)
        member = members[0]
        info = jma_member(data[member["data_offset"]:member["data_offset"] + member["size"]], decode_gates=True)
        elevations = [s["product"]["elevation_deg"] for s in info["sweeps"]]
        order = sorted(range(len(elevations)), key=lambda i: elevations[i])
        info["tar"] = {"file_bytes": len(data), "members": members}
        info["elevations_scan_order"] = elevations
        info["elevations_sorted"] = [elevations[i] for i in order]
        info["sorted_scan_index"] = order
        out[key] = info
    for key, entry_id in (("n5_full", "jma-n5-20191012-090000"), ("n6_full", "jma-n6-20191012-090000")):
        data = corpus_bytes(entry_id)
        members = tar_members(data)
        stations = []
        for member in members:
            body = data[member["data_offset"]:member["data_offset"] + member["size"]]
            info = jma_member(body)
            first = info["sweeps"][0]["product"]
            stations.append({
                "member": member["name"], "station_id": first["station_id"], "station_number": first["station_number"],
                "latitude": first["latitude"], "longitude": first["longitude"], "altitude_m": first["altitude_m"],
                "sweeps": len(info["sweeps"]), "reference_time": info["reference_time"],
                "radials_per_sweep": sorted({s["grid"]["radials"] for s in info["sweeps"]}),
                "gates_per_sweep": sorted({s["grid"]["gates"] for s in info["sweeps"]}),
                "data_offset": member["data_offset"], "size": member["size"],
                "grid_points": sum(s["grid"]["points"] for s in info["sweeps"]),
            })
        out[key] = {"file_bytes": len(data), "member_count": len(members), "stations": stations,
                    "total_grid_points": sum(s["grid_points"] for s in stations)}
    return out


# ------------------------------------------------------------------ router ---

def router():
    import pyart
    from metpy.io import Level2File

    out = {}
    for entry_id in ("l2-ktlx-20240315-000217-trim", "l2-ktlx-19990504-002218-trim", "l2-kvwx-20080415-235337",
                     "l2-kpah-20080415-235014"):
        path = str(corpus_path(entry_id))
        radar = pyart.io.read_nexrad_archive(path)
        raw = corpus_bytes(entry_id)
        # MetPy only detects a gzip object from a ".gz" name; hand it the
        # decompressed stream instead.
        l2 = Level2File(io.BytesIO(gzip.decompress(raw)) if raw[:2] == bytes([0x1F, 0x8B]) else path)
        out[entry_id] = {
            "pyart_nsweeps": int(radar.nsweeps), "pyart_nrays": int(radar.nrays),
            "pyart_rays_per_sweep": [int(e - s + 1) for s, e in zip(radar.sweep_start_ray_index["data"],
                                                                    radar.sweep_end_ray_index["data"])],
            "metpy_sweeps": len(l2.sweeps), "metpy_radials": sum(len(s) for s in l2.sweeps),
            "metpy_station": l2.stid.decode() if isinstance(l2.stid, bytes) else l2.stid,
            "first_bytes": corpus_bytes(entry_id)[:4].hex(),
        }

    entry_id = "odim-au24-20260610-000300-nci-zip-member"
    if entry_id in MANIFEST:
        import h5py
        raw = corpus_bytes(entry_id)
        sig, version, flags, method, _mtime, _mdate, crc, csize, usize, nlen, xlen = struct.unpack("<IHHHHHIIIHH", raw[:30])
        name = raw[30:30 + nlen].decode()
        start = 30 + nlen + xlen
        member = zlib.decompress(raw[start:start + csize], -15)
        h5 = h5py.File(io.BytesIO(member), "r")
        datasets = sorted((k for k in h5 if k.startswith("dataset")), key=lambda s: int(s[7:]))
        elangles = sorted(float(h5[d]["where"].attrs["elangle"]) for d in datasets)
        out["nci_zip_member"] = {
            "response_bytes": len(raw), "signature": hex(sig), "flags": flags, "method": method,
            "compressed_size": csize, "uncompressed_size": usize, "name": name, "data_start": start,
            "trailing_bytes": len(raw) - start - csize, "trailing_signature": raw[start + csize:start + csize + 4].hex(),
            "crc_ok": zlib.crc32(member) == crc, "member_sha256": hashlib.sha256(member).hexdigest(),
            "member_bytes": len(member), "member_first8": member[:8].hex(),
            "what": h5_attrs(h5["what"]), "where": h5_attrs(h5["where"]),
            "datasets": len(datasets), "elangles_sorted": elangles,
            "lowest_sweep": next({"nrays": int(h5[d]["where"].attrs["nrays"]), "nbins": int(h5[d]["where"].attrs["nbins"])}
                                 for d in datasets if float(h5[d]["where"].attrs["elangle"]) == elangles[0]),
        }
    return out


SECTIONS = {"cfradial": cfradial, "odim": odim, "dorade": dorade, "jma": jma, "router": router}


def main():
    global ALLOW_DOWNLOAD
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--section", action="append", choices=sorted(SECTIONS))
    parser.add_argument("--json", help="also write the goldens to this file")
    parser.add_argument("--no-download", action="store_true")
    args = parser.parse_args()
    ALLOW_DOWNLOAD = not args.no_download
    result = {}
    for name in args.section or sorted(SECTIONS):
        result[name] = SECTIONS[name]()
    text = json.dumps(result, indent=1, default=lambda o: o.item() if hasattr(o, "item") else str(o))
    if args.json:
        Path(args.json).write_text(text + "\n", encoding="utf-8")
    sys.stdout.write(text + "\n")


if __name__ == "__main__":
    main()
