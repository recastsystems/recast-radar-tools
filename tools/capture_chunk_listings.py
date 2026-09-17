#!/usr/bin/env python3
r"""Capture real NEXRAD real-time chunk listings for the chunk timing model tests.

Source: the public bucket https://unidata-nexrad-level2-chunks.s3.amazonaws.com/
(keys `SITE/VOLID/YYYYMMDD-HHMMSS-CCC-T`, T in S/I/E). Chunk objects expire
within about a day, so a capture is a snapshot that cannot be re-fetched later.

Subcommands:

  survey [--sites K...]
      For every site prefix in the bucket, find the newest complete volume,
      download its Start chunk, decode Message 5 and print the VCP, the cut
      count, the chunk count and the volume duration. Nothing is written.

  capture SITE[:FIRST-LAST] [...] [--volumes N] [--out DIR] [--metpy]
      For each site, take the N newest consecutive complete volumes (every
      chunk id 1..E present, one Start, one End), or exactly the volume ids
      FIRST..LAST when given (ids wrap from 999 to 1). For each volume write

        DIR/SITE/VOLID-YYYYMMDD-HHMMSS.xml          raw ListObjectsV2 response
                                                     for prefix SITE/VOLID/,
                                                     byte for byte
        DIR/SITE/VOLID-YYYYMMDD-HHMMSS.chunks.csv    per chunk: Message 31
                                                     radial count, elevation
                                                     number, first/last radial
                                                     time and azimuth, radial
                                                     statuses (decoded from
                                                     the downloaded chunk)
        DIR/SITE/VOLID-YYYYMMDD-HHMMSS.vcp.csv       Message 5 cut table from
                                                     the Start chunk

      and append the volume to DIR/manifest.toml (sha256 of every written
      file). The chunk bytes themselves are not written. With --metpy the
      concatenated chunks are also read with MetPy's Level2File and its
      sweeps (radial counts, first/last radial times) and VCP table are
      compared with this script's decoder; any disagreement aborts.

Decoding follows ICD 2620002 (Archive II / Message 31 / Message 5), standard
library only: each chunk is a sequence of LDM records (4-byte big-endian
length, sign ignored, then a bzip2 stream); the Start chunk begins with the
24-byte volume header. Inside a record, Message 31 is variable length
(12-byte CTM + size_hw * 2 bytes); every other message is a 2432-byte frame.

The committed fixtures (crates/recast-radar-data/tests/fixtures/chunks) were
produced on 2026-09-17 at about 01:30Z, after a `survey`, with Python 3.13 and
MetPy 1.7.1, by:

  python tools/capture_chunk_listings.py capture KAMA KDGX KGRB KRGX KMAX KEAX \
      KIWA:306-308 TATL --volumes 3 --metpy --out <scratch dir>

and the output directory was copied into the fixtures directory unchanged.

Sites were picked from the survey for VCP variety: VCP 35, VCP 35 with base
tilt and SAILS, VCP 34, VCP 12, VCP 212 with and without AVSET, VCP 215, and
TDWR VCP 90. KIWA 306-308 includes volume 307, which TD.1 captured chunk by
chunk.

The held-out fixtures (crates/recast-radar-data/tests/fixtures/chunks-holdout),
which were not used to build or tune the timing model, were produced on
2026-09-17 with the same tools, from sites outside the fitted set picked from a
`survey` at about 02:45Z for what the fitted set lacks (TDWR VCP 80, VCP 34 with
a base tilt, VCP 215 with MESO-SAILS, the 999 -> 1 id wrap), by:

  python tools/capture_chunk_listings.py capture TLAS:998-1 --metpy --out <dir A>
  python tools/capture_chunk_listings.py capture KTLX KMUX KHNX KMSX KDMX KGJX KBUF PAHG TDEN \
      --volumes 3 --metpy --out <dir B>

(02:46Z and 03:09Z); both output directories were copied into the fixtures
directory unchanged, with the TLAS manifest entries appended after dir B's.

In the survey output, "inserted" counts Message 5 cuts with nonzero
supplemental data (SAILS, MESO-SAILS, MRLE, base tilt).
"""

