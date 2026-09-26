"""Step 2: our ODIM decode against a direct h5py read (section 3.1).

For every ODIM part in WORK/upstream_survey.jsonl (run_upstream_survey.py;
ZIP members and gzip wrappers unwrapped here), each datasetN/dataM is read
with h5py (gain and offset applied, `nodata` and `undetect` masked) and
matched to our sweep and field by quantity, elevation (0.05 deg) and ray
count. Gate count, spacing, valid-gate count, minimum and maximum, and the
first gate centre against `rstart * 1000 + rscale / 2` are compared.

No network. Writes WORK/odim_crosscheck.txt (every difference, then the
totals) and prints its tail.

Usage: python tools/feeds_survey/odim_crosscheck.py
"""

import gzip
import io
import json
import os
import struct
import zlib

import h5py
import numpy as np

from common import work


def text(value):
    if isinstance(value, bytes):
        return value.decode()
    return value.item() if hasattr(value, "item") else value


def h5_rows(raw):
    rows = []
    with h5py.File(io.BytesIO(raw), "r") as f:
        for dname in sorted((k for k in f if k.startswith("dataset")), key=lambda k: int(k[7:])):
            ds = f[dname]
            where = {k: text(v) for k, v in ds["where"].attrs.items()} if "where" in ds else {}
            ds_what = {k: text(v) for k, v in ds["what"].attrs.items()} if "what" in ds else {}
            for mname in sorted((k for k in ds if k.startswith("data")), key=lambda k: int(k[4:])):
                member = ds[mname]
                what = dict(ds_what)
                if "what" in member:
                    what.update({k: text(v) for k, v in member["what"].attrs.items()})
                data = member["data"][()]
                mask = np.ones(data.shape, bool)
                for key in ("nodata", "undetect"):
                    if key in what:
                        mask &= data != what[key]
                values = data[mask].astype(np.float64) * float(what.get("gain", 1.0)) + float(what.get("offset", 0.0))
                values = values[np.isfinite(values)]
                rows.append({
                    "dataset": dname, "quantity": what.get("quantity"), "elangle": where.get("elangle"),
                    "nrays": data.shape[0], "nbins": data.shape[1], "rscale": where.get("rscale"),
                    "rstart_km": where.get("rstart"), "valid": int(values.size),
                    "min": float(values.min()) if values.size else None,
                    "max": float(values.max()) if values.size else None,
                })
    return rows


def unwrap(raw):
    if raw[:4] == b"PK\x03\x04":
        method, = struct.unpack("<H", raw[8:10])
        name_len, extra_len = struct.unpack("<HH", raw[26:30])
        body = raw[30 + name_len + extra_len:]
        raw = zlib.decompressobj(-15).decompress(body) if method == 8 else body
    if raw[:2] == b"\x1f\x8b":
        raw = gzip.decompress(raw)
    return raw


problems = files = datasets = 0
with open(work("odim_crosscheck.txt"), "w", encoding="utf-8") as out:
    for line in open(work("upstream_survey.jsonl"), encoding="utf-8"):
        row = json.loads(line)
        if row["kind"] != "part" or not row.get("ok") or row["volume"]["source_format"] != "OdimH5":
            continue
        files += 1
        with open(row["path"], "rb") as handle:
            raw = unwrap(handle.read())
        tag = os.path.basename(row["path"])[:60]
        try:
            h5 = h5_rows(raw)
        except Exception as error:  # noqa: BLE001 - reported as a difference
            out.write(f"H5PY FAIL {tag}: {error}\n")
            problems += 1
            continue
        ours = [(s["fixed_angle_deg"], s["nrays"], s["gate_spacing_m"], s["first_gate_center_m"], f)
                for s in row["volume"]["sweeps"] for f in s["fields"]]
        used = set()
        for ref in h5:
            datasets += 1
            match = next((i for i, (angle, rays, _, _, field) in enumerate(ours)
                          if i not in used and field["name"] == ref["quantity"] and ref["elangle"] is not None
                          and abs(angle - ref["elangle"]) < 0.051 and rays == ref["nrays"]), None)
            if match is None:
                out.write(f"UNMATCHED {row['pair']} {tag} {ref['dataset']} {ref['quantity']} el={ref['elangle']}\n")
                problems += 1
                continue
            used.add(match)
            _, _, spacing, first_centre, field = ours[match]
            issues = []
            if field["ngates"] != ref["nbins"]:
                issues.append(f"ngates {field['ngates']} vs {ref['nbins']}")
            if field["valid"] != ref["valid"]:
                issues.append(f"valid {field['valid']} vs {ref['valid']}")
            for key in ("min", "max"):
                a, b = field[key], ref[key]
                if (a is None) != (b is None) or (a is not None and abs(a - b) > 1e-3 * max(1.0, abs(b))):
                    issues.append(f"{key} {a} vs {b}")
            plain = field["gate_stride"] == 1 and field["gate_start"] == 0
            if ref["rscale"] is not None and spacing is not None and plain and abs(spacing - ref["rscale"]) > 0.01:
                issues.append(f"spacing {spacing} vs rscale {ref['rscale']}")
            if None not in (ref["rstart_km"], ref["rscale"], first_centre) and plain:
                expect = ref["rstart_km"] * 1000 + ref["rscale"] / 2
                if abs(first_centre - expect) > 0.5:
                    issues.append(f"first centre {first_centre} vs rstart*1000+rscale/2 {expect}")
            if issues:
                problems += 1
                out.write(f"DIFF {row['pair']} {tag} {ref['dataset']} {ref['quantity']} el={ref['elangle']}: {'; '.join(issues)}\n")
    out.write(f"{files} ODIM files, {datasets} datasets, {problems} differences\n")
with open(work("odim_crosscheck.txt"), encoding="utf-8") as handle:
    print(handle.read()[-4000:])
