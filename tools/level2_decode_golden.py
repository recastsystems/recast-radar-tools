#!/usr/bin/env python3
"""Golden values for the Level II decoder tests in recast-radar-io-nexrad.

Writes one JSON file per corpus input to ``testdata/level2/golden/decode/``.
The Rust unit tests in ``crates/recast-radar-io-nexrad/src/lib.rs`` (module
``tests``) read them. Every value comes from the file bytes through readers
that share no code with the Rust decoder:

* a byte walker written for this script: the 24-byte volume header, LDM bzip2
  record framing (control word, compressed and decompressed lengths), every
  message header of the uncompressed message stream, and for Message 1 and
  Message 31 radials the header fields, data block pointers and raw gate codes;
* MetPy 1.7.1 ``Level2File``: station id and volume header time, sweeps and
  radial counts, radial headers, VOL block (site latitude, longitude, height),
  Nyquist velocity, VCP (Message 5, or the Message 1 header), and the scaled
  moment arrays (NaN for codes 0 and 1);
* Py-ART 2.2.5 ``NEXRADLevel2File``: sweeps, rays per sweep, raw moment codes
  (``get_data(..., raw_data=True)``) and Nyquist velocities, cross-checked
  against the walker and MetPy.

The script fails (exit status 1) when the three readers disagree, so every
committed golden value is confirmed by at least two of them.

Run with the venv that has arm_pyart 2.2.5 and metpy 1.7.1:

    python tools/level2_decode_golden.py [--id ID ...] [--check]

``--check`` regenerates the values and compares them with the committed JSON
instead of writing. Inputs that are not committed are read from the shared
testdata cache (``RECAST_RADAR_TESTDATA`` or ``%LOCALAPPDATA%\\recast-radar-tools\\testdata``,
``$XDG_CACHE_HOME``/``~/.cache`` elsewhere), where ``cargo test`` downloads them.
"""

import argparse
import bz2
import datetime as dt
import gzip
import hashlib
import io
import json
import logging
import math
import os
import struct
import sys
import tempfile
import tomllib
import warnings
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
TESTDATA = ROOT / "testdata"
OUT_DIR = TESTDATA / "level2" / "golden" / "decode"

VOLUME_HEADER_LEN = 24
CTM_LEN = 12
MESSAGE_HEADER_LEN = 16
RECORD_BYTES = 2432

# Committed trimmed fixtures: every one is decoded by the corpus-wide test.
TRIMMED = [
    "l2-ktlx-19910605-162126-trim",
    "l2-ktlx-19990504-002218-trim",
    "l2-ktlx-20030508-221041-trim",
    "l2-klix-20050829-130035-trim",
    "l2-kdmx-20080525-205148-trim",
    "l2-ktlx-20130520-201643-trim",
    "l2-koax-20140616-205305-trim",
    "l2-kewx-20160413-022531-trim",
    "l2-kdvn-20200810-180401-trim",
    "l2-klix-20210829-180425-trim",
    "l2-kbox-20220129-150537-trim",
    "l2-tstl-20230331-230314-trim",
    "l2-pgua-20230524-030945-trim",
    "l2-kmtx-20240301-212827-trim",
    "l2-ktlx-20240315-000217-trim",
    "l2-kilx-20260418-013553-trim",
]

# Inputs: golden name -> manifest ids concatenated in order.
INPUTS = {name: [name] for name in TRIMMED}
INPUTS.update({
    # gzip archive objects (downloaded on first use)
    "l2-kpah-20080415-235014": ["l2-kpah-20080415-235014"],
    "l2-ktlx-19990503-230052": ["l2-ktlx-19990503-230052"],
    # the full bench volume (LDM bzip2 records, first cut ends with status 2)
    "l2-ktlx-20240315-000217": ["l2-ktlx-20240315-000217"],
    # committed real-time chunks: Start chunk + first intermediate chunk
    "l2chunk-kiwa-307-20260917-003629-001-s+002-i": [
        "l2chunk-kiwa-307-20260917-003629-001-s",
        "l2chunk-kiwa-307-20260917-003629-002-i",
    ],
})

