#!/usr/bin/env python3
"""Validate the trimmed Level II fixtures against their full source volumes.

For every manifest entry whose ``derivation`` starts with ``trim-level2``
(``testdata/manifest.toml`` and ``testdata/*/manifest.toml``):

1. The committed trimmed file and the source volume (``derived_from``, taken
   from the shared download cache and downloaded there if missing) match their
   manifest sha256 and size.
2. Py-ART ``read_nexrad_archive`` and MetPy ``Level2File`` both read the
   trimmed file, and it holds exactly the sweeps and radial counts that the
   ``--sweeps`` / ``--max-radials`` options in the derivation select.
3. Py-ART: for every kept sweep and every moment, the raw moment arrays
   (``NEXRADLevel2File.get_data(..., raw_data=True)``) equal the same radials
   of the same sweep in the full file, as do the radial azimuth, elevation,
   collection time, Nyquist velocity, unambiguous range, VCP (Message 5) and
   location. The ``Radar`` objects from ``read_nexrad_archive`` (the source
   read with ``scans=`` limited to the kept sweeps, so both share one range
   geometry) have equal masked field data, angles, times and fixed angles.
4. MetPy: every kept radial (header, VOL/ELV/RAD blocks, and each moment's
   data block header and decoded array, NaN-aware) equals the same radial of
   the full file, and every metadata item decoded from the trimmed file
   (volume header, Messages 2, 3, 5, 13, 15, 18) equals the full file's.
5. Bytes: each trimmed record decompresses to a contiguous run of the
   source's uncompressed message stream, in source order; for sources made of
   LDM records, each trimmed record's bzip2 payload is identical to a source
   record's payload.

Run with the venv that has arm_pyart 2.2.5 and metpy 1.7.1:

    python tools/validate_trimmed.py [--id TRIM_ID ...] [--json results.json] [--no-download]

Exit status is 0 only when every check passes for every fixture.
"""

import argparse
import bz2
import copy
import gzip
import hashlib
import json
import logging
import math
import os
import sys
import tempfile
import tomllib
import urllib.request
import warnings
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
TESTDATA = ROOT / "testdata"
TOOL = "trim-level2"


# ---------------------------------------------------------------- manifest ---

def load_manifest():
    files = []
    paths = [TESTDATA / "manifest.toml"] if (TESTDATA / "manifest.toml").is_file() else []
    paths += sorted(p / "manifest.toml" for p in TESTDATA.iterdir()
                    if p.is_dir() and (p / "manifest.toml").is_file())
    for path in paths:
        with open(path, "rb") as fh:
            files += tomllib.load(fh).get("file", [])
    return {entry["id"]: entry for entry in files}


def parse_derivation(derivation):
    """Options from 'trim-level2 --sweeps 2 --max-radials 240: ...'."""
    words = derivation.split(":", 1)[0].split()
    if not words or words[0] != TOOL:
        return None
    options = {"sweeps": None, "max_radials": None}
    it = iter(words[1:])
    for flag in it:
        value = int(next(it))
        if flag == "--sweeps":
            options["sweeps"] = value
        elif flag == "--max-radials":
            options["max_radials"] = value
        else:
            raise ValueError(f"unexpected option {flag} in derivation")
    return options


def cache_dir():
    if os.environ.get("RECAST_RADAR_TESTDATA"):
        return Path(os.environ["RECAST_RADAR_TESTDATA"])
    for var, suffix in (("LOCALAPPDATA", ()), ("XDG_CACHE_HOME", ()), ("HOME", (".cache",))):
        if var == "LOCALAPPDATA" and os.name != "nt":
            continue
        if os.environ.get(var):
            return Path(os.environ[var], *suffix, "recast-radar-tools", "testdata")
    return ROOT / ".testdata-cache"


def sha256_file(path):
    digest = hashlib.sha256()
    size = 0
    with open(path, "rb") as fh:
        for block in iter(lambda: fh.read(1 << 20), b""):
            digest.update(block)
            size += len(block)
    return digest.hexdigest(), size


def committed_path(entry):
    rel = Path(entry["committed"])
    return ROOT / rel if rel.parts[0] == "testdata" else TESTDATA / rel


