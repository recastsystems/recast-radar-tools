#!/usr/bin/env python3
"""xradar goldens for the CfRadial 2 decoder of recast-radar-io-cfradial.

xradar's ``open_cfradial2_datatree`` (independent of the Rust crate under
test) opens each CfRadial 2 file of the corpus with ``first_dim="time"``,
``mask_and_scale=False``, ``decode_times=False`` and ``optional_groups=True``
and records, per sweep in xradar's order:

- the source group, ``sweep_fixed_angle``, ``sweep_mode``, ``follow_mode``,
  ``prt_mode``, the ray and gate counts, the first range centre and the
  spacing of the first two;
- per ray: ``time`` (seconds since the sweep's ``time.units`` reference, as
  an absolute Unix time), ``azimuth`` and ``elevation`` as SHA-256 of their
  float32 little-endian values, and whether xradar's time sort kept the file
  order;
- every ``(time, range)`` field: numpy dtype, ``scale_factor``,
  ``add_offset``, ``_FillValue`` and a SHA-256 of the raw values
  (little-endian, stored width) in the file's ray order;

and for the volume: ``instrument_name``, ``volume_number``,
``platform_type``, latitude/longitude/altitude, the time coverage, and the
radar_parameters and radar_calibration variables xradar keeps.

Run with the reference venv:

    python tools/cfradial2_golden.py [--id ID ...]

Writes testdata/golden/cfradial2/<id>.json.
"""

import argparse
import hashlib
import json
import math
import sys
import warnings
from pathlib import Path

import netCDF4
import numpy as np
import xradar as xd

sys.path.insert(0, str(Path(__file__).resolve().parent))
import hdf5_golden  # noqa: E402  (manifest and download helpers)

ROOT = hdf5_golden.ROOT
OUT = ROOT / "testdata" / "golden" / "cfradial2"

IDS = [
    "cfrad2-spol-20080604-002217-sur",
    "cfrad2-radx-irene-sr2-20110827-120420-sur-r30km",
    "cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32",
    "cfrad2-xradar-xsapr-sgp-20110520-ppi",
    "cfrad2-xradar-dow8-20211011-223602-rhi-r300",
]


def number(value):
    value = float(value)
    if math.isnan(value):
        return "NaN"
    return value


def sha(arr):
    arr = np.ascontiguousarray(np.asarray(arr))
    return hashlib.sha256(arr.astype(arr.dtype.newbyteorder("<"), copy=False).tobytes()).hexdigest()


def text(value):
    value = np.asarray(value)
    if value.dtype.kind == "S":
        return b"".join(value.reshape(-1).tolist()).split(b"\0")[0].decode().strip()
    item = value.reshape(-1)[0] if value.size else ""
    if isinstance(item, bytes):
        return item.decode().strip()
    return str(item).strip()


def unix_seconds(units, seconds):
    reference = netCDF4.num2date(0, units, only_use_cftime_datetimes=False,
                                 only_use_python_datetimes=True)
    import datetime as dt
    ref = reference.replace(tzinfo=dt.timezone.utc).timestamp()
    return [ref + float(s) for s in seconds]


def dump(entry_id):
    path = hdf5_golden.corpus_path(entry_id)
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        tree = xd.io.open_cfradial2_datatree(path, first_dim="time", mask_and_scale=False,
                                             decode_times=False, optional_groups=True)
    raw = netCDF4.Dataset(path)
    raw.set_auto_maskandscale(False)
    source_groups = sorted((g for g in raw.groups if g.startswith("sweep_")),
                           key=lambda g: int(g.split("_", 1)[1]))
    root = tree["/"].to_dataset()
    out = {
        "id": entry_id,
        "sha256": hdf5_golden.MANIFEST[entry_id]["sha256"],
        "generator": f"tools/cfradial2_golden.py (xradar {xd.__version__}, netCDF4-python "
                     f"{netCDF4.__version__})",
        "instrument_name": root.attrs.get("instrument_name"),
        "volume_number": int(root["volume_number"].values) if "volume_number" in root else None,
        "platform_type": text(root["platform_type"].values) if "platform_type" in root else None,
        "time_coverage_start": text(raw["time_coverage_start"][...])
        if "time_coverage_start" in raw.variables else None,
        "sweeps": [],
    }
    for name in ("latitude", "longitude", "altitude"):
        value = None
        if name in root.variables or name in root.coords:
            value = np.asarray(root[name].values).reshape(-1)
            value = number(value[0]) if value.size else None
        out[name] = value
    for group in ("radar_parameters", "radar_calibration"):
        if f"/{group}" in tree:
            ds = tree[group].to_dataset()
            out[group] = {k: [number(v) for v in np.asarray(ds[k].values).reshape(-1)]
                          for k in ds.data_vars if np.asarray(ds[k].values).dtype.kind in "fiu"}
    for index, source in enumerate(source_groups):
        sweep = tree[f"sweep_{index}"].to_dataset()
        group = raw[source]
        ray_dim = group["time"].dimensions[0]
        file_time = np.asarray(group["time"][...], dtype="float64")
        xr_time = np.asarray(sweep["time"].values, dtype="float64")
        entry = {
            "source_group": source,
            "fixed_angle": number(sweep["sweep_fixed_angle"].values),
            "sweep_mode": text(sweep["sweep_mode"].values),
            "follow_mode": text(sweep["follow_mode"].values),
            "prt_mode": text(sweep["prt_mode"].values),
            "nrays": int(sweep.sizes["time"]),
            "ngates": int(sweep.sizes["range"]),
            "ray_dimension": ray_dim,
            "range_first": number(sweep["range"].values[0]),
            "range_spacing": number(sweep["range"].values[1] - sweep["range"].values[0])
            if sweep.sizes["range"] > 1 else None,
            "file_order_is_time_order": bool(np.array_equal(np.sort(file_time), file_time)),
            "unix_time_first_last": [number(v) for v in
                                     unix_seconds(group["time"].units, file_time[[0, -1]])],
            "azimuth_sha256": sha(np.asarray(group["azimuth"][...], dtype="float32")),
            "elevation_sha256": sha(np.asarray(group["elevation"][...], dtype="float32")),
            "fields": [],
        }
        assert np.array_equal(np.sort(xr_time), xr_time)
        for name, var in group.variables.items():
            if var.dimensions != (ray_dim, group["range"].dimensions[0]):
                continue
            attrs = {a: var.getncattr(a) for a in var.ncattrs()}
            entry["fields"].append({
                "name": name,
                "dtype": str(var.dtype),
                "in_xradar": name in sweep.data_vars,
                "scale_factor": number(attrs["scale_factor"]) if "scale_factor" in attrs else None,
                "add_offset": number(attrs["add_offset"]) if "add_offset" in attrs else None,
                "fill_value": number(attrs["_FillValue"]) if "_FillValue" in attrs else None,
                "sha256": sha(np.asarray(var[...])),
            })
        out["sweeps"].append(entry)
    return out


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--id", action="append", help="corpus id (default: all)")
    args = parser.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)
    for entry_id in args.id or IDS:
        result = dump(entry_id)
        target = OUT / f"{entry_id}.json"
        with open(target, "w", encoding="utf-8", newline="\n") as fh:
            json.dump(result, fh, indent=1)
            fh.write("\n")
        print(f"{entry_id}: {len(result['sweeps'])} sweeps -> {target.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
