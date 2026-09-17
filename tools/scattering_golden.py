#!/usr/bin/env python3
"""Golden values for the real-input tests of recast-radar-scattering.

The Rust unit tests in ``crates/recast-radar-scattering/src/{lut,p3_table,scheme_psd,
tmatrix_runtime}.rs`` load the committed PyTMatrix 0.3.3 lookup tables and the WRF P3
v5.4 tables from the corpus (``testdata/scattering/manifest.toml``) and compare what the
crate decodes, interpolates and parses against the JSON files this script writes:

    testdata/golden/scattering/tmatrix_luts.json
    testdata/golden/scattering/p3_tables.json

Every expected value comes from outside the crate:

- LUT bytes are read here with ``struct``/``json`` from the schema-1 layout documented
  in ``crates/recast-radar-scattering/tools/pytmatrix-0.3.3/PACK_FORMAT.md`` and the
  generator ``manifest.json`` (8-byte magic, u16 LE schema, u32 LE header length, UTF-8
  JSON header, f64 LE payload point-major with the last declared axis fastest, nine
  components per node).
- The held-out nodes, the direct PyTMatrix results recomputed for them and the
  validator's own multilinear interpolation come from the post-freeze held-out report
  (corpus id ``tmatrix-held-out-interpolation-report-v10``); this script only checks
  that the report's ``lut_sha256`` pins the committed table and repeats the validator's
  interpolation with numpy on the payload read here.
- Prepared-plan layouts (strides, base index, active axes, upper offsets and fractions)
  are computed from the axis coordinates with the last-axis-fastest rule and
  ``fraction = (x - lower) / (upper - lower)``.
- P3 table records are read from the text with the record layout of the WRF v5.4
  tables (``module_mp_p3.F`` READ statements: 3 or 4 integer indices then 14 or 15 REAL
  fields per main record, ``mass rain rime value value`` collision records), never with
  the Rust parser.

Usage:
    python tools/scattering_golden.py

Files come from the committed corpus (testdata/files/...) or the shared download cache
that recast-radar-testdata fills. The committed files were written with Python 3.13 and
numpy 2.5.3.
"""

import hashlib
import json
import os
import struct
import sys
import tomllib
import urllib.request
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
TESTDATA = ROOT / "testdata"
GOLDEN = TESTDATA / "golden" / "scattering"

COMPONENTS = ["zh", "zv", "hh_vv_covariance_real", "hh_vv_covariance_imaginary", "kdp", "ah", "av",
              "fall_speed_first_moment", "fall_speed_second_moment"]


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


def cache_dir():
    if os.environ.get("RECAST_RADAR_TESTDATA"):
        return Path(os.environ["RECAST_RADAR_TESTDATA"])
    if os.name == "nt" and os.environ.get("LOCALAPPDATA"):
        return Path(os.environ["LOCALAPPDATA"], "recast-radar-tools", "testdata")
    if os.environ.get("XDG_CACHE_HOME"):
        return Path(os.environ["XDG_CACHE_HOME"], "recast-radar-tools", "testdata")
    if os.environ.get("HOME"):
        return Path(os.environ["HOME"], ".cache", "recast-radar-tools", "testdata")
    return ROOT / ".testdata-cache"


def corpus_bytes(entry_id):
    entry = MANIFEST[entry_id]
    if "committed" in entry:
        path = TESTDATA / entry["committed"].removeprefix("testdata/")
    else:
        path = cache_dir() / entry_id
        if not path.is_file():
            path.parent.mkdir(parents=True, exist_ok=True)
            with urllib.request.urlopen(entry["urls"][0], timeout=600) as r:
                path.write_bytes(r.read())
    data = path.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    if digest != entry["sha256"] or len(data) != entry["size"]:
        raise ValueError(f"{entry_id}: sha256/size mismatch ({digest}, {len(data)})")
    return data


# ---------------------------------------------------------------- LUTs ---

def read_lut(data):
    assert data[:8] == b"BRSLUT01", data[:8]
    schema, header_length = struct.unpack("<HI", data[8:14])
    assert schema == 1
    header_json = data[14:14 + header_length]
    header = json.loads(header_json.decode("utf-8"))
    payload = data[14 + header_length:]
    points = int(np.prod([len(axis["coordinates"]) for axis in header["axes"]]))
    values = np.frombuffer(payload, dtype="<f8").reshape(points, 9)
    return header, header_json, payload, values


def strides(axes):
    out = [1] * len(axes)
    for index in range(len(axes) - 2, -1, -1):
        out[index] = out[index + 1] * len(axes[index + 1]["coordinates"])
    return out


