"""Golden values for the Meteo-France BUFR files, from numpy_bufr.

    pip install unlzw3 "git+https://github.com/Bram94/numpy_bufr@0641ed67de0f80c358714540d6c0b3dedf24af18"
    python tools/meteofrance_bufr_golden.py TABLES_DIR

`TABLES_DIR` is numpy_bufr's `Tables/libdwd` (unzip its Tables.zip). The
files are read from the testdata cache (they are not redistributed); for
every polar image (BUFR message) of each, the JSON under
testdata/conformance/meteofrance/ holds what numpy_bufr (an independent
decoder, MIT) decodes: the geometry, the time, the code table, and
fingerprints of the pixel codes (count, sum, all-ones count, and a
position-weighted sum that catches reordering) — no data values.
"""

from __future__ import annotations

import json
import os
import pathlib
import sys
import zlib

import numpy as np
import unlzw3
from numpy_bufr import decode_bufr

IDS = [
    "meteofrance-pag-07168-20130619-1200-c",
    "meteofrance-pag-07274-20130619-1200-e",
    "meteofrance-pag-07274-20130619-1200-a",
    "meteofrance-pam-07274-20130619-1200-a",
    "meteofrance-pag-07274-20130619-1205-a",
]
ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "testdata" / "conformance" / "meteofrance"


def cache_dir() -> pathlib.Path:
    if os.environ.get("RECAST_RADAR_TESTDATA_CACHE"):
        return pathlib.Path(os.environ["RECAST_RADAR_TESTDATA_CACHE"])
    base = os.environ.get("LOCALAPPDATA") or os.path.expanduser("~/.cache")
    return pathlib.Path(base) / "recast-radar-tools" / "testdata"


def expand(raw: bytes) -> bytes:
    out = b""
    while raw[:2] == b"\x1f\x8b":
        member = zlib.decompressobj(16 + 15)
        out += member.decompress(raw)
        raw = member.unused_data
    if raw[:2] == b"\x1f\x9d":
        out += unlzw3.unlzw(raw)
    return out


def messages(data: bytes):
    at = 0
    while (at := data.find(b"BUFR", at)) >= 0:
        length = int.from_bytes(data[at + 4:at + 7], "big")
        yield data[at:at + length]
        at += length


def first(values: dict, key: str):
    seq = values.get(key)
    return None if not seq else float(seq[0])


def image(decoder, message: bytes):
    try:
        _, _, data, loops = decoder(message)
    except ValueError:
        # numpy_bufr cannot expand a replication of zero; no polar image
        # has one (the PAM file's last message, a rain accumulation, does).
        return None
    values, loops = dict(data[0]), loops[0]
    # Elements inside replications (0-02-135 is in 3-21-196) come in loops.
    for loop in loops.values():
        for key, seq in loop.items():
            values.setdefault(key, list(np.ravel(seq)))
    if "002135" not in values or "005192" in values or "030021" not in values:
        return None
    rays, gates = int(first(values, "030022")), int(first(values, "030021"))
    pixels = None
    table = None
    for loop in loops.values():
        codes = loop.get("030001")
        if codes is not None and len(codes) == rays * gates and "021216" not in loop:
            pixels = np.asarray(codes)
        elif codes is not None and "021216" in loop:
            # 3-21-193 gives each code a lower and an upper bound (two
            # 0-21-216); numpy_bufr keys loop values by descriptor and keeps
            # the second, the upper bound.
            table = [[int(c), float(hi)] for c, hi in zip(codes, np.ravel(loop["021216"]))]
        elif "021216" in loop and table is None:
            table = [float(v) for v in loop["021216"]]
    if pixels is None:
        raise SystemExit(f"no pixel array of {rays} x {gates}")
    # numpy_bufr reports all-ones codes as NaN.
    missing = int(np.isnan(pixels).sum())
    codes = np.nan_to_num(pixels, nan=-1).astype(np.int64)
    valid = codes >= 0
    weights = (np.arange(codes.size) % 9973) + 1
    time = [int(values[key][0]) for key in ("004001", "004002", "004003", "004004", "004005", "004006")]
    return {
        "elevation_deg": first(values, "002135"),
        "rays": rays,
        "gates": gates,
        "gate_m": first(values, "055233"),
        "time": "{:04d}-{:02d}-{:02d}T{:02d}:{:02d}:{:02d}Z".format(*time),
        "station": int(first(values, "001001")) * 1000 + int(first(values, "001002")),
        "latitude": first(values, "005001"),
        "longitude": first(values, "006001"),
        "velocity_minimum": first(values, "049241"),
        "velocity_step": first(values, "049231"),
        "table": table,
        "codes": {
            "count": int(codes.size),
            "all_ones": missing,
            "sum": int(codes[valid].sum()),
            "weighted_sum": int((codes[valid] * weights[valid]).sum()),
            "max": int(codes[valid].max()),
        },
    }


def main() -> None:
    decoder = decode_bufr.DecodeBUFR(os.path.abspath(sys.argv[1]), "libdwd")
    OUT.mkdir(parents=True, exist_ok=True)
    for file_id in IDS:
        raw = (cache_dir() / file_id).read_bytes()
        images = []
        for index, message in enumerate(messages(expand(raw))):
            found = image(decoder, message)
            if found is not None:
                found["message"] = index
                images.append(found)
        path = OUT / f"{file_id}.json"
        path.write_text(json.dumps({"id": file_id, "decoder": "numpy_bufr 0641ed67", "images": images}, indent=1) + "\n",
                        encoding="utf-8", newline="\n")
        print(path.relative_to(ROOT), len(images), "images")


if __name__ == "__main__":
    main()
