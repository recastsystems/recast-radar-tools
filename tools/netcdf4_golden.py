#!/usr/bin/env python3
"""netCDF4-python goldens for the netCDF-4 data model of recast-radar-hdf5.

netCDF4-python (the netCDF-C library, independent of the Rust crates under
test) opens each netCDF-4 file of the corpus, the classic CfRadial 1 files,
and ODIM_H5 files (plain HDF5, which netCDF-C reads with phony dimensions),
and records for every group,
depth first in netCDF-C's order:

- the path, the dimensions defined there (name, length, unlimited), the
  child groups, and every attribute (name, kind, value);
- every variable in netCDF-C order: name, netCDF type (CDL name), dimension
  names, shape, attributes, and its values with scaling, masking and
  char-to-string conversion off, as a SHA-256 of a canonical little-endian
  encoding plus a few leading values;

and for the file: the data model, ``_NCProperties`` and the classic-model
flag.

Canonical encoding (hashed; the Rust test encodes the same way): numbers as
little-endian bytes of the stored width; ``char`` as the raw bytes;
``string`` values as each element's UTF-8 text followed by one NUL.

Attribute kinds: ``text`` for NC_CHAR and NC_STRING values (netCDF4-python
returns both as ``str``), else the numpy dtype name of the value.

Run with the reference venv:

    python tools/netcdf4_golden.py [--id ID ...] [--no-download]

Writes testdata/golden/netcdf4/<id>.json.
"""

import argparse
import hashlib
import json
import math
import sys
from pathlib import Path

import netCDF4
import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import hdf5_golden  # noqa: E402  (manifest and download helpers)

ROOT = hdf5_golden.ROOT
OUT = ROOT / "testdata" / "golden" / "netcdf4"

IDS = [
    # netCDF-4 CfRadial 1 (netCDF-C 4.1-era and Radx writers).
    "cfrad1-xsapr-sgp-20110520-ppi-netcdf4",
    "cfrad1-dow8-20211011-223602-rhi",
    "cfrad1-spol-20080604-002217-sur",
    # CfRadial 2 (xradar and Radx writers).
    "cfrad2-spol-20080604-002217-sur",
    "cfrad2-radx-irene-sr2-20110827-120420-sur-r30km",
    "cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32",
    "cfrad2-xradar-xsapr-sgp-20110520-ppi",
    "cfrad2-xradar-dow8-20211011-223602-rhi-r300",
    # Classic CfRadial 1 (netCDF-C reads CDF-1/CDF-2 too): the io-cfradial
    # every-value test walks these like the netCDF-4 ones.
    "cfrad1-xsapr-sgp-20110520-ppi-classic",
    "cfrad1-dow8-20211011-223602-rhi-trim3-classic",
    "cfrad1-irene-sr2-20110827-120420-sur-sweeps01",
    # ODIM_H5: plain HDF5, phony dimensions.
    "odim-dkrom-20260820-1130-pvol",
    "odim-imgw-ram-20260711-0015-kdp-max",
]

HEAD = 8

TYPE_NAMES = {
    "int8": "byte",
    "uint8": "ubyte",
    "S1": "char",
    "int16": "short",
    "uint16": "ushort",
    "int32": "int",
    "uint32": "uint",
    "int64": "int64",
    "uint64": "uint64",
    "float32": "float",
    "float64": "double",
}


def json_number(value):
    if isinstance(value, (float, np.floating)):
        value = float(value)
        if math.isnan(value):
            return "NaN"
        if math.isinf(value):
            return "Infinity" if value > 0 else "-Infinity"
        return value
    return int(value)


def type_name(variable):
    if variable.dtype is str:
        return "string"
    if isinstance(variable.datatype, np.dtype):
        return TYPE_NAMES.get(variable.dtype.str.lstrip("<>|=") if variable.dtype.kind == "S"
                              else variable.dtype.name, "user-defined")
    return "user-defined"