def bracket(coordinates, value):
    """(lower, upper, fraction) as the documented locate rule: exact nodes and
    singleton axes bracket to themselves with fraction 0."""
    first, last = coordinates[0], coordinates[-1]
    if value < first or value > last:
        raise ValueError(f"{value} outside [{first}, {last}]")
    if len(coordinates) == 1 or value == first:
        return 0, 0, 0.0
    if value == last:
        return len(coordinates) - 1, len(coordinates) - 1, 0.0
    upper = int(np.searchsorted(np.asarray(coordinates), value, side="left"))
    if coordinates[upper] == value:
        return upper, upper, 0.0
    lower = upper - 1
    return lower, upper, (value - coordinates[lower]) / (coordinates[upper] - coordinates[lower])


def multilinear(header, values, coordinates):
    """Reference multilinear interpolation, corner order lower-first per active axis."""
    axes = header["axes"]
    st = strides(axes)
    brackets = [bracket(axis["coordinates"], coordinates[axis["kind"]]) for axis in axes]
    active = [i for i, (lo, up, _) in enumerate(brackets) if lo != up]
    out = np.zeros(9)
    for corner in range(1 << len(active)):
        index = 0
        weight = 1.0
        bit = 0
        for axis_index, (lo, up, fraction) in enumerate(brackets):
            if lo == up:
                coordinate_index = lo
            else:
                if (corner >> bit) & 1:
                    weight *= fraction
                    coordinate_index = up
                else:
                    weight *= 1.0 - fraction
                    coordinate_index = lo
                bit += 1
            index += coordinate_index * st[axis_index]
        out += weight * values[index]
    return out


def prepared_plan(header, coordinates):
    axes = header["axes"]
    st = strides(axes)
    base = 0
    offsets = []
    fractions = []
    for axis_index, axis in enumerate(axes):
        lo, up, fraction = bracket(axis["coordinates"], coordinates[axis["kind"]])
        base += lo * st[axis_index]
        if lo != up:
            offsets.append((up - lo) * st[axis_index])
            fractions.append(fraction)
    return {"strides": st, "base_point_index": base, "active_axis_count": len(offsets),
            "corner_count": 1 << len(offsets), "upper_point_offsets": offsets, "upper_fractions": fractions}