def source_path(entry, allow_download):
    path = cache_dir() / entry["id"]
    if not path.is_file():
        if not allow_download:
            raise FileNotFoundError(f"{entry['id']} is not cached at {path}")
        path.parent.mkdir(parents=True, exist_ok=True)
        for url in entry.get("urls", []):
            fd, tmp = tempfile.mkstemp(prefix=f".{entry['id']}.", suffix=".part", dir=path.parent)
            os.close(fd)
            try:
                urllib.request.urlretrieve(url, tmp)
                if sha256_file(tmp)[0] == entry["sha256"]:
                    os.replace(tmp, path)
                    break
            except OSError as error:
                print(f"  download {url}: {error}", file=sys.stderr)
            finally:
                if os.path.exists(tmp):
                    os.remove(tmp)
        else:
            raise FileNotFoundError(f"could not download {entry['id']}")
    return path


# ------------------------------------------------------------ comparisons ---

def deep_equal(a, b):
    """Structural equality: numpy arrays exactly (NaN == NaN), tuples/namedtuples,
    lists, dicts, floats (NaN == NaN)."""
    if isinstance(a, np.ma.MaskedArray) or isinstance(b, np.ma.MaskedArray):
        return np.ma.asarray(a).dtype == np.ma.asarray(b).dtype and masked_equal(a, b)
    if isinstance(a, np.ndarray) or isinstance(b, np.ndarray):
        a, b = np.asarray(a), np.asarray(b)
        if a.shape != b.shape or a.dtype != b.dtype:
            return False
        return bool(np.array_equal(a, b, equal_nan=a.dtype.kind in "fc"))
    if isinstance(a, tuple) and isinstance(b, tuple):
        return type(a) is type(b) and len(a) == len(b) and all(map(deep_equal, a, b))
    if isinstance(a, list) and isinstance(b, list):
        return len(a) == len(b) and all(map(deep_equal, a, b))
    if isinstance(a, dict) and isinstance(b, dict):
        return a.keys() == b.keys() and all(deep_equal(a[k], b[k]) for k in a)
    if isinstance(a, float) and isinstance(b, float) and math.isnan(a) and math.isnan(b):
        return True
    return bool(a == b)


def masked_equal(a, b):
    a, b = np.ma.asarray(a), np.ma.asarray(b)
    if a.shape != b.shape:
        return False
    ma, mb = np.ma.getmaskarray(a), np.ma.getmaskarray(b)
    return bool(np.array_equal(ma, mb)) and bool(np.array_equal(a.data[~ma], b.data[~mb]))


class Checks:
    def __init__(self):
        self.failures = []
        self.count = 0

    def check(self, ok, what):
        self.count += 1
        if not ok:
            self.failures.append(what)
        return ok


# --------------------------------------------------------------- readers ---

_STATION_TABLE = None


def restore_station_table():
    """Py-ART 2.2.5 get_nexrad_location converts the station elevation from feet
    to meters in place in its global table on every call, so a second read of
    a TDWR file (location looked up by ICAO) gets a different altitude. Put the
    original table back before each read."""
    global _STATION_TABLE
    from pyart.io import nexrad_common

    if _STATION_TABLE is None:
        _STATION_TABLE = copy.deepcopy(nexrad_common.NEXRAD_LOCATIONS)
    nexrad_common.NEXRAD_LOCATIONS.clear()
    nexrad_common.NEXRAD_LOCATIONS.update(copy.deepcopy(_STATION_TABLE))