from __future__ import annotations

import argparse
import bz2
import concurrent.futures
import datetime as dt
import hashlib
import io
import os
import struct
import sys
import time
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

BUCKET_URL = "https://unidata-nexrad-level2-chunks.s3.amazonaws.com/"
NS = "{http://s3.amazonaws.com/doc/2006-03-01/}"
USER_AGENT = "recast-radar-tools capture_chunk_listings.py"
# Volume ids run 1..=999 and the id after 999 is 1 (there is no id 0).
MAX_VOLUME_ID = 999


def next_volume_id(volume_id: int) -> int:
    return 1 if volume_id >= MAX_VOLUME_ID else volume_id + 1


# ---------------------------------------------------------------- HTTP / S3


def http_get(url: str, attempts: int = 4) -> bytes:
    delay = 1.0
    for attempt in range(attempts):
        try:
            request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
            with urllib.request.urlopen(request, timeout=60) as response:
                return response.read()
        except Exception:  # noqa: BLE001 - retried, re-raised on the last attempt
            if attempt + 1 == attempts:
                raise
            time.sleep(delay)
            delay *= 2
    raise AssertionError("unreachable")


def list_page(prefix: str, delimiter: str | None = None, token: str | None = None) -> bytes:
    query = [("list-type", "2"), ("prefix", prefix)]
    if delimiter:
        query.append(("delimiter", delimiter))
    if token:
        query.append(("continuation-token", token))
    return http_get(BUCKET_URL + "?" + urllib.parse.urlencode(query))


def parse_listing(body: bytes):
    root = ET.fromstring(body)
    contents = []
    for node in root.findall(NS + "Contents"):
        contents.append(
            {
                "key": node.findtext(NS + "Key"),
                "last_modified": node.findtext(NS + "LastModified"),
                "size": int(node.findtext(NS + "Size")),
            }
        )
    prefixes = [node.findtext(NS + "Prefix") for node in root.findall(NS + "CommonPrefixes")]
    truncated = root.findtext(NS + "IsTruncated") == "true"
    token = root.findtext(NS + "NextContinuationToken")
    return contents, prefixes, truncated, token


def list_all(prefix: str, delimiter: str | None = None):
    contents, prefixes, pages = [], [], []
    token = None
    while True:
        body = list_page(prefix, delimiter, token)
        pages.append(body)
        page_contents, page_prefixes, truncated, token = parse_listing(body)
        contents.extend(page_contents)
        prefixes.extend(page_prefixes)
        if not truncated:
            return contents, prefixes, pages


def parse_chunk_key(key: str):
    site, volume, name = key.split("/")
    date, clock, chunk, kind = name.split("-")
    return {
        "site": site,
        "volume_id": int(volume),
        "volume_time": dt.datetime.strptime(date + clock, "%Y%m%d%H%M%S").replace(
            tzinfo=dt.timezone.utc
        ),
        "chunk_id": int(chunk),
        "kind": kind,
    }


def parse_time(value: str) -> dt.datetime:
    return dt.datetime.fromisoformat(value.replace("Z", "+00:00"))


def volume_ids_newest_first(site: str) -> list[int]:
    """Volume ids present under SITE/, newest first.

    Ids count up 1..=999 and wrap to 1; the newest id is the one just before
    the largest circular gap in the set of present ids.
    """
    _, prefixes, _ = list_all(site + "/", "/")
    ids = sorted({int(p.rstrip("/").split("/")[1]) for p in prefixes if p.rstrip("/").split("/")[1].isdigit()})
    if not ids:
        return []
    largest_gap, newest_index = -1, len(ids) - 1
    for index, current in enumerate(ids):
        following = ids[(index + 1) % len(ids)] + (MAX_VOLUME_ID if index + 1 == len(ids) else 0)
        if following - current > largest_gap:
            largest_gap, newest_index = following - current, index
    return [ids[(newest_index - k) % len(ids)] for k in range(len(ids))]


