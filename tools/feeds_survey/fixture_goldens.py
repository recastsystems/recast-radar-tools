"""The independent-reader values pinned in
crates/recast-radar-data/tests/feeds_known_failures.rs (its `golden` module):
Py-ART 2.3.0 on the committed KXWA head and on the whole KXWA volume, which
the check reads.

The committed head is read from testdata/files/feeds/; the whole volume from
the shared testdata cache (recast-radar-testdata's cache directory:
$RECAST_RADAR_TESTDATA, else %LOCALAPPDATA%/recast-radar-tools/testdata),
where the survey's fetch left it. No network.

Usage: python tools/feeds_survey/fixture_goldens.py
"""

import json
import os
import warnings

warnings.filterwarnings("ignore")

import numpy as np  # noqa: E402
import pyart  # noqa: E402

from common import REPO  # noqa: E402

FEEDS = os.path.join(REPO, "testdata", "files", "feeds")
CACHE = os.environ.get("RECAST_RADAR_TESTDATA") or os.path.join(
    os.environ.get("LOCALAPPDATA", os.path.expanduser("~/.cache")), "recast-radar-tools", "testdata"
)

LEVEL2 = {
    "ndswc-kxwa-20260924-214316-head41": os.path.join(FEEDS, "ndswc", "KXWA20260924_214316_V06.head41.ar2v"),
    "ndswc-kxwa-20260924-214316": os.path.join(CACHE, "ndswc-kxwa-20260924-214316"),
}


def attempt(read, path):
    if not os.path.exists(path):
        return {"error": f"missing {path}"}
    try:
        return read(path)
    except Exception as error:  # noqa: BLE001 - a reader failure is a result here
        return {"error": f"{type(error).__name__}: {error}"[:200]}


def summary(radar):
    first = radar.get_slice(0)
    return {
        "nsweeps": int(radar.nsweeps),
        "nrays": int(radar.nrays),
        "ngates": int(radar.ngates),
        "range0_m": float(radar.range["data"][0]),
        "gate_spacing_m": float(radar.range["data"][1] - radar.range["data"][0]),
        "fixed_angle": [round(float(a), 4) for a in radar.fixed_angle["data"]],
        "rays_per_sweep": [
            int(end - start + 1)
            for start, end in zip(radar.sweep_start_ray_index["data"], radar.sweep_end_ray_index["data"])
        ],
        "valid_gates_sweep0": {name: int(np.ma.count(field["data"][first])) for name, field in sorted(radar.fields.items())},
    }


def level2(path):
    radar = pyart.io.read_nexrad_archive(path)
    return {"vcp": radar.metadata.get("vcp_pattern"), **summary(radar)}


for fixture_id, path in LEVEL2.items():
    print(fixture_id, json.dumps(attempt(level2, path)))
