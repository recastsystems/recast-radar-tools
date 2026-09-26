#!/usr/bin/env python3
"""Read-time benchmark: recast_radar against xradar and Py-ART.

Usage (from the repository root, with recast_radar, xradar and arm_pyart
importable)::

    python crates/recast-radar-py/bench/read_bench.py [--threads N] [--repeat 5] [--json] [ID ...]

Every case is a real file from the testdata manifests (committed, or the
download cache of ``recast-radar-testdata``; see ``pytests/conftest.py``).
Each reader decodes the whole file and every field value:

``recast_radar``          ``recast_radar.open(path).load()``: DataTree, CF-decoded
``recast_radar packed``   ``recast_radar.open(path, decode=False).load()``
``recast_radar to_pyart`` ``recast_radar.to_pyart(path)``
``xradar``                ``xradar.io.open_*_datatree(path).load()``
``xradar packed``         the same with ``mask_and_scale=False``
``pyart``                 Py-ART's reader for the format, then every field's data

Times are the median of ``--repeat`` runs after one warm-up run, in one
process. ``--threads N`` sets ``RAYON_NUM_THREADS`` for recast_radar's Rust
decoders before the package is imported (xradar and Py-ART run on one
thread). The machine and versions are printed with the table.
"""

from __future__ import annotations

import argparse
import gzip
import json
import os
import platform
import shutil
import statistics
import sys
import tempfile
import time
import warnings
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "pytests"))

CASES = [
    ("l2-ktlx-20240315-000217", "nexrad"),
    ("l2-kdvn-20200810-180401", "nexrad"),
    ("l2-klix-20050829-130035", "nexrad"),
    ("odim-dkrom-20260820-1130-pvol", "odim"),
    ("odim-iesha-20260305-0115-pvol", "odim"),
    ("odim-bejab-20190606-0000-pvol", "odim"),
    ("cfrad1-irene-sr2-20110827-120420-sur-sweeps01", "cfradial1"),
    ("cfrad1-dow8-20211011-223602-rhi-trim3-classic", "cfradial1"),
]


def median_time(fn, repeat: int) -> float:
    fn()
    times = []
    for _ in range(repeat):
        start = time.perf_counter()
        fn()
        times.append(time.perf_counter() - start)
    return statistics.median(times)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("ids", nargs="*", help="testdata ids (default: all cases)")
    parser.add_argument("--threads", type=int, default=0, help="RAYON_NUM_THREADS for recast_radar (0: default)")
    parser.add_argument("--repeat", type=int, default=5)
    parser.add_argument("--json", action="store_true", help="print JSON instead of a table")
    args = parser.parse_args()
    if args.threads:
        os.environ["RAYON_NUM_THREADS"] = str(args.threads)

    warnings.simplefilter("ignore")
    import numpy as np
    import pyart
    import xarray
    import xradar

    import recast_radar
    from conftest import data_path

    openers = {
        "nexrad": xradar.io.open_nexradlevel2_datatree,
        "odim": xradar.io.open_odim_datatree,
        "cfradial1": xradar.io.open_cfradial1_datatree,
    }
    pyart_readers = {
        "nexrad": pyart.io.read_nexrad_archive,
        "odim": pyart.aux_io.read_odim_h5,
        "cfradial1": pyart.io.read_cfradial,
    }

    def pyart_read(path, kind):
        radar = pyart_readers[kind](str(path))
        for field in radar.fields.values():
            np.asarray(field["data"])
        return radar

    cases = [case for case in CASES if not args.ids or case[0] in args.ids]
    rows = []
    with tempfile.TemporaryDirectory() as tmp:
        for file_id, kind in cases:
            path = data_path(file_id)
            xradar_path = path
            with open(path, "rb") as f:
                if f.read(2) == b"\x1f\x8b":  # xradar cannot read whole-file gzip
                    xradar_path = Path(tmp) / (file_id + ".raw")
                    with gzip.open(path, "rb") as src, open(xradar_path, "wb") as dst:
                        shutil.copyfileobj(src, dst)
            readers = {
                "recast_radar": lambda: recast_radar.open(path).load(),
                "recast_radar packed": lambda: recast_radar.open(path, decode=False).load(),
                "recast_radar to_pyart": lambda: recast_radar.to_pyart(path),
                "xradar": lambda: openers[kind](str(xradar_path)).load(),
                "xradar packed": lambda: openers[kind](str(xradar_path), mask_and_scale=False).load(),
                "pyart": lambda: pyart_read(path, kind),
            }
            row = {"id": file_id, "size": path.stat().st_size}
            for name, fn in readers.items():
                try:
                    row[name] = median_time(fn, args.repeat)
                except Exception as exc:  # noqa: BLE001 - report and go on
                    row[name] = f"error: {type(exc).__name__}"
            rows.append(row)

    meta = {
        "python": platform.python_version(),
        "machine": platform.platform(),
        "processor": platform.processor(),
        "cpus": os.cpu_count(),
        "rayon_threads": os.environ.get("RAYON_NUM_THREADS", "default"),
        "repeat": args.repeat,
        "recast_radar": recast_radar.__version__,
        "xradar": xradar.__version__,
        "arm_pyart": pyart.__version__,
        "xarray": xarray.__version__,
        "numpy": np.__version__,
    }
    if args.json:
        print(json.dumps({"meta": meta, "rows": rows}, indent=1))
        return 0
    names = [name for name in rows[0] if name not in ("id", "size")] if rows else []
    print(" ".join(f"{k}={v}" for k, v in meta.items()))
    print()
    print("| file | MB | " + " | ".join(names) + " |")
    print("|---|---:|" + "---:|" * len(names))
    for row in rows:
        cells = []
        for name in names:
            value = row[name]
            cells.append(f"{value * 1000:.0f} ms" if isinstance(value, float) else value)
        print(f"| `{row['id']}` | {row['size'] / 1e6:.1f} | " + " | ".join(cells) + " |")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