def complete_volume(site: str, volume_id: int):
    """Return (listing body, chunk rows) when SITE/VOLID/ is one complete volume."""
    prefix = f"{site}/{volume_id}/"  # ids are not zero-padded in keys
    contents, _, pages = list_all(prefix)
    if len(pages) != 1 or not contents:
        return None
    rows = []
    for item in contents:
        try:
            parsed = parse_chunk_key(item["key"])
        except ValueError:
            return None
        parsed.update(item)
        rows.append(parsed)
    rows.sort(key=lambda row: row["chunk_id"])
    times = {row["volume_time"] for row in rows}
    ids = [row["chunk_id"] for row in rows]
    kinds = [row["kind"] for row in rows]
    if len(times) != 1 or ids != list(range(1, len(rows) + 1)):
        return None
    if kinds[0] != "S" or kinds[-1] != "E" or any(k != "I" for k in kinds[1:-1]):
        return None
    return pages[0], rows


# ------------------------------------------------------------ Level II decode


def ldm_records(chunk: bytes):
    offset = 24 if chunk[:4] in (b"AR2V", b"ARCH") else 0
    while offset + 4 <= len(chunk):
        (length,) = struct.unpack_from(">i", chunk, offset)
        length = abs(length)
        offset += 4
        if length == 0 or offset + length > len(chunk):
            raise ValueError(f"bad LDM record length {length} at {offset - 4}")
        yield bz2.decompress(chunk[offset : offset + length])
        offset += length


def messages(record: bytes):
    offset = 0
    while offset + 28 <= len(record):
        size_hw, _channel, msg_type, _seq, _date, _ms, _segments, _segment = struct.unpack_from(
            ">HBBHHIHH", record, offset + 12
        )
        if msg_type == 31:
            end = offset + 12 + size_hw * 2
            yield msg_type, record[offset + 28 : end]
            offset = end
        else:
            if msg_type != 0:
                yield msg_type, record[offset + 28 : offset + 2432]
            offset += 2432


def decode_radial_header(body: bytes):
    (
        _radar,
        ms,
        date,
        _az_number,
        azimuth,
        _compression,
        _spare,
        _length,
        az_spacing,
        status,
        elevation_number,
        _sector,
        elevation,
    ) = struct.unpack_from(">4sIHHfBBHBBBBf", body, 0)
    when = dt.datetime(1970, 1, 1, tzinfo=dt.timezone.utc) + dt.timedelta(days=date - 1, milliseconds=ms)
    return {
        "time": when,
        "azimuth": azimuth,
        "az_spacing": az_spacing,
        "status": status,
        "elevation_number": elevation_number,
        "elevation": elevation,
    }


WAVEFORMS = {1: "CS", 2: "CD/W", 3: "CD/WO", 4: "B", 5: "SPP"}


def decode_message5(body: bytes):
    (size_hw, pattern_type, pattern, cuts, version, clutter_group, dop_res, pulse_width) = struct.unpack_from(
        ">HHHHBBBB", body, 0
    )
    sequencing, supplemental = struct.unpack_from(">HH", body, 14)
    rows = []
    for index in range(cuts):
        base = 22 + 46 * index
        el_code, channel, waveform, super_res, surv_prf, surv_pulses, az_code = struct.unpack_from(
            ">HBBBBHh", body, base
        )
        sector1_prf, sector1_pulses, cut_supplemental = struct.unpack_from(">HHH", body, base + 24)
        rows.append(
            {
                "cut": index + 1,
                "elevation_deg": el_code * 360.0 / 65536.0,
                "channel": channel,
                "waveform": WAVEFORMS.get(waveform, str(waveform)),
                "super_res": super_res,
                "surv_prf": surv_prf,
                "surv_pulses": surv_pulses,
                "azimuth_rate_deg_s": az_code * 90.0 / 65536.0,
                "doppler_prf": sector1_prf,
                "doppler_pulses": sector1_pulses,
                "supplemental": cut_supplemental,
            }
        )
    return {
        "vcp": pattern,
        "pattern_type": pattern_type,
        "version": version,
        "doppler_resolution_code": dop_res,
        "pulse_width_code": pulse_width,
        "sequencing": sequencing,
        "supplemental": supplemental,
        "cuts": rows,
    }