def check_pyart(trim_path, src_path, options, checks, info):
    import pyart
    from pyart.io.common import prepare_for_read
    from pyart.io.nexrad_level2 import NEXRADLevel2File

    ft = NEXRADLevel2File(prepare_for_read(str(trim_path)))
    fs = NEXRADLevel2File(prepare_for_read(str(src_path)))
    sweeps = options["sweeps"]
    checks.check(ft.nscans == sweeps, f"pyart: trimmed nscans {ft.nscans} != {sweeps}")
    checks.check(ft._msg_type == fs._msg_type, "pyart: radial message type differs")
    counts_t = [ft.get_nrays(i) for i in range(ft.nscans)]
    counts_s = [fs.get_nrays(i) for i in range(sweeps)]
    info["pyart_rays"] = counts_t
    info["source_rays"] = counts_s
    # A radial limit is applied at a record boundary, so a sweep may keep fewer
    # radials than the limit, but never more, and never zero.
    for i, (nt, ns) in enumerate(zip(counts_t, counts_s)):
        limit = options["max_radials"]
        ok = 0 < nt <= ns and (nt == ns if limit is None else nt <= limit)
        checks.check(ok, f"pyart: sweep {i + 1} keeps {nt} of {ns} rays (limit {limit})")
    checks.check(deep_equal(ft.vcp, fs.vcp), "pyart: Message 5 VCP differs")
    checks.check(ft.get_vcp_pattern() == fs.get_vcp_pattern(), "pyart: VCP pattern differs")
    checks.check(deep_equal(ft.volume_header, fs.volume_header), "pyart: volume header differs")
    checks.check(ft.location() == fs.location(), "pyart: location differs")

    info_t = ft.scan_info(list(range(ft.nscans)))
    info_s = fs.scan_info(list(range(sweeps)))
    raw_arrays = 0
    for i in range(min(ft.nscans, sweeps)):
        nt = counts_t[i]
        st, ss = info_t[i], info_s[i]
        for key in ("moments", "ngates", "gate_spacing", "first_gate"):
            checks.check(st[key] == ss[key], f"pyart: sweep {i + 1} scan_info {key} differs")
        # Whole decoded radial records: message header, Message 1/31 header,
        # VOL/ELV/RAD blocks and every moment block with its raw data array.
        records_t = [ft.radial_records[k] for k in ft.scan_msgs[i]]
        records_s = [fs.radial_records[k] for k in fs.scan_msgs[i][:nt]]
        checks.check(deep_equal(records_t, records_s), f"pyart: sweep {i + 1} radial records differ")
        for key, getter in (("azimuth", ft.get_azimuth_angles), ("elevation", ft.get_elevation_angles)):
            source_getter = getattr(fs, getter.__name__)
            checks.check(deep_equal(getter([i]), source_getter([i])[:nt]),
                         f"pyart: sweep {i + 1} {key} angles differ")
        checks.check(deep_equal(ft.get_nyquist_vel([i]), fs.get_nyquist_vel([i])[:nt]),
                     f"pyart: sweep {i + 1} Nyquist velocity differs")
        checks.check(deep_equal(ft.get_unambigous_range([i]), fs.get_unambigous_range([i])[:nt]),
                     f"pyart: sweep {i + 1} unambiguous range differs")
        for moment in st["moments"]:
            ngates = max(st["ngates"][st["moments"].index(moment)],
                         ss["ngates"][ss["moments"].index(moment)])
            a = ft.get_data(moment, ngates, scans=[i], raw_data=True)
            b = fs.get_data(moment, ngates, scans=[i], raw_data=True)[:nt]
            checks.check(deep_equal(a, b), f"pyart: sweep {i + 1} raw {moment} differs")
            raw_arrays += 1
    info["pyart_raw_arrays_compared"] = raw_arrays

    # Radar objects: the source read with the kept scans only, so the range
    # axis and interpolation decisions are the same for both files.
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        restore_station_table()
        rt = pyart.io.read_nexrad_archive(str(trim_path))
        restore_station_table()
        rs = pyart.io.read_nexrad_archive(str(src_path), scans=list(range(sweeps)))
    info["pyart_radar"] = {"nsweeps": rt.nsweeps, "nrays": rt.nrays, "ngates": rt.ngates,
                           "fields": sorted(rt.fields)}
    checks.check(rt.nsweeps == sweeps, f"pyart: Radar has {rt.nsweeps} sweeps")
    checks.check(sorted(rt.fields) == sorted(rs.fields), "pyart: Radar field names differ")
    checks.check(deep_equal(rt.range["data"], rs.range["data"]), "pyart: Radar range differs")
    checks.check(deep_equal(rt.fixed_angle["data"], rs.fixed_angle["data"]),
                 "pyart: Radar fixed angles differ")
    for key in ("latitude", "longitude", "altitude"):
        checks.check(deep_equal(getattr(rt, key)["data"], getattr(rs, key)["data"]),
                     f"pyart: Radar {key} differs")
    time_offset = None
    for i in range(min(rt.nsweeps, rs.nsweeps)):
        t0, t1 = rt.sweep_start_ray_index["data"][i], rt.sweep_end_ray_index["data"][i] + 1
        s0 = rs.sweep_start_ray_index["data"][i]
        s1 = s0 + (t1 - t0)
        for key in ("azimuth", "elevation"):
            checks.check(deep_equal(getattr(rt, key)["data"][t0:t1], getattr(rs, key)["data"][s0:s1]),
                         f"pyart: Radar sweep {i + 1} {key} differs")
        # Times are offsets from the first ray; both files start at the same ray.
        dt = rt.time["data"][t0:t1] - rs.time["data"][s0:s1]
        time_offset = float(np.max(np.abs(dt))) if dt.size else 0.0
        checks.check(time_offset == 0.0, f"pyart: Radar sweep {i + 1} times differ")
        for name in rt.fields:
            checks.check(masked_equal(rt.fields[name]["data"][t0:t1], rs.fields[name]["data"][s0:s1]),
                         f"pyart: Radar sweep {i + 1} field {name} differs")
    checks.check(rt.time["units"] == rs.time["units"], "pyart: Radar time units differ")

    # Weather in the kept radials, for the manifest descriptions.
    stats = []
    for i in range(rt.nsweeps):
        t0, t1 = rt.sweep_start_ray_index["data"][i], rt.sweep_end_ray_index["data"][i] + 1
        s = {"sweep": i + 1, "fixed_angle": round(float(rt.fixed_angle["data"][i]), 2),
             "azimuth_first": round(float(rt.azimuth["data"][t0]), 1),
             "azimuth_last": round(float(rt.azimuth["data"][t1 - 1]), 1)}
        if "reflectivity" in rt.fields:
            z = rt.fields["reflectivity"]["data"][t0:t1]
            if z.count():
                arg = np.ma.argmax(z)
                ray, gate = np.unravel_index(arg, z.shape)
                s["max_dbz"] = round(float(z.max()), 1)
                s["max_dbz_range_km"] = round(float(rt.range["data"][gate]) / 1000, 1)
                s["max_dbz_azimuth"] = round(float(rt.azimuth["data"][t0 + ray]), 1)
                s["gates_ge_50dbz"] = int((z >= 50).sum())
        if "velocity" in rt.fields:
            v = rt.fields["velocity"]["data"][t0:t1]
            if v.count():
                s["max_abs_vel"] = round(float(np.ma.abs(v).max()), 1)
        stats.append(s)
    info["kept_weather"] = stats