def lut_golden(table_id, config_id, manifest_id, report):
    data = corpus_bytes(table_id)
    config = corpus_bytes(config_id)
    manifest = json.loads(corpus_bytes(manifest_id))
    header, header_json, payload, values = read_lut(data)
    lut_sha = hashlib.sha256(data).hexdigest()
    assert manifest["lut_sha256"] == lut_sha
    assert manifest["payload_sha256"] == hashlib.sha256(payload).hexdigest()
    assert manifest["generator_config_sha256"] == hashlib.sha256(config).hexdigest()
    assert header["config_sha256"] == hashlib.sha256(config).hexdigest()
    assert header["generator_config_utf8"].encode("utf-8") == config
    assert manifest["grid_point_count"] == values.shape[0]
    table = next(t for t in report["tables"] if t["lut_sha256"] == lut_sha)
    held_out = []
    for node in table["nodes"]:
        coordinates = node["coordinates"]
        reference = multilinear(header, values, coordinates)
        validator = np.array([node["lut_multilinear_interpolation"][c] for c in COMPONENTS])
        assert np.allclose(reference, validator, rtol=1e-9, atol=0), (coordinates, reference, validator)
        held_out.append({
            "coordinates": [coordinates[axis["kind"]] for axis in header["axes"]],
            "direct_pytmatrix": [node["direct_pytmatrix"][c] for c in COMPONENTS],
            "validator_interpolation": [node["lut_multilinear_interpolation"][c] for c in COMPONENTS],
            "within_thresholds": node["within_predeclared_interpolation_thresholds"],
            "node_index": node["node_index"],
        })
    axes = [{"kind": axis["kind"], "unit": axis["unit"], "count": len(axis["coordinates"]),
             "first": axis["coordinates"][0], "last": axis["coordinates"][-1]} for axis in header["axes"]]
    # Probe nodes: first, last and a middle point of the payload.
    probes = []
    for index in (0, values.shape[0] // 2, values.shape[0] - 1):
        multi = []
        rem = index
        for s in strides(header["axes"]):
            multi.append(rem // s)
            rem %= s
        probes.append({"index": index, "coordinate_indices": multi,
                       "coordinates": [header["axes"][a]["coordinates"][m] for a, m in enumerate(multi)],
                       "components": values[index].tolist()})
    # Prepared plans for two queries: one bracketing the first two non-singleton
    # axes, one on exact nodes (all singleton brackets).
    diameters = header["axes"][0]["coordinates"]
    ratios = header["axes"][1]["coordinates"]
    between = {header["axes"][0]["kind"]: diameters[1] + 0.3 * (diameters[2] - diameters[1]),
               header["axes"][1]["kind"]: (ratios[0] + ratios[1]) / 2.0}
    exact_last = {}
    for axis in header["axes"]:
        between.setdefault(axis["kind"], axis["coordinates"][0])
        exact_last[axis["kind"]] = axis["coordinates"][-1]
    exact_first = {axis["kind"]: axis["coordinates"][0] for axis in header["axes"]}
    plans = {}
    for name, query in (("between", between), ("exact_first", exact_first), ("exact_last", exact_last)):
        plans[name] = {"query": [query[axis["kind"]] for axis in header["axes"]],
                       "plan": prepared_plan(header, query),
                       "interpolation": multilinear(header, values, query).tolist()}
    return {
        "id": table_id, "lut_sha256": lut_sha, "config_sha256": header["config_sha256"],
        "payload_sha256": header["payload_sha256"], "payload_byte_length": len(payload),
        "header_byte_length": len(header_json), "grid_point_count": values.shape[0],
        "table_id": manifest["table_id"], "generator": header["generator"], "science": header["science"],
        "axes": axes, "probes": probes, "held_out": held_out, "plans": plans,
        "config_terminal_velocity": json.loads(config)["terminal_velocity"],
    }


def tmatrix_luts():
    report = json.loads(corpus_bytes("tmatrix-held-out-interpolation-report-v10"))
    nodes = json.loads(corpus_bytes("tmatrix-held-out-nodes-v10"))
    assert report["node_request_sha256"] == hashlib.sha256(
        corpus_bytes("tmatrix-held-out-nodes-v10")).hexdigest()
    out = {"report_id": report["report_id"], "thresholds": report["thresholds"],
           "selection_seed": report["selection_seed"], "held_out_node_count": report["held_out_node_count"],
           "tables": {}}
    for key, ids in (("rain", ("tmatrix-lut-rain-sband-pytmatrix-0.3.3",
                               "tmatrix-lut-rain-sband-pytmatrix-0.3.3-config",
                               "tmatrix-lut-rain-sband-pytmatrix-0.3.3-manifest")),
                     ("dry_ice", ("tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3",
                                  "tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3-config",
                                  "tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3-manifest"))):
        out["tables"][key] = lut_golden(*ids, report)
    del nodes
    return out


# ------------------------------------------------------------ P3 tables ---

def p3_records(text, indices, fields):
    """Main records of a P3 table text: list of (index tuple, field list) read
    from the text in file order, skipping the collision records (which have
    exactly 5 tokens: mass rain rime value value)."""
    lines = text.split("\n")
    header, separator = lines[0], lines[1]
    main = []
    collision = 0
    for line in lines[2:]:
        if not line:
            continue
        tokens = line.split()
        if len(tokens) == indices + fields:
            main.append((tuple(int(t) for t in tokens[:indices]),
                         [float(np.float32(t)) for t in tokens[indices:]]))
        elif len(tokens) == 5:
            collision += 1
        else:
            raise ValueError(f"unexpected record {line!r}")
    return header, separator, main, collision


def p3_golden(entry_id, indices, fields, lambda_field, mu_field, density_field=None):
    text = corpus_bytes(entry_id).decode("ascii")
    header, separator, main, collision = p3_records(text, indices, fields)
    samples = []
    for position in (0, 1, 49, len(main) // 2, len(main) - 1):
        index, values = main[position]
        sample = {"position": position, "index": list(index), "inverse_qmin": values[6],
                  "inverse_qmax": values[7], "lambda": values[lambda_field], "mu": values[mu_field]}
        if density_field is not None:
            sample["mean_density"] = values[density_field]
        samples.append(sample)
    inverse = [v[6] for _, v in main]
    return {"id": entry_id, "header": header, "separator": separator, "main_records": len(main),
            "collision_records": collision, "data_rows": len(main) + collision,
            "lines": text.count("\n"), "bytes": len(text.encode("ascii")),
            "first_line": text.split("\n")[2], "samples": samples,
            "inverse_qmin_min": min(inverse), "inverse_qmin_max": max(inverse),
            "all_qmin_ge_qmax": all(v[6] >= v[7] for _, v in main),
            "all_qmin_positive": all(v[6] > 0.0 for _, v in main)}


def p3_tables():
    return {
        "two_moment_first_block": p3_golden("wrf-p3-lookup-table-1-v5.4-2momI-first-block", 3, 14, 12, 13),
        "three_moment_first_block": p3_golden("wrf-p3-lookup-table-1-v5.4-3momI-first-block", 4, 15, 13, 14, 11),
        "two_moment": p3_golden("wrf-p3-lookup-table-1-v5.4-2momI", 3, 14, 12, 13),
        "three_moment": p3_golden("wrf-p3-lookup-table-1-v5.4-3momI", 4, 15, 13, 14, 11),
    }


def main():
    GOLDEN.mkdir(parents=True, exist_ok=True)
    for name, build in (("tmatrix_luts.json", tmatrix_luts), ("p3_tables.json", p3_tables)):
        path = GOLDEN / name
        with open(path, "w", encoding="utf-8", newline="\n") as fh:
            json.dump(build(), fh, indent=1, sort_keys=True)
            fh.write("\n")
        print(f"wrote {path} ({path.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