def decode_start_chunk(chunk: bytes):
    for record in ldm_records(chunk):
        for msg_type, body in messages(record):
            if msg_type == 5:
                return decode_message5(body)
    raise ValueError("Start chunk has no Message 5")


def decode_radial_chunk(chunk: bytes):
    radials, other = [], {}
    for record in ldm_records(chunk):
        for msg_type, body in messages(record):
            if msg_type == 31:
                radials.append(decode_radial_header(body))
            else:
                other[msg_type] = other.get(msg_type, 0) + 1
    return radials, other


def iso_ms(value: dt.datetime) -> str:
    return value.strftime("%Y-%m-%dT%H:%M:%S.") + f"{value.microsecond // 1000:03d}Z"


def chunk_url(key: str) -> str:
    return BUCKET_URL + key


# ------------------------------------------------------------------ survey


def survey_site(site: str):
    try:
        for volume_id in volume_ids_newest_first(site)[:4]:
            found = complete_volume(site, volume_id)
            if found is None:
                continue
            _, rows = found
            vcp = decode_start_chunk(http_get(chunk_url(rows[0]["key"])))
            duration = (parse_time(rows[-1]["last_modified"]) - rows[0]["volume_time"]).total_seconds()
            inserted = sum(1 for c in vcp["cuts"] if c["supplemental"] != 0)
            return (
                f"{site} vol {volume_id:3d} {rows[0]['volume_time']:%H:%M:%S} vcp {vcp['vcp']:3d} "
                f"cuts {len(vcp['cuts']):2d} inserted {inserted} chunks {len(rows):3d} "
                f"duration {duration:5.0f} s"
            )
        return f"{site} no complete volume among the newest four"
    except Exception as error:  # noqa: BLE001 - survey keeps going
        return f"{site} error {error}"


def command_survey(args):
    if args.sites:
        sites = args.sites
    else:
        _, prefixes, _ = list_all("", "/")
        sites = [p.rstrip("/") for p in prefixes]
    with concurrent.futures.ThreadPoolExecutor(max_workers=16) as pool:
        for line in pool.map(survey_site, sites):
            print(line, flush=True)


# ----------------------------------------------------------------- capture


def metpy_check(chunks: list[bytes], per_chunk, vcp):
    from metpy.io import Level2File

    level2 = Level2File(io.BytesIO(b"".join(chunks)))
    ours = {}
    for radials in per_chunk:
        for radial in radials:
            ours.setdefault(radial["elevation_number"], []).append(radial)
    if len(level2.sweeps) != len(ours):
        raise SystemExit(f"MetPy sweeps {len(level2.sweeps)} != decoded elevations {len(ours)}")
    for sweep, (our_el, our_radials) in zip(level2.sweeps, sorted(ours.items())):
        first, last = sweep[0][0], sweep[-1][0]
        if first.el_num != our_el or len(sweep) != len(our_radials):
            raise SystemExit(f"MetPy sweep {first.el_num}/{len(sweep)} != decoded {our_el}/{len(our_radials)}")
        for header, radial in ((first, our_radials[0]), (last, our_radials[-1])):
            midnight = radial["time"].replace(hour=0, minute=0, second=0, microsecond=0)
            if header.time_ms != (radial["time"] - midnight) // dt.timedelta(milliseconds=1):
                raise SystemExit(f"MetPy radial time {header.time_ms} ms != decoded {radial['time']}")
            if abs(header.az_angle - radial["azimuth"]) > 1e-3:
                raise SystemExit(f"MetPy azimuth {header.az_angle} != decoded {radial['azimuth']}")
    info = level2.vcp_info
    if info.num != vcp["vcp"] or info.num_el_cuts != len(vcp["cuts"]):
        raise SystemExit(f"MetPy VCP {info.num}/{info.num_el_cuts} != decoded {vcp['vcp']}/{len(vcp['cuts'])}")
    for el, row in zip(info.els, vcp["cuts"]):
        if abs(el.el_angle - row["elevation_deg"]) > 1e-3 or abs(el.az_rate - row["azimuth_rate_deg_s"]) > 1e-3:
            raise SystemExit(
                f"MetPy cut {el.el_angle}/{el.az_rate} != decoded {row['elevation_deg']}/{row['azimuth_rate_deg_s']}"
            )
    import metpy

    return (
        f"MetPy {metpy.__version__} Level2File agrees: {len(level2.sweeps)} sweeps, "
        f"VCP {info.num} with {info.num_el_cuts} cuts"
    )