def attribute_json(name, value):
    if isinstance(value, str):
        return {"name": name, "kind": "text", "value": value}
    arr = np.asarray(value)
    if arr.dtype.kind in ("U", "O", "S"):
        return {"name": name, "kind": "text", "value": [str(v) for v in arr.reshape(-1)]}
    flat = arr.reshape(-1)
    return {"name": name, "kind": arr.dtype.name, "value": [json_number(v) for v in flat]}


def values_json(variable, kind):
    variable.set_auto_maskandscale(False)
    variable.set_auto_chartostring(False)
    data = variable[...]
    if kind == "string":
        flat = np.asarray(data, dtype=object).reshape(-1)
        blob = b"".join(str(v).encode("utf-8") + b"\0" for v in flat)
        head = [str(v) for v in flat[:HEAD]]
    else:
        arr = np.ma.getdata(np.asarray(data)).reshape(-1)
        if kind == "char":
            blob = arr.astype("S1").tobytes()
            head = [b.decode("latin-1") for b in arr[:HEAD].astype("S1")]
        else:
            blob = arr.astype(arr.dtype.newbyteorder("<"), copy=False).tobytes()
            head = [json_number(v) for v in arr[:HEAD]]
    return {"len": int(np.asarray(data).size) if kind != "string" else len(flat),
            "sha256": hashlib.sha256(blob).hexdigest(), "head": head}


def group_json(group, out):
    path = group.path
    entry = {
        "path": path,
        "dims": [{"name": d.name, "len": len(d), "unlimited": d.isunlimited()}
                 for d in group.dimensions.values()],
        "attributes": [attribute_json(n, group.getncattr(n)) for n in group.ncattrs()],
        "groups": [g.path for g in group.groups.values()],
        "variables": [],
    }
    for variable in group.variables.values():
        kind = type_name(variable)
        entry["variables"].append({
            "name": variable.name,
            "type": kind,
            "dims": list(variable.dimensions),
            "shape": list(variable.shape),
            "attributes": [attribute_json(n, variable.getncattr(n)) for n in variable.ncattrs()],
            "value": values_json(variable, kind),
        })
    out.append(entry)
    for child in group.groups.values():
        group_json(child, out)


def dump(entry_id):
    path = hdf5_golden.corpus_path(entry_id)
    ds = netCDF4.Dataset(path)
    try:
        properties = ds.getncattr("_NCProperties")
    except (AttributeError, KeyError):
        properties = None
    groups = []
    group_json(ds, groups)
    return {
        "id": entry_id,
        "sha256": hdf5_golden.MANIFEST[entry_id]["sha256"],
        "generator": f"tools/netcdf4_golden.py (netCDF4-python {netCDF4.__version__}, "
                     f"netCDF-C {netCDF4.__netcdf4libversion__}, HDF5 {netCDF4.__hdf5libversion__})",
        "data_model": ds.data_model,
        "nc_properties": properties,
        "groups": groups,
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--id", action="append", help="corpus id (default: all)")
    parser.add_argument("--no-download", action="store_true")
    args = parser.parse_args()
    hdf5_golden.ALLOW_DOWNLOAD = not args.no_download
    OUT.mkdir(parents=True, exist_ok=True)
    for entry_id in args.id or IDS:
        result = dump(entry_id)
        target = OUT / f"{entry_id}.json"
        head = {k: v for k, v in result.items() if k != "groups"}
        lines = [json.dumps(head, separators=(",", ":"))[:-1] + ',"groups":[']
        for index, group in enumerate(result["groups"]):
            comma = "," if index + 1 < len(result["groups"]) else ""
            lines.append(json.dumps(group, separators=(",", ":")) + comma)
        lines.append("]}")
        with open(target, "w", encoding="utf-8", newline="\n") as fh:
            fh.write("\n".join(lines) + "\n")
        print(f"{entry_id}: {len(result['groups'])} groups -> {target.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