# Fixtures that also get the metadata-less (GR2-style) layout check and the
# truncated-last-record check.
LAYOUT_CHECKS = {"l2-ktlx-20240315-000217-trim"}


# ------------------------------------------------------------------ corpus ---

def load_manifest():
    entries = {}
    paths = sorted(TESTDATA.glob("*/manifest.toml"))
    if (TESTDATA / "manifest.toml").is_file():
        paths.insert(0, TESTDATA / "manifest.toml")
    for path in paths:
        with open(path, "rb") as fh:
            for entry in tomllib.load(fh).get("file", []):
                entries[entry["id"]] = entry
    return entries


def cache_dir():
    if os.environ.get("RECAST_RADAR_TESTDATA"):
        return Path(os.environ["RECAST_RADAR_TESTDATA"])
    if os.name == "nt" and os.environ.get("LOCALAPPDATA"):
        return Path(os.environ["LOCALAPPDATA"]) / "recast-radar-tools" / "testdata"
    if os.environ.get("XDG_CACHE_HOME"):
        return Path(os.environ["XDG_CACHE_HOME"]) / "recast-radar-tools" / "testdata"
    return Path.home() / ".cache" / "recast-radar-tools" / "testdata"


def read_entry(manifest, file_id):
    entry = manifest[file_id]
    if "committed" in entry:
        path = TESTDATA / entry["committed"].removeprefix("testdata/")
    else:
        path = cache_dir() / file_id
    data = path.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    if digest != entry["sha256"] or len(data) != entry["size"]:
        raise SystemExit(f"{file_id}: {path} does not match the manifest sha256/size")
    return data


# ------------------------------------------------------------- byte walker ---

def ldm_records(data):
    """LDM bzip2 records after the volume header, or None when not LDM framed."""
    pos = VOLUME_HEADER_LEN
    records = []
    while pos + 4 <= len(data):
        control = struct.unpack(">i", data[pos:pos + 4])[0]
        if control == 0 or pos + 4 + abs(control) > len(data):
            return None
        block = data[pos + 4:pos + 4 + abs(control)]
        if not block.startswith(b"BZh"):
            return None
        records.append({"control_word": control, "offset": pos,
                        "payload": bz2.decompress(block)})
        pos += 4 + abs(control)
        if control < 0:
            break
    return records or None


def message_stream(raw):
    """(outer compression, LDM records or None, uncompressed message stream)."""
    outer = "none"
    if raw[:2] == b"\x1f\x8b":
        raw = gzip.decompress(raw)
        outer = "gzip"
    records = ldm_records(raw)
    if records is None:
        return outer, None, raw[VOLUME_HEADER_LEN:], raw[:VOLUME_HEADER_LEN]
    return outer, records, b"".join(r["payload"] for r in records), raw[:VOLUME_HEADER_LEN]


def walk_messages(stream):
    """Every message in the stream, with MetPy's framing rule: 2432-byte
    records, except Message 29/31 which are CTM + 2 * size halfwords."""
    messages = []
    pos = 0
    while pos + CTM_LEN + MESSAGE_HEADER_LEN <= len(stream):
        (size_hw, channel, msg_type, seq, date, time_ms, segments,
         segment) = struct.unpack(">HBBHHIHH", stream[pos + CTM_LEN:pos + 28])
        if size_hw == 0:
            length = RECORD_BYTES
        elif msg_type in (29, 31):
            length = CTM_LEN + 2 * size_hw
        else:
            length = RECORD_BYTES
        messages.append({
            "offset": pos, "size_hw": size_hw, "channel": channel, "type": msg_type,
            "sequence": seq, "date": date, "time_ms": time_ms, "segments": segments,
            "segment": segment,
        })
        pos += length
    return messages