def sha256_file(path: str) -> str:
    with open(path, "rb") as handle:
        return hashlib.sha256(handle.read()).hexdigest()


def write_bytes(path: str, data: bytes):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as handle:
        handle.write(data)


def capture_volume(out_dir: str, site: str, listing: bytes, rows, use_metpy: bool, captured_at: str):
    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
        chunks = list(pool.map(lambda row: http_get(chunk_url(row["key"])), rows))
    for row, chunk in zip(rows, chunks):
        if len(chunk) != row["size"]:
            raise SystemExit(f"{row['key']}: downloaded {len(chunk)} bytes, listing says {row['size']}")
    vcp = decode_start_chunk(chunks[0])
    per_chunk, other_messages = [], []
    for chunk in chunks[1:]:
        radials, other = decode_radial_chunk(chunk)
        per_chunk.append(radials)
        other_messages.append(other)
    note = metpy_check(chunks, per_chunk, vcp) if use_metpy else "MetPy check not run"

    stem = f"{rows[0]['volume_id']:03d}-{rows[0]['volume_time']:%Y%m%d-%H%M%S}"
    base = os.path.join(out_dir, site, stem)
    write_bytes(base + ".xml", listing)

    lines = [
        "chunk_id,type,size,last_modified,radials,elevation_numbers,elevation_deg,"
        "first_radial_time,last_radial_time,first_azimuth,last_azimuth,az_spacing,radial_statuses,other_messages"
    ]
    for row, radials, other in zip(rows[1:], per_chunk, other_messages):
        elevations = sorted({r["elevation_number"] for r in radials})
        statuses = sorted({r["status"] for r in radials})
        spacing = sorted({r["az_spacing"] for r in radials})
        lines.append(
            ",".join(
                [
                    str(row["chunk_id"]),
                    row["kind"],
                    str(row["size"]),
                    row["last_modified"],
                    str(len(radials)),
                    " ".join(map(str, elevations)),
                    f"{radials[0]['elevation']:.3f}" if radials else "",
                    iso_ms(radials[0]["time"]) if radials else "",
                    iso_ms(radials[-1]["time"]) if radials else "",
                    f"{radials[0]['azimuth']:.3f}" if radials else "",
                    f"{radials[-1]['azimuth']:.3f}" if radials else "",
                    " ".join(map(str, spacing)),
                    " ".join(map(str, statuses)),
                    " ".join(f"{k}x{v}" for k, v in sorted(other.items())),
                ]
            )
        )
    write_bytes(base + ".chunks.csv", ("\n".join(lines) + "\n").encode())

    vcp_lines = [
        "cut,elevation_deg,waveform,channel,super_res,surv_prf,surv_pulses,azimuth_rate_deg_s,doppler_prf,doppler_pulses,supplemental"
    ]
    for cut in vcp["cuts"]:
        vcp_lines.append(
            f"{cut['cut']},{cut['elevation_deg']:.4f},{cut['waveform']},{cut['channel']},{cut['super_res']},"
            f"{cut['surv_prf']},{cut['surv_pulses']},{cut['azimuth_rate_deg_s']:.4f},{cut['doppler_prf']},"
            f"{cut['doppler_pulses']},{cut['supplemental']}"
        )
    write_bytes(base + ".vcp.csv", ("\n".join(vcp_lines) + "\n").encode())

    radial_count = sum(len(r) for r in per_chunk)
    elevations = sorted({r["elevation_number"] for radials in per_chunk for r in radials})
    entry = [
        "[[volume]]",
        f'site = "{site}"',
        f"volume_id = {rows[0]['volume_id']}",
        f'volume_time = "{rows[0]["volume_time"]:%Y-%m-%dT%H:%M:%SZ}"',
        f"vcp = {vcp['vcp']}",
        f"vcp_cuts = {len(vcp['cuts'])}",
        f"elevations_collected = {len(elevations)}",
        f"chunks = {len(rows)}",
        f"radials = {radial_count}",
        f"bytes = {sum(row['size'] for row in rows)}",
        f'start_last_modified = "{rows[0]["last_modified"]}"',
        f'end_last_modified = "{rows[-1]["last_modified"]}"',
        f'listing_url = "{BUCKET_URL}?list-type=2&prefix={site}/{rows[0]["volume_id"]}/"',
        f'captured_at = "{captured_at}"',
        f'listing = "{site}/{stem}.xml"',
        f'listing_sha256 = "{sha256_file(base + ".xml")}"',
        f'chunks_csv = "{site}/{stem}.chunks.csv"',
        f'chunks_csv_sha256 = "{sha256_file(base + ".chunks.csv")}"',
        f'vcp_csv = "{site}/{stem}.vcp.csv"',
        f'vcp_csv_sha256 = "{sha256_file(base + ".vcp.csv")}"',
        f'metpy = "{note}"',
        "",
    ]
    print(f"  {site}/{stem}: VCP {vcp['vcp']}, {len(rows)} chunks, {radial_count} radials; {note}", flush=True)
    return "\n".join(entry)


