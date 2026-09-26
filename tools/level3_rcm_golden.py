#!/usr/bin/env python3
"""Golden values for Level III Radar Coded Messages (product 74, and the
radar coded message inside the unedited Radar Coded Message, product 83).

Usage (from the workspace root)::

    python tools/level3_rcm_golden.py [--check]

Reads every product-74 and product-83 file of
``testdata/level3/manifest.toml`` and writes ``testdata/level3/golden-rcm.json``.
Standard library only.

A second reading of ICD 2620001P Appendix B (and 2620001H Appendix B for the
remark groups) by the Rust decoder's author, since no third-party decoder of
the groups exists (MetPy only splits the three parts), written with regular
expressions over the message text: the text is located through the Product Description Block's symbology
offset (halfwords 55-56), the three parts are cut at ``/NEXRAA``,
``/NEXRBB`` and ``/NEXRCC``, and every list is read with its whitespace
removed (the message is a sequence of 70-character records that split
groups and pad with spaces).

Sub-box lettering of the 1/16 LFM grid: ``A``-``D`` down the western column
of a box, ``E``-``H`` the next, and so on. Appendix B gives the lettering
only as a picture (Figure B-1); this is the lettering under which the
intensity groups of every corpus message run north to south and west to
east without overlapping (the other candidate, row-major, gives 23-35 order
inversions per message), as Appendix B requires. ``order_inversions``
records the count under the chosen lettering (0).

Schema per file (``files[i]``): ``id``, ``node``, ``category``, ``site``;
``part_a``: ``site``, ``time`` (ISO, UTC), ``status``, ``radne``,
``radom``, ``mode``, ``scan_strategy``, ``ni`` (the ``/NI`` count),
``cells`` (levels in the groups), ``nonzero`` (levels above 0: the ``/NI``
count in every corpus message), ``groups``, ``grid_sha256`` (SHA-256 of
the 100 x 100 fine grid as bytes, row-major from the north-west, 0 where not
reported), ``histogram`` (level -> fine boxes), ``max_top`` (``[height,
row, column]``), ``ncen``, ``centroids`` (``[id, row, column, direction,
speed]``); ``part_b``: ``site``, ``time``, ``vadna``, ``winds`` (``[height,
confidence, direction, speed]``); ``part_c``: ``site``, ``time``,
``ntvs``, ``tvs`` (``[number, row, column]``), ``nmes``, ``mesocyclones``,
``ncen``, ``storm_tops`` (``[id, row, column, top, hail]``), ``remarks``.
Fine rows and columns count from the north-west corner (0-99).

Product 83 (``IRM``, no ICD; ``docs/level3/reference.md`` section 4.4): the
message text is read from the Tabular Alphanumeric Block, after its block
header and the second Message Header and Product Description Blocks (those of
a product 74). ``irm_grid_sha256`` is the SHA-256 of the 100 x 100 grid of
the last symbology layer's packet 32, read here from its bytes (code, row
count, then per row a byte count and ``run << 4 | level`` bytes), to compare
with ``part_a.grid_sha256``. A product 83 whose tabular offset names the end
of its message carries no message text (KLOT 1994): its entry holds only
``id``, ``irm_grid_sha256`` and ``no_message: true``.
"""

import argparse
import hashlib
import json
import re
import struct
import sys
import tomllib
from datetime import datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "testdata" / "level3" / "golden-rcm.json"


def message_bytes(data):
    """The message after the WMO heading and AWIPS line (no zlib in the corpus RCMs)."""
    m = re.match(rb"(\x01\r\r\n\d{3,5} ?\r\r\n)?[A-Z]{4}\d\d [A-Z]{4} \d{6}( [A-Z]{3})?\r\r\n([A-Z0-9]{3,6} *\r\r\n)?", data)
    return data[m.end():] if m else data