def metpy_metadata(f):
    names = ("rda_status", "maintenance_data", "vcp_info", "clutter_filter_bypass_map",
             "clutter_filter_map", "rda")
    return {n: getattr(f, n) for n in names if hasattr(f, n)}


def check_metpy(trim_path, src_path, options, checks, info):
    from metpy.io import Level2File

    logging.getLogger("metpy.io.nexrad").setLevel(logging.ERROR)
    with open(trim_path, "rb") as fh:
        mt = Level2File(fh)
    with open(src_path, "rb") as fh:
        ms = Level2File(fh)
    sweeps = options["sweeps"]
    nonempty = [s for s in mt.sweeps if s]
    info["metpy_radials"] = [len(s) for s in mt.sweeps]
    checks.check(len(mt.sweeps) == sweeps and len(nonempty) == sweeps,
                 f"metpy: trimmed has {len(mt.sweeps)} sweeps ({len(nonempty)} non-empty), expected {sweeps}")
    checks.check(deep_equal(mt.vol_hdr, ms.vol_hdr), "metpy: volume header differs")
    meta_t, meta_s = metpy_metadata(mt), metpy_metadata(ms)
    for name, value in meta_t.items():
        if name == "rda_status":
            ok = deep_equal(value, meta_s.get(name, [])[:len(value)])
        else:
            ok = name in meta_s and deep_equal(value, meta_s[name])
        checks.check(ok, f"metpy: metadata {name} differs")
    info["metpy_metadata"] = sorted(meta_t)
    moments = 0
    for i in range(min(len(mt.sweeps), sweeps)):
        rt, rs = mt.sweeps[i], ms.sweeps[i]
        checks.check(0 < len(rt) <= len(rs), f"metpy: sweep {i + 1} has {len(rt)} of {len(rs)} radials")
        for k, (a, b) in enumerate(zip(rt, rs)):
            if not deep_equal(a, b):
                checks.check(False, f"metpy: sweep {i + 1} radial {k} differs")
                break
            data = a.moments if hasattr(a, "moments") else a[1]
            moments += len(data)
        else:
            checks.check(True, f"metpy: sweep {i + 1} radials equal")
    info["metpy_moment_arrays_compared"] = moments


# ------------------------------------------------------------------ bytes ---

def unwrap(path):
    raw = Path(path).read_bytes()
    if raw[:2] == b"\x1f\x8b":
        raw = gzip.decompress(raw)
    elif raw[:3] == b"BZh":
        raw = bz2.decompress(raw)
    return raw


