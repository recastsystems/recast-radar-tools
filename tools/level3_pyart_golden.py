#!/usr/bin/env python3
"""Py-ART golden values for the Level III volumes.

Usage (from the workspace root)::

    python tools/level3_pyart_golden.py [--check]

Reads every file of ``testdata/level3/manifest.toml`` whose product code Py-ART
2.3.0 supports (``pyart.io.nexrad_level3.SUPPORTED_PRODUCTS``) with
``pyart.io.read_nexrad_level3`` and writes ``testdata/level3/golden-pyart.json``.

Per file (``files[i]``): ``id``, ``product``, ``nrays``, ``ngates``,
``field`` (Py-ART field name), ``azimuth`` (first 4 and last value, degrees),
``azimuth_sha256`` (SHA-256 of the float32 azimuths), ``range`` (first two
centres, metres), ``elevation`` (first value), ``fixed_angle``, ``latitude``,
``longitude``, ``altitude``, ``time_units``, ``time`` (first and last ray
time), and ``values``: ``count`` (unmasked gates), ``min``, ``max``,
``mean``, and ``sha256`` of the float32 data with masked gates as NaN
(C order). Files Py-ART rejects are listed with ``error``.
"""

import argparse
import hashlib
import json
import sys
import tomllib
import warnings
from pathlib import Path

import numpy as np

import pyart
from pyart.io import nexrad_level3

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "testdata" / "level3" / "golden-pyart.json"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    warnings.simplefilter("ignore")
    manifest = tomllib.loads((ROOT / "testdata" / "level3" / "manifest.toml").read_text())
    supported = set(nexrad_level3.SUPPORTED_PRODUCTS)
    files = []
    for entry in manifest["file"]:
        codes = [int(t.split(":")[1]) for t in entry.get("tags", []) if t.startswith("product:")]
        if not codes or codes[0] not in supported:
            continue
        path = ROOT / "testdata" / entry["committed"]
        item = {"id": entry["id"], "product": codes[0]}
        try:
            radar = pyart.io.read_nexrad_level3(str(path))
        except Exception as err:  # noqa: BLE001 - record what Py-ART does
            item["error"] = f"{type(err).__name__}: {err}"
            files.append(item)
            continue
        if not radar.fields:
            item["error"] = "no field"
            files.append(item)
            continue
        name = next(iter(radar.fields))
        data = radar.fields[name]["data"]
        filled = np.ma.filled(data.astype(np.float32), np.float32(np.nan))
        az = radar.azimuth["data"].astype(np.float32)
        item.update({
            "nrays": int(radar.nrays),
            "ngates": int(radar.ngates),
            "field": name,
            "azimuth": [float(x) for x in az[:4]] + [float(az[-1])],
            "azimuth_sha256": hashlib.sha256(az.tobytes()).hexdigest(),
            "range": [float(x) for x in radar.range["data"][:2]],
            "elevation": float(radar.elevation["data"][0]),
            "fixed_angle": float(radar.fixed_angle["data"][0]),
            "latitude": float(radar.latitude["data"][0]),
            "longitude": float(radar.longitude["data"][0]),
            "altitude": float(radar.altitude["data"][0]),
            "time_units": radar.time["units"],
            "time": [float(radar.time["data"][0]), float(radar.time["data"][-1])],
            "values": {
                "count": int(data.count()),
                "min": None if data.count() == 0 else float(data.min()),
                "max": None if data.count() == 0 else float(data.max()),
                "mean": None if data.count() == 0 else float(data.mean()),
                "sha256": hashlib.sha256(np.ascontiguousarray(filled).tobytes()).hexdigest(),
            },
        })
        files.append(item)
    text = json.dumps({"pyart": pyart.__version__, "files": files}, indent=1, sort_keys=True) + "\n"
    if args.check:
        if OUT.read_text() != text:
            sys.exit(f"{OUT} differs")
        return
    with open(OUT, "w", newline="\n") as out:
        out.write(text)
    print(len(files), "files;", sum("error" in f for f in files), "rejected")


if __name__ == "__main__":
    main()
