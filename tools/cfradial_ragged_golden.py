#!/usr/bin/env python3
"""Goldens for CfRadial 1 files in `n_gates_vary` storage written by LROSE Radx.

For each file, netCDF4-python (netCDF-C) reads the variables as stored and
this script lays them out the way CfRadial 1.4 defines them (sections 2.3.1,
4.4 and 4.5), independently of the Rust decoder under test:

- the sweeps from `sweep_start_ray_index` / `sweep_end_ray_index`;
- each ray's gates from `ray_start_index` and `ray_n_gates` in the
  `(n_points)` field arrays, each sweep's rows padded to its longest ray with
  the field's `_FillValue`;
- each sweep's gate geometry from `range(range)`, or from the rows of a
  two-dimensional `range(time, range)` (LROSE Radx writes one when the
  geometry varies between rays; every row of a sweep must agree), with
  `ray_start_range` / `ray_gate_spacing` recorded beside it.

Recorded per sweep: rays, gates, first gate centre and spacing (metres); per
field: the scale, offset and fill, the SHA-256 of the raw stored codes
(row-major, rays x gates, little-endian, padded as above), the number of
codes that are not the fill, and the first codes of the first ray.

For a file with a one-dimensional `range`, Py-ART (`read_cfradial`) and
xradar (`open_cfradial1_datatree`) read it too: the script asserts that their
gate ranges and masked physical values equal the layout above. Neither reads
a two-dimensional `range`: Py-ART cannot broadcast it and xradar refuses a
multi-dimensional `range` index.

Run with the reference venv:

    python tools/cfradial_ragged_golden.py

Writes testdata/golden/cfradial1-ragged/<id>.json.
"""

import hashlib
import json
import sys
import tomllib
import warnings
from pathlib import Path

import netCDF4
import numpy as np

ROOT = Path(__file__).resolve().parent.parent
TESTDATA = ROOT / "testdata"
OUT = TESTDATA / "golden" / "cfradial1-ragged"

IDS = [
    "cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-per-ray-geometry",
    "cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-finest-geometry-netcdf4",
]
FIELDS = ["DBZH", "VRADH"]


def manifest():
    entries = {}
    for path in [TESTDATA / "manifest.toml", *sorted(TESTDATA.glob("*/manifest.toml"))]:
        if path.is_file():
            with open(path, "rb") as fh:
                for entry in tomllib.load(fh).get("file", []):
                    entries[entry["id"]] = entry
    return entries


def layout(path):
    ds = netCDF4.Dataset(path)
    ds.set_auto_maskandscale(False)
    var = ds.variables
    starts = var["sweep_start_ray_index"][:].astype(int)
    ends = var["sweep_end_ray_index"][:].astype(int)
    counts = var["ray_n_gates"][:].astype(int)
    first_index = var["ray_start_index"][:].astype(int)
    rng = var["range"][:].astype(np.float64)
    two_d = var["range"].dimensions == ("time", "range")
    ray_start_range = var["ray_start_range"][:].astype(np.float64)
    ray_gate_spacing = var["ray_gate_spacing"][:].astype(np.float64)
    sweeps = []
    for start, end in zip(starts, ends):
        rays = range(start, end + 1)
        ngates = int(counts[start:end + 1].max())
        if two_d:
            rows = rng[start:end + 1, :ngates]
            assert (rows == rows[0]).all(), "rays of one sweep disagree"
            centres = rows[0]
        else:
            centres = rng[:ngates]
        spacing = float(centres[1] - centres[0])
        assert np.allclose(np.diff(centres), spacing, atol=1e-3)
        fields = {}
        for name in FIELDS:
            v = var[name]
            fill = v.getncattr("_FillValue")
            data = v[:]
            out = np.full((len(rays), ngates), fill, dtype=data.dtype)
            for row, ray in enumerate(rays):
                n = counts[ray]
                out[row, :n] = data[first_index[ray]:first_index[ray] + n]
            fields[name] = {
                "dtype": str(data.dtype),
                "scale_factor": float(v.getncattr("scale_factor")),
                "add_offset": float(v.getncattr("add_offset")),
                "fill": int(fill),
                "codes_sha256": hashlib.sha256(out.astype("<" + data.dtype.str[1:]).tobytes()).hexdigest(),
                "not_fill": int((out != fill).sum()),
                "first_ray": out[0, :12].astype(int).tolist(),
                "_codes": out,
            }
        sweeps.append({
            "rays": len(rays),
            "ngates": ngates,
            "first_center_m": float(centres[0]),
            "spacing_m": spacing,
            "ray_start_range_m": sorted(set(ray_start_range[start:end + 1].tolist())),
            "ray_gate_spacing_m": sorted(set(ray_gate_spacing[start:end + 1].tolist())),
            "fields": fields,
        })
    return ds, two_d, sweeps


def cross_check(path, sweeps):
    """Py-ART and xradar read a one-dimensional-range file the same way."""
    import pyart
    import xradar

    radar = pyart.io.read_cfradial(str(path))
    for index, sweep in enumerate(sweeps):
        rays = radar.get_slice(index)
        n = sweep["ngates"]
        assert np.allclose(radar.range["data"][:n], sweep["first_center_m"] + sweep["spacing_m"] * np.arange(n))
        for name, field in sweep["fields"].items():
            codes = field["_codes"]
            expect = np.ma.masked_equal(codes, field["fill"]).astype(np.float64) * field["scale_factor"] + field["add_offset"]
            got = radar.fields[name]["data"][rays, :n]
            assert (np.ma.getmaskarray(got) == np.ma.getmaskarray(expect)).all(), (index, name)
            assert np.allclose(got.compressed(), expect.compressed(), atol=1e-4), (index, name)
            assert np.ma.getmaskarray(radar.fields[name]["data"][rays, n:]).all()
    tree = xradar.io.open_cfradial1_datatree(str(path), first_dim="time")
    for index, sweep in enumerate(sweeps):
        ds = tree[f"sweep_{index}"].ds
        n = sweep["ngates"]
        assert ds.sizes["range"] == n, (index, ds.sizes)
        assert np.allclose(ds["range"].values, sweep["first_center_m"] + sweep["spacing_m"] * np.arange(n))
        for name, field in sweep["fields"].items():
            codes = field["_codes"]
            expect = np.where(codes == field["fill"], np.nan, codes.astype(np.float64) * field["scale_factor"] + field["add_offset"])
            got = ds[name].values
            assert np.array_equal(np.isnan(got), np.isnan(expect)), (index, name)
            assert np.allclose(got[~np.isnan(got)], expect[~np.isnan(expect)], atol=1e-4), (index, name)
    return ["netCDF4", "pyart.io.read_cfradial", "xradar.io.open_cfradial1_datatree"]


def main():
    warnings.filterwarnings("ignore")
    entries = manifest()
    OUT.mkdir(parents=True, exist_ok=True)
    for entry_id in IDS:
        path = TESTDATA / entries[entry_id]["committed"]
        ds, two_d, sweeps = layout(path)
        readers = ["netCDF4"] if two_d else cross_check(path, sweeps)
        for sweep in sweeps:
            for field in sweep["fields"].values():
                del field["_codes"]
        golden = {
            "id": entry_id,
            "range_dims": list(ds.variables["range"].dimensions),
            "n_gates_vary": ds.getncattr("n_gates_vary"),
            "readers": readers,
            "sweeps": sweeps,
        }
        (OUT / f"{entry_id}.json").write_text(json.dumps(golden, indent=1) + "\n")
        print(f"{entry_id}: {len(sweeps)} sweeps, readers {readers}")


if __name__ == "__main__":
    sys.exit(main())