def body_of(stream, message):
    start = message["offset"] + CTM_LEN + MESSAGE_HEADER_LEN
    return stream[start:message["offset"] + CTM_LEN + 2 * message["size_hw"]]


def msg31_radial(body):
    (stid, time_ms, date, az_num, az, compression, _spare, rad_length, az_spacing,
     status, el_num, sector, el, spot, az_index, blocks) = struct.unpack(
        ">4sIHHfBBHBBBBfBBH", body[:32])
    pointers = list(struct.unpack(">10I", body[32:72]))
    radial = {
        "stid": stid.decode("latin-1"), "time_ms": time_ms, "date": date,
        "azimuth_number": az_num, "azimuth_deg": az, "radial_length": rad_length,
        "azimuth_spacing_code": az_spacing, "status": status, "elevation_number": el_num,
        "cut_sector": sector, "elevation_deg": el, "block_count": blocks,
        "block_pointers": pointers, "moments": {}, "vol": None, "nyquist": None,
    }
    for ptr in pointers[:blocks]:
        if ptr == 0 or ptr + 4 > len(body):
            continue
        kind, name = body[ptr:ptr + 1], body[ptr + 1:ptr + 4]
        if kind == b"R" and name == b"VOL":
            lat, lon, amsl, feedhorn = struct.unpack(">ffhH", body[ptr + 8:ptr + 20])
            vcp = struct.unpack(">H", body[ptr + 40:ptr + 42])[0]
            radial["vol"] = {"lat": lat, "lon": lon, "site_amsl": amsl,
                             "feedhorn_agl": feedhorn, "vcp": vcp}
        elif kind == b"R" and name == b"RAD":
            radial["nyquist"] = struct.unpack(">h", body[ptr + 16:ptr + 18])[0] / 100.0
        elif kind == b"D":
            (gates, first, width, _tover, _snr, _flags, word, scale,
             offset) = struct.unpack(">HhhHhBBff", body[ptr + 8:ptr + 28])
            start = ptr + 28
            dtype = ">u1" if word == 8 else ">u2"
            codes = np.frombuffer(body[start:start + gates * word // 8], dtype)
            radial["moments"][name.decode("latin-1").strip()] = {
                "gates": gates, "first_gate_m": first, "gate_width_m": width,
                "word_size": word, "scale": scale, "offset": offset,
                "codes": codes.astype(np.int64),
            }
    return radial


def msg1_radial(body):
    (time_ms, date, _unamb, az_code, az_num, status, el_code, el_num, surv_first,
     dop_first, surv_width, dop_width, surv_gates, dop_gates, sector, _calib, ref_ptr,
     vel_ptr, sw_ptr, dop_res, vcp) = struct.unpack(">IHhHHHHHhhHHHHHfHHHHH", body[:46])
    nyquist_raw = struct.unpack(">h", body[60:62])[0]
    radial = {
        "time_ms": time_ms, "date": date, "azimuth_number": az_num,
        "azimuth_deg": az_code * 360.0 / 65536.0, "status": status,
        "elevation_number": el_num, "cut_sector": sector,
        "elevation_deg": el_code * 360.0 / 65536.0, "vcp": vcp,
        "nyquist": nyquist_raw / 100.0 if nyquist_raw > 0 else None,
        "nyquist_offset_46_raw": struct.unpack(">h", body[46:48])[0],
        "moments": {},
    }
    velocity_scale = 1.0 if dop_res == 4 else 2.0
    for name, ptr, gates, first, width, scale, offset in (
            ("REF", ref_ptr, surv_gates, surv_first, surv_width, 2.0, 66.0),
            ("VEL", vel_ptr, dop_gates, dop_first, dop_width, velocity_scale, 129.0),
            ("SW", sw_ptr, dop_gates, dop_first, dop_width, 2.0, 129.0)):
        if ptr and gates:
            codes = np.frombuffer(body[ptr:ptr + gates], ">u1").astype(np.int64)
            radial["moments"][name] = {
                "gates": gates, "first_gate_m": first, "gate_width_m": width,
                "word_size": 8, "scale": scale, "offset": offset, "codes": codes,
            }
    return radial


def radials_of(stream, messages):
    radials = []
    for message in messages:
        if message["size_hw"] == 0:
            continue
        if message["type"] == 31:
            radials.append(("31", message, msg31_radial(body_of(stream, message))))
        elif message["type"] == 1:
            radials.append(("1", message, msg1_radial(body_of(stream, message))))
    return radials


def split_sweeps(radials):
    """Sweeps as MetPy forms them: a new sweep at each start-of-elevation
    status (0, 3 or 5), padded up to the elevation number."""
    sweeps = []
    for kind, message, radial in radials:
        if radial["status"] in (0, 3, 5):
            sweeps.append([])
        while len(sweeps) < radial["elevation_number"]:
            sweeps.append([])
        sweeps[-1].append((kind, message, radial))
    return sweeps


# ------------------------------------------------------------------ helpers ---

def epoch_ms(date, time_ms):
    """NEXRAD modified Julian date (day 1 = 1970-01-01) and ms of day."""
    return (date - 1) * 86_400_000 + time_ms


def metpy_epoch_ms(value):
    delta = value - dt.datetime(1970, 1, 1)
    return delta // dt.timedelta(milliseconds=1)


def f32(value):
    return float(np.float32(value))


def fail(problems, message):
    problems.append(message)


def moment_summary(name, sweep, metpy_sweep, problems, label):
    rows = [(i, r) for i, (_, _, r) in enumerate(sweep) if name in r["moments"]]
    first = rows[0][1]["moments"][name]
    valid_count = 0
    raw_sum = 0
    scaled_sum = 0.0
    scaled_min = math.inf
    scaled_max = -math.inf
    min_at = max_at = None
    gates_max = 0
    for row, radial in rows:
        block = radial["moments"][name]
        for key in ("first_gate_m", "gate_width_m", "word_size", "scale", "offset"):
            if block[key] != first[key]:
                fail(problems, f"{label} {name}: {key} changes within the sweep")
        codes = block["codes"]
        gates_max = max(gates_max, block["gates"])
        valid = codes >= 2
        valid_count += int(valid.sum())
        raw_sum += int(codes[valid].sum())
        # MetPy's scaled values for the same radial, NaN for codes 0 and 1.
        _, metpy_moment = metpy_moment_of(metpy_sweep[row], name)
        metpy_valid = ~np.isnan(metpy_moment)
        if not np.array_equal(metpy_valid, valid):
            fail(problems, f"{label} {name} row {row}: MetPy missing mask differs from codes")
        scaled = np.asarray(metpy_moment, dtype=np.float64)
        expected = (codes.astype(np.float64) - block["offset"]) / block["scale"]
        if not np.allclose(scaled[valid], expected[valid], rtol=0, atol=1e-9):
            fail(problems, f"{label} {name} row {row}: MetPy values differ from codes")
        if valid.any():
            scaled_sum += float(scaled[valid].sum())
            gate = int(np.argmax(np.where(valid, scaled, -np.inf)))
            if scaled[gate] > scaled_max:
                scaled_max, max_at = float(scaled[gate]), [row, gate]
            gate = int(np.argmin(np.where(valid, scaled, np.inf)))
            if scaled[gate] < scaled_min:
                scaled_min, min_at = float(scaled[gate]), [row, gate]

    # Sample gates: the extremes, and the first valid gate of rows spread over
    # the sweep plus a gate 40% of the way out on the same rows.
    samples = []
    picks = []
    if max_at:
        picks += [tuple(max_at), tuple(min_at)]
    for k in range(6):
        row, radial = rows[(len(rows) - 1) * k // 5]
        codes = radial["moments"][name]["codes"]
        valid = np.nonzero(codes >= 2)[0]
        if len(valid):
            picks.append((row, int(valid[0])))
        picks.append((row, int(len(codes) * 2 // 5)))
    seen = set()
    for row, gate in picks:
        if (row, gate) in seen:
            continue
        seen.add((row, gate))
        block = sweep[row][2]["moments"][name]
        code = int(block["codes"][gate])
        value = None if code < 2 else f32((code - block["offset"]) / block["scale"])
        samples.append([row, gate, code, value])

    return {
        "rows": len(rows),
        "row_radials": [row for row, _ in rows] if len(rows) != len(sweep) else None,
        "word_size": first["word_size"],
        "gates_max": gates_max,
        "first_gate_m": first["first_gate_m"],
        "gate_width_m": first["gate_width_m"],
        "scale": f32(first["scale"]),
        "offset": f32(first["offset"]),
        "valid_count": valid_count,
        "raw_valid_sum": raw_sum,
        "scaled_sum": scaled_sum,
        "scaled_min": scaled_min if max_at else None,
        "scaled_max": scaled_max if max_at else None,
        "samples": samples,
    }


def metpy_moments(metpy_radial):
    """MetPy's moments of one radial by name. MetPy keys Message 31 moments by
    bytes and Message 1 moments by str."""
    if isinstance(metpy_radial, tuple) and len(metpy_radial) == 2:  # Message 1
        return dict(metpy_radial[1])
    return {key.decode("latin-1") if isinstance(key, bytes) else key: value
            for key, value in metpy_radial.moments.items()}


def metpy_moment_of(metpy_radial, name):
    return metpy_moments(metpy_radial)[name]


# ------------------------------------------------------------------ readers ---

def read_metpy(stream_bytes):
    from metpy.io import Level2File
    return Level2File(io.BytesIO(stream_bytes))


def read_pyart(raw_bytes):
    from pyart.io.nexrad_level2 import NEXRADLevel2File
    with tempfile.NamedTemporaryFile(delete=False, suffix=".V06") as fh:
        fh.write(raw_bytes)
        name = fh.name
    try:
        return NEXRADLevel2File(name)
    finally:
        try:
            os.unlink(name)
        except OSError:
            pass


# ----------------------------------------------------------------- golden ---

def golden_for(name, ids, manifest):
    problems = []
    raw = b"".join(read_entry(manifest, file_id) for file_id in ids)
    outer, records, stream, header = message_stream(raw)
    tape, extension, date, time_ms, icao = struct.unpack(">9s3sII4s", header)

    metpy_input = gzip.decompress(raw) if outer == "gzip" else raw
    metpy = read_metpy(metpy_input)
    messages = walk_messages(stream)
    radials = radials_of(stream, messages)
    sweeps = split_sweeps(radials)

    # Volume header against MetPy.
    if metpy.stid != icao:
        fail(problems, f"{name}: MetPy stid {metpy.stid!r} != header bytes {icao!r}")
    if metpy_epoch_ms(metpy.dt) != epoch_ms(date, time_ms):
        fail(problems, f"{name}: MetPy volume time differs from header bytes")

    # Sweeps and radial headers against MetPy.
    metpy_counts = [len(s) for s in metpy.sweeps]
    walker_counts = [len(s) for s in sweeps]
    if metpy_counts != walker_counts:
        fail(problems, f"{name}: MetPy sweeps {metpy_counts} != walker {walker_counts}")

    # Py-ART: sweeps, rays, raw codes and Nyquist.
    pyart_file = read_pyart(metpy_input)
    pyart_counts = [len(msgs) for msgs in pyart_file.scan_msgs]
    if pyart_counts != walker_counts:
        fail(problems, f"{name}: Py-ART rays {pyart_counts} != walker {walker_counts}")

    sweep_goldens = []
    for index, sweep in enumerate(sweeps):
        label = f"{name} sweep {index}"
        metpy_sweep = metpy.sweeps[index]
        first_kind, _, first = sweep[0]
        last = sweep[-1][2]
        for row, (kind, _, radial) in enumerate(sweep):
            if kind == "31":
                mh = metpy_sweep[row].header
                pairs = [(mh.az_num, radial["azimuth_number"]), (mh.az_angle, radial["azimuth_deg"]),
                         (mh.el_angle, radial["elevation_deg"]), (mh.time_ms, radial["time_ms"]),
                         (mh.date, radial["date"]), (mh.el_num, radial["elevation_number"])]
                metpy_nyquist = (metpy_sweep[row].radial_consts.nyq_vel
                                 if metpy_sweep[row].radial_consts else None)
            else:
                mh = metpy_sweep[row][0]
                pairs = [(mh.az_num, radial["azimuth_number"]),
                         (mh.time_ms, radial["time_ms"]), (mh.date, radial["date"]),
                         (mh.el_num, radial["elevation_number"])]
                if abs(mh.az_angle - radial["azimuth_deg"]) > 1e-9 or abs(
                        mh.el_angle - radial["elevation_deg"]) > 1e-9:
                    fail(problems, f"{label} row {row}: MetPy angles differ")
                metpy_nyquist = mh.nyq_vel
            if any(a != b for a, b in pairs):
                fail(problems, f"{label} row {row}: MetPy radial header differs: {pairs}")
            walker_nyquist = radial["nyquist"] or 0.0
            if metpy_nyquist is not None and abs((metpy_nyquist or 0.0) - walker_nyquist) > 1e-9:
                fail(problems, f"{label} row {row}: MetPy Nyquist {metpy_nyquist} != {walker_nyquist}")
            if set(radial["moments"]) != set(metpy_moments(metpy_sweep[row])):
                fail(problems, f"{label} row {row}: moment names differ from MetPy")

        moment_names = sorted({m for _, _, r in sweep for m in r["moments"]})
        moments = {m: moment_summary(m, sweep, metpy_sweep, problems, label) for m in moment_names}

        nyquist = pyart_file.get_nyquist_vel(scans=[index])
        walker = np.array([(r["nyquist"] or 0.0) for _, _, r in sweep])
        if not np.allclose(nyquist, walker, atol=1e-6):
            fail(problems, f"{label}: Py-ART Nyquist differs")
        for m, summary in moments.items():
            raw_codes = pyart_file.get_data(m, summary["gates_max"], scans=[index], raw_data=True)
            raw_codes = np.asarray(raw_codes, dtype=np.int64)
            # Py-ART pads rays past their gate count with code 1.
            valid = raw_codes >= 2
            if int(valid.sum()) != summary["valid_count"] or int(
                    raw_codes[valid].sum()) != summary["raw_valid_sum"]:
                fail(problems, f"{label} {m}: Py-ART raw codes differ "
                               f"({int(valid.sum())} vs {summary['valid_count']})")

        nyquists = [r["nyquist"] for _, _, r in sweep if r["nyquist"]]
        sweep_goldens.append({
            "radials": len(sweep),
            "message_type": int(first_kind),
            "elevation_number": first["elevation_number"],
            "first_status": first["status"],
            "last_status": last["status"],
            "first_elevation_deg": f32(first["elevation_deg"]),
            "first_azimuth_deg": f32(first["azimuth_deg"]),
            "last_azimuth_deg": f32(last["azimuth_deg"]),
            "azimuth_sum_deg": float(sum(f32(r["azimuth_deg"]) for _, _, r in sweep)),
            "elevation_sum_deg": float(sum(f32(r["elevation_deg"]) for _, _, r in sweep)),
            "first_epoch_ms": epoch_ms(first["date"], first["time_ms"]),
            "nyquist_count": len(nyquists),
            "nyquist_sum_mps": float(sum(nyquists)),
            "nyquist_min_mps": min(nyquists) if nyquists else None,
            "nyquist_max_mps": max(nyquists) if nyquists else None,
            "moments": moments,
        })

    first_kind, first_message, first_radial = radials[0]
    site = None
    vcp = None
    if first_kind == "31":
        vol = next((r["vol"] for _, _, r in radials if r["vol"]), None)
        metpy_vol = next((r.vol_consts for s in metpy.sweeps for r in s if r.vol_consts), None)
        if vol:
            site = {"latitude_deg": vol["lat"], "longitude_deg": vol["lon"],
                    "site_amsl_m": vol["site_amsl"], "feedhorn_agl_m": vol["feedhorn_agl"],
                    "vol_block_vcp": vol["vcp"]}
            if (metpy_vol.lat, metpy_vol.lon, metpy_vol.site_amsl, metpy_vol.feedhorn_agl) != (
                    vol["lat"], vol["lon"], vol["site_amsl"], vol["feedhorn_agl"]):
                fail(problems, f"{name}: MetPy VOL block differs")
    else:
        vcp = first_radial["vcp"]
        if metpy.sweeps[0][0][0].vcp != vcp:
            fail(problems, f"{name}: MetPy Message 1 VCP differs")
    message5_vcp = getattr(metpy, "vcp_info", None)
    message5_vcp = message5_vcp.num if message5_vcp is not None else None
    walker5 = next((struct.unpack(">H", body_of(stream, m)[4:6])[0]
                    for m in messages if m["type"] == 5 and m["size_hw"]), None)
    if message5_vcp is not None and walker5 != message5_vcp:
        fail(problems, f"{name}: Message 5 VCP {walker5} != MetPy {message5_vcp}")

    by_type = {}
    for m in messages:
        if m["size_hw"]:
            by_type[str(m["type"])] = by_type.get(str(m["type"]), 0) + 1
    first_radial_index = next(i for i, m in enumerate(messages) if m["size_hw"] and m["type"] in (1, 31))

    golden = {
        "name": name,
        "ids": ids,
        "generator": "tools/level2_decode_golden.py (MetPy 1.7.1 Level2File, Py-ART 2.2.5 NEXRADLevel2File, byte walker)",
        "outer_compression": outer,
        "ldm_records": None,
        "stream_len": len(stream),
        "volume_header": {
            "tape": tape.decode("latin-1"),
            "extension": extension.decode("latin-1"),
            "icao_hex": icao.hex(),
            "metpy_stid": metpy.stid.decode("latin-1"),
            "julian_date": date,
            "time_ms": time_ms,
            "metpy_epoch_ms": metpy_epoch_ms(metpy.dt),
        },
        "message_count": sum(1 for m in messages if m["size_hw"]),
        "messages_by_type": by_type,
        # Non-empty message headers up to and including the first radial:
        # [stream offset, size halfwords, channel, type, sequence, date,
        #  milliseconds, segments, segment number]
        "messages_before_first_radial": [
            [m["offset"], m["size_hw"], m["channel"], m["type"], m["sequence"], m["date"],
             m["time_ms"], m["segments"], m["segment"]]
            for m in messages[:first_radial_index + 1] if m["size_hw"]
        ],
        "first_radial": {
            "message_type": int(first_kind),
            "body_offset": first_message["offset"] + CTM_LEN + MESSAGE_HEADER_LEN,
            **{k: (f32(v) if isinstance(v, float) else v) for k, v in first_radial.items()
               if k not in ("moments", "vol", "nyquist")},
        },
        "site": site,
        "message1_vcp": vcp,
        "message5_vcp": walker5,
        "metpy_sweep_radials": metpy_counts,
        "pyart_sweep_rays": pyart_counts,
        "sweeps": sweep_goldens,
    }
    if records is not None:
        golden["ldm_records"] = []
        cursor = 0
        for record in records:
            end = cursor + len(record["payload"])
            golden["ldm_records"].append({
                "control_word": record["control_word"],
                "offset": record["offset"],
                "decompressed_len": len(record["payload"]),
                "radials": sum(1 for _, m, _ in radials if cursor <= m["offset"] < end),
            })
            cursor = end

    if name in LAYOUT_CHECKS:
        golden["layout_checks"] = layout_checks(name, raw, header, records, stream, messages,
                                                radials, problems)
    return golden, problems


def layout_checks(name, raw, header, records, stream, messages, radials, problems):
    """Byte layouts derived from the real file that the Rust tests build too,
    read back with MetPy to confirm they carry the same radials."""
    checks = {}

    # 1. The file without its last LDM record (what a decoder can recover when
    #    that record is corrupt).
    last = records[-1]
    truncated = raw[:last["offset"]]
    metpy = read_metpy(truncated)
    walker = split_sweeps(radials_of(b"".join(r["payload"] for r in records[:-1]),
                                     walk_messages(b"".join(r["payload"] for r in records[:-1]))))
    counts = [len(s) for s in walker]
    if [len(s) for s in metpy.sweeps] != counts:
        fail(problems, f"{name}: MetPy on the file without its last record disagrees")
    checks["without_last_record_sweep_radials"] = counts

    # 2. GR2-style export: the volume header, only the Message 2 and 5 records
    #    of the metadata, then every Message 31 back to back; uncompressed.
    kept = [m for m in messages if m["size_hw"] and m["type"] in (2, 5)]
    radial_messages = [m for m in messages if m["size_hw"] and m["type"] == 31]
    gr2 = bytearray(header)
    for m in kept:
        gr2 += stream[m["offset"]:m["offset"] + RECORD_BYTES]
    for m in radial_messages:
        gr2 += stream[m["offset"]:m["offset"] + CTM_LEN + 2 * m["size_hw"]]
    metpy = read_metpy(bytes(gr2))
    gr2_counts = [len(s) for s in metpy.sweeps]
    if gr2_counts != [len(s) for s in split_sweeps(radials)]:
        fail(problems, f"{name}: MetPy on the GR2 layout disagrees: {gr2_counts}")
    checks["gr2_metadata_types"] = [m["type"] for m in kept]
    checks["gr2_len"] = len(gr2)
    checks["gr2_sweep_radials"] = gr2_counts
    checks["gr2_azimuth_numbers_sweep0_head"] = [r["azimuth_number"]
                                                 for _, _, r in split_sweeps(radials)[0][:8]]
    return checks


# -------------------------------------------------------------------- main ---

def to_json(value, indent=0):
    """JSON with one key per line and arrays of scalars kept on one line."""
    pad = " " * indent
    inner = " " * (indent + 1)
    if isinstance(value, dict):
        if not value:
            return "{}"
        items = [f"{inner}{json.dumps(k)}: {to_json(v, indent + 1)}" for k, v in value.items()]
        return "{\n" + ",\n".join(items) + "\n" + pad + "}"
    if isinstance(value, list):
        if all(not isinstance(v, (dict, list)) for v in value):
            return json.dumps(value)
        items = [inner + to_json(v, indent + 1) for v in value]
        return "[\n" + ",\n".join(items) + "\n" + pad + "]"
    return json.dumps(value)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--id", action="append", help="golden name (default: all)")
    parser.add_argument("--check", action="store_true",
                        help="compare with the committed JSON instead of writing")
    args = parser.parse_args()
    logging.disable(logging.WARNING)
    warnings.simplefilter("ignore")

    manifest = load_manifest()
    names = args.id or list(INPUTS)
    status = 0
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    for name in names:
        golden, problems = golden_for(name, INPUTS[name], manifest)
        text = to_json(golden) + "\n"
        path = OUT_DIR / f"{name}.json"
        for problem in problems:
            print(f"DISAGREE {problem}", file=sys.stderr)
        if problems:
            status = 1
        if args.check:
            if not path.is_file() or path.read_text(encoding="utf-8") != text:
                print(f"STALE {path.relative_to(ROOT)}", file=sys.stderr)
                status = 1
            else:
                print(f"ok {name}")
        else:
            path.write_text(text, encoding="utf-8", newline="\n")
            print(f"wrote {path.relative_to(ROOT)} ({len(problems)} disagreements)")
    return status


if __name__ == "__main__":
    sys.exit(main())
