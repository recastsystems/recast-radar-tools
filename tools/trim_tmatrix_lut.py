#!/usr/bin/env python3
"""Trim a PyTMatrix 0.3.3 lookup table (BRSLUT01, schema 1) to a subset of its grid.

The research LUTs the property-aware runtime reads are tens of megabytes (the S-band dry
oblate P3/ISHMAEL table holds 163,520 nodes in 44 MB). The unit tests need only a few
nodes around their queries and the axis end points the runtime's contract checks, so this
tool writes a smaller table whose every node is a node of the source table, byte for byte:

- each axis keeps the coordinates at the given indices (the first and last are always
  kept, because the runtime requires exact axis bounds such as the dielectric model's
  temperature range);
- the payload keeps the nine f64 components of every kept node, in the source's
  point-major, last-axis-fastest order;
- the generator config keeps every key and value except the axis coordinates and the
  table id, which gains a ``-trim`` suffix; it is written with json.dumps(indent=2);
- the header keeps the generator identity and science metadata and gets the new axes,
  config text and hash, grid point count, payload length and payload hash.

The table is not regenerated: no PyTMatrix call is made, and node values are those the
locked generator produced for the source table (identified by its SHA-256 in the output
manifest). File layout (the crate's ``OfflineLut::from_bytes``): the 8-byte magic
``BRSLUT01``, a u16 LE schema, a u32 LE header length, the UTF-8 JSON header, then the
payload.

Usage:
    python tools/trim_tmatrix_lut.py SOURCE_DIR OUT_DIR AXIS=i,j,k [AXIS=...]

SOURCE_DIR holds the generator's ``table.lut`` and ``config.json``; AXIS is an axis kind
(``equivolume_diameter``, ``temperature``, ...) and i,j,k are coordinate indices to keep
(end points are added). Axes not named keep every coordinate. OUT_DIR receives
``table.lut``, ``config.json`` and ``trim.json``.
"""

import hashlib
import itertools
import json
import struct
import sys
from pathlib import Path

MAGIC = b"BRSLUT01"
COMPONENTS = 9


def read_lut(data):
    if data[:8] != MAGIC:
        raise SystemExit("not a BRSLUT01 file")
    schema, header_len = struct.unpack_from("<HI", data, 8)
    header = json.loads(data[14:14 + header_len].decode("utf-8"))
    payload = data[14 + header_len:]
    return schema, header, payload


def main(argv):
    if len(argv) < 2:
        raise SystemExit(__doc__)
    source, out = Path(argv[0]), Path(argv[1])
    keep_requests = {}
    for item in argv[2:]:
        kind, _, indices = item.partition("=")
        keep_requests[kind] = sorted({int(i) for i in indices.split(",") if i})

    data = (source / "table.lut").read_bytes()
    config_bytes = (source / "config.json").read_bytes()
    schema, header, payload = read_lut(data)
    if header["generator_config_utf8"].encode("utf-8") != config_bytes:
        raise SystemExit("config.json is not the config embedded in table.lut")
    if hashlib.sha256(payload).hexdigest() != header["payload_sha256"]:
        raise SystemExit("payload digest mismatch")

    axes = header["axes"]
    sizes = [len(axis["coordinates"]) for axis in axes]
    kept = []
    for axis, size in zip(axes, sizes):
        indices = set(keep_requests.pop(axis["kind"], range(size)))
        indices |= {0, size - 1}
        if any(i < 0 or i >= size for i in indices):
            raise SystemExit(f"{axis['kind']}: index outside 0..{size - 1}")
        kept.append(sorted(indices))
    if keep_requests:
        raise SystemExit(f"unknown axes {sorted(keep_requests)}")

    strides = [1] * len(sizes)
    for k in range(len(sizes) - 2, -1, -1):
        strides[k] = strides[k + 1] * sizes[k + 1]
    node_bytes = 8 * COMPONENTS
    if len(payload) != node_bytes * strides[0] * sizes[0]:
        raise SystemExit("payload length does not match the axes")
    new_payload = bytearray()
    for combo in itertools.product(*kept):
        flat = sum(i * s for i, s in zip(combo, strides))
        new_payload += payload[flat * node_bytes:(flat + 1) * node_bytes]

    config = json.loads(config_bytes.decode("utf-8"))
    for axis, indices in zip(config["axes"], kept):
        axis["coordinates"] = [axis["coordinates"][i] for i in indices]
    source_table_id = config["table_id"]
    config["table_id"] = source_table_id + "-trim"
    new_config = (json.dumps(config, indent=2) + "\n").encode("utf-8")

    for axis, indices in zip(header["axes"], kept):
        axis["coordinates"] = [axis["coordinates"][i] for i in indices]
    points = 1
    for indices in kept:
        points *= len(indices)
    header["generator_config_utf8"] = new_config.decode("utf-8")
    header["config_sha256"] = hashlib.sha256(new_config).hexdigest()
    header["grid_point_count"] = points
    header["payload_byte_length"] = len(new_payload)
    header["payload_sha256"] = hashlib.sha256(new_payload).hexdigest()
    header_json = json.dumps(header, separators=(",", ":")).encode("utf-8")
    lut = MAGIC + struct.pack("<HI", schema, len(header_json)) + header_json + bytes(new_payload)

    out.mkdir(parents=True, exist_ok=True)
    (out / "table.lut").write_bytes(lut)
    (out / "config.json").write_bytes(new_config)
    trim = {
        "tool": "tools/trim_tmatrix_lut.py",
        "source_table_id": source_table_id,
        "source_lut_sha256": hashlib.sha256(data).hexdigest(),
        "source_lut_bytes": len(data),
        "source_config_sha256": hashlib.sha256(config_bytes).hexdigest(),
        "kept_indices": {axis["kind"]: indices for axis, indices in zip(axes, kept)},
        "grid_point_count": points,
        "lut_sha256": hashlib.sha256(lut).hexdigest(),
        "lut_bytes": len(lut),
        "config_sha256": hashlib.sha256(new_config).hexdigest(),
    }
    (out / "trim.json").write_text(json.dumps(trim, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps(trim, indent=2))


if __name__ == "__main__":
    main(sys.argv[1:])