def ldm_records(raw):
    """(compressed payload, control word) for each LDM record, or None."""
    if raw[28:31] != b"BZh":
        return None
    records, off = [], 24
    while off < len(raw):
        cw = int.from_bytes(raw[off:off + 4], "big", signed=True)
        records.append((raw[off + 4:off + 4 + abs(cw)], cw))
        off += 4 + abs(cw)
    return records


def check_bytes(trim_path, src_path, checks, info):
    trim, src = unwrap(trim_path), unwrap(src_path)
    checks.check(trim[:24] == src[:24], "bytes: volume header differs")
    trim_records = ldm_records(trim)
    checks.check(trim_records is not None, "bytes: trimmed file is not LDM records")
    if trim_records is None:
        return
    signs = [cw < 0 for _, cw in trim_records]
    checks.check(signs == [False] * (len(signs) - 1) + [True],
                 "bytes: only the last control word should be negative")
    src_records = ldm_records(src)
    if src_records is None:
        stream = src[24:]
    else:
        stream = b"".join(bz2.decompress(payload) for payload, _ in src_records)
    pos = 0
    for n, (payload, _) in enumerate(trim_records):
        data = bz2.decompress(payload)
        at = stream.find(data, pos)
        if not checks.check(at >= 0, f"bytes: record {n} is not a later run of source messages"):
            break
        pos = at + len(data)
    info["records"] = len(trim_records)
    if src_records is not None:
        payloads = {payload for payload, _ in src_records}
        same = sum(payload in payloads for payload, _ in trim_records)
        checks.check(same == len(trim_records),
                     f"bytes: {len(trim_records) - same} record payloads differ from every source record")
        info["records_identical_to_source"] = same


# ------------------------------------------------------------------- main ---

def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--id", action="append", help="validate only this trimmed id (repeatable)")
    parser.add_argument("--json", help="write per-file results to this JSON file")
    parser.add_argument("--no-download", action="store_true", help="fail instead of downloading sources")
    args = parser.parse_args()

    manifest = load_manifest()
    trimmed = [e for e in manifest.values() if parse_derivation(e.get("derivation", "")) is not None]
    if args.id:
        trimmed = [e for e in trimmed if e["id"] in args.id]
    if not trimmed:
        print("no trimmed Level II entries found")
        return 1

    results, failed = [], 0
    for entry in trimmed:
        info = {"id": entry["id"], "source": entry.get("derived_from")}
        checks = Checks()
        try:
            options = parse_derivation(entry["derivation"])
            info["options"] = options
            trim_path = committed_path(entry)
            digest, size = sha256_file(trim_path)
            checks.check(digest == entry["sha256"] and size == entry["size"],
                         f"trimmed file hash/size {digest}/{size} != manifest")
            checks.check(size <= 2_000_000, f"trimmed file is {size} bytes, over the 2 MB hard cap")
            info["size"] = size
            source = manifest[entry["derived_from"]]
            src_path = source_path(source, not args.no_download)
            digest, size = sha256_file(src_path)
            checks.check(digest == source["sha256"] and size == source["size"],
                         "source file hash/size != manifest")
            check_bytes(trim_path, src_path, checks, info)
            check_pyart(trim_path, src_path, options, checks, info)
            check_metpy(trim_path, src_path, options, checks, info)
        except Exception as error:  # a reader that cannot read the file is a failure
            checks.check(False, f"exception: {type(error).__name__}: {error}")
        info["checks"] = checks.count
        info["failures"] = checks.failures
        results.append(info)
        status = "PASS" if not checks.failures else "FAIL"
        failed += bool(checks.failures)
        print(f"{status} {entry['id']}: {checks.count} checks, "
              f"{info.get('size', '?')} bytes, Py-ART rays {info.get('pyart_rays')}, "
              f"MetPy radials {info.get('metpy_radials')}, records {info.get('records')}"
              + (f" ({info['records_identical_to_source']} identical to source)"
                 if "records_identical_to_source" in info else ""))
        for failure in checks.failures:
            print(f"    {failure}")
        for s in info.get("kept_weather", []):
            print(f"    kept sweep {s}")
        sys.stdout.flush()

    total = sum(r.get("size", 0) for r in results)
    print(f"{len(results) - failed}/{len(results)} trimmed files pass; {total} bytes committed")
    if args.json:
        Path(args.json).write_text(json.dumps(results, indent=1, default=str))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