def box(text):
    r, c, s = (ord(ch) - 65 for ch in text)
    return [r * 4 + s % 4, c * 4 + s // 4]


def when(text):
    m = re.fullmatch(r"(\d\d)(\d\d)(\d\d)(\d\d)(\d\d)", text or "")
    if not m:
        return None
    d, mo, y, h, mi = (int(g) for g in m.groups())
    year = 1900 + y if y >= 70 else 2000 + y
    try:
        return datetime(year, mo, d, h, mi).isoformat()
    except ValueError:
        return None


def squeeze(text):
    return re.sub(r"\s+", "", text)


def part(body, marker):
    m = re.search(re.escape(marker) + r"(.*?)(?=/END|/NEXR|$)", body, re.S)
    return m.group(1) if m else None


def head(text):
    m = re.match(r"\s*(\S+)\s+(\d{10})?", text)
    return (m.group(1), when(m.group(2)), text[m.end():]) if m else (None, None, text)


def part_a(text):
    site, time, rest = head(text)
    out = {"site": site, "time": time}
    pre = rest.split("/", 1)[0].split()
    out["status"] = next((w for w in pre if w not in ("RADNE", "RADOM")), "")
    out["radne"] = "RADNE" in pre
    out["radom"] = "RADOM" in pre
    m = re.search(r"/MD(\S*)", rest)
    out["mode"] = m.group(1) if m else None
    m = re.search(r"/SC([^/]*)", rest)
    out["scan_strategy"] = m.group(1).strip() if m else None
    m = re.search(r"/NI(\d+):([^/]*)", rest)
    out["ni"] = int(m.group(1)) if m else None
    grid = bytearray(100 * 100)
    cells = 0
    nonzero = 0
    starts = []
    groups = [g for g in squeeze(m.group(2)).split(",") if g] if m else []
    for g in groups:
        row, col = box(g[:3])
        starts.append((row, col))
        levels = []
        for ch in g[3:]:
            if ch.isdigit():
                levels.append(int(ch))
            else:
                levels.extend([levels[-1]] * (ord(ch) - 64))
        for k, level in enumerate(levels):
            if col + k < 100:
                grid[row * 100 + col + k] = level
        cells += len(levels)
        nonzero += sum(1 for level in levels if level)
    out["groups"] = len(groups)
    out["cells"] = cells
    out["nonzero"] = nonzero
    out["order_inversions"] = sum(1 for a, b in zip(starts, starts[1:]) if b < a)
    out["grid_sha256"] = hashlib.sha256(bytes(grid)).hexdigest()
    hist = {}
    for v in grid:
        if v:
            hist[str(v)] = hist.get(str(v), 0) + 1
    out["histogram"] = hist
    m = re.search(r"/MT(\d+):\s*([A-Y]{2}[A-P])", rest)
    out["max_top"] = [int(m.group(1))] + box(m.group(2)) if m else None
    m = re.search(r"/NCEN(\d+):([^/]*)", rest)
    out["ncen"] = int(m.group(1)) if m else None
    out["centroids"] = []
    if m:
        for c in re.finditer(r"C(..)([A-Y]{2}[A-P])\s*(\d{3})(\d{3})", m.group(2)):
            out["centroids"].append([c.group(1).strip()] + box(c.group(2)) + [int(c.group(3)), int(c.group(4))])
    return out


def part_b(text):
    site, time, rest = head(text)
    flat = squeeze(rest)
    winds = [[int(h), c, int(d), int(f)] for h, c, d, f in re.findall(r"(\d{3})([A-Z])(\d{3})(\d{3})", flat)]
    return {"site": site, "time": time, "vadna": "VADNA" in flat, "winds": winds}


def part_c(text):
    site, time, rest = head(text)
    out = {"site": site, "time": time}
    m = re.search(r"/NTVS(\d+):([^/]*)", rest)
    out["ntvs"] = int(m.group(1)) if m else None
    out["tvs"] = [[int(n)] + box(g) for n, g in re.findall(r"TVS(\d\d)([A-Y]{2}[A-P])", squeeze(m.group(2)))] if m else []
    m = re.search(r"/NMES(\d+):([^/]*)", rest)
    out["nmes"] = int(m.group(1)) if m else None
    out["mesocyclones"] = [[int(n)] + box(g) for n, g in re.findall(r"M(\d\d)([A-Y]{2}[A-P])", squeeze(m.group(2)))] if m else []
    m = re.search(r"/NCEN(\d+):([^/]*)", rest)
    out["ncen"] = int(m.group(1)) if m else None
    out["storm_tops"] = []
    if m:
        for c in re.finditer(r"C(..)([A-Y]{2}[A-P])\s*S(\d+)H(.)", m.group(2)):
            out["storm_tops"].append([c.group(1).strip()] + box(c.group(2)) + [int(c.group(3)), c.group(4)])
    known = ("NTVS", "NMES", "NCEN")
    out["remarks"] = [" ".join(chunk.split()) for chunk in rest.split("/")[1:]
                      if chunk.strip() and not chunk.startswith(known)]
    return out


def irm_grid(m, hw):
    """The packet 32 grid of the last symbology layer of a product 83."""
    sym = 2 * ((hw[54] << 16) | hw[55])
    layers = struct.unpack(">H", m[sym + 8:sym + 10])[0]
    p = sym + 10
    for _ in range(layers):
        length = struct.unpack(">I", m[p + 2:p + 6])[0]
        layer = m[p + 6:p + 6 + length]
        p += 6 + length
    code, rows = struct.unpack(">HH", layer[:4])
    assert code == 32
    grid = bytearray()
    q = 4
    for _ in range(rows):
        n = struct.unpack(">H", layer[q:q + 2])[0]
        for byte in layer[q + 2:q + 2 + n]:
            grid += bytes([byte & 15]) * (byte >> 4)
        q += 2 + n
    return bytes(grid)


def decode(data):
    m = message_bytes(data)
    hw = struct.unpack(">60H", m[:120])
    extra = {}
    if hw[15] == 83:
        tab = 2 * ((hw[58] << 16) | hw[59])
        extra["irm_grid_sha256"] = hashlib.sha256(irm_grid(m, hw)).hexdigest()
        if tab >= len(m):
            # Observed (KLOT 1994): the tabular offset names the end of the
            # message and no block (so no message text) was sent; only the
            # packet 32 grid is recorded.
            return {"no_message": True, **extra}
        offset = tab + 8 + 120
    else:
        offset = 2 * ((hw[54] << 16) | hw[55])
    text = m[offset:].decode("latin-1")
    first = text.find("/NEXR")
    words = text[:first].split()
    out = {"node": words[0], "category": words[1], "site": words[2]}
    body = text[first:]
    for key, marker, fn in (("part_a", "/NEXRAA", part_a), ("part_b", "/NEXRBB", part_b),
                            ("part_c", "/NEXRCC", part_c)):
        p = part(body, marker)
        out[key] = fn(p) if p is not None else None
    out.update(extra)
    return out


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    manifest = tomllib.loads((ROOT / "testdata" / "level3" / "manifest.toml").read_text())
    files = []
    for entry in manifest["file"]:
        if not {"product:74", "product:83"} & set(entry.get("tags", [])):
            continue
        data = (ROOT / "testdata" / entry["committed"]).read_bytes()
        files.append({"id": entry["id"], **decode(data)})
    text = json.dumps({"files": files}, indent=1, sort_keys=True) + "\n"
    if args.check:
        if OUT.read_text() != text:
            sys.exit(f"{OUT} differs")
        return
    with open(OUT, "w", newline="\n") as f:
        f.write(text)
    for f in files:
        if "part_a" in f:
            print(f["id"], f["part_a"]["ni"], f["part_a"]["cells"], f["part_a"]["order_inversions"])
        else:
            print(f["id"], "no message")


if __name__ == "__main__":
    main()