def command_capture(args):
    out_dir = args.out
    os.makedirs(out_dir, exist_ok=True)
    manifest = os.path.join(out_dir, "manifest.toml")
    captured_at = dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    entries = []
    for spec in args.sites:
        site, _, id_range = spec.partition(":")
        volumes = []
        if id_range:
            first, _, last = id_range.partition("-")
            wanted = [int(first)]
            while wanted[-1] != int(last) and len(wanted) < MAX_VOLUME_ID:
                wanted.append(next_volume_id(wanted[-1]))
            for volume_id in reversed(wanted):
                found = complete_volume(site, volume_id)
                if found is None:
                    raise SystemExit(f"{site}/{volume_id}/ is not one complete volume")
                volumes.append((volume_id, found))
            args_volumes = len(volumes)
        else:
            args_volumes = args.volumes
        ids = [] if id_range else volume_ids_newest_first(site)
        for volume_id in ids[: args.volumes + 3]:
            found = complete_volume(site, volume_id)
            if found is None:
                if volumes:
                    break  # keep the run consecutive
                continue
            volumes.append((volume_id, found))
            if len(volumes) == args.volumes:
                break
        if len(volumes) < args_volumes:
            raise SystemExit(f"{site}: only {len(volumes)} consecutive complete volumes")
        newest_first = [volume_id for volume_id, _ in volumes]
        if any(next_volume_id(b) != a for a, b in zip(newest_first, newest_first[1:])):
            raise SystemExit(f"{site}: volume ids {newest_first} are not consecutive")
        print(f"{site}: volumes {[v for v, _ in reversed(volumes)]}", flush=True)
        for _, (listing, rows) in reversed(volumes):
            entries.append(capture_volume(out_dir, site, listing, rows, args.metpy, captured_at))
    header = ""
    if not os.path.exists(manifest):
        header = (
            "# Real NEXRAD real-time chunk listings from unidata-nexrad-level2-chunks, written by\n"
            "# tools/capture_chunk_listings.py (see its docstring for the file formats). The .xml\n"
            "# files are the raw ListObjectsV2 responses; the .csv files are decoded from the\n"
            "# downloaded chunks, which are not committed (they expire from the bucket).\n\n"
        )
    with open(manifest, "a", encoding="utf-8", newline="\n") as handle:
        handle.write(header + "\n".join(entries))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    survey = sub.add_parser("survey")
    survey.add_argument("--sites", nargs="*")
    survey.set_defaults(func=command_survey)
    capture = sub.add_parser("capture")
    capture.add_argument("sites", nargs="+")
    capture.add_argument("--volumes", type=int, default=3)
    capture.add_argument("--out", default="crates/recast-radar-data/tests/fixtures/chunks")
    capture.add_argument("--metpy", action="store_true")
    capture.set_defaults(func=command_capture)
    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    sys.exit(main())
