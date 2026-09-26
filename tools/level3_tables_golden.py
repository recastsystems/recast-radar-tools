#!/usr/bin/env python3
"""Golden values for the Level III storm attribute tables.

Usage (from the workspace root)::

    python tools/level3_tables_golden.py [--check]

Writes ``testdata/level3/golden-tables.json``. Requires MetPy 1.7.1 (the
reference venv).

The page text comes from MetPy's ``Level3File`` (``tab_pages`` and the text
packets of ``graph_pages``), and for the stand-alone alphanumeric products
101-104, which MetPy cannot read, from the page block at the symbology offset
read here (ICD 2620001 Figure 3-16, with ``tools/level3_golden.py``'s framing
and page readers); the rows are read from it with one regular expression per
table layout (ICD 2620003AE Appendix C tabular formats and Appendix B Format
III), independently of the Rust decoder. Products 101, 102, 103 and 104
carry the pages of 58, 59, 60 and 61 and are read as those; products 35, 36
and 39 carry the combined attribute table of 37 and 38:

* product 58, tabular pages titled ``STORM POSITION/FORECAST``:
  ``[id, az, ran, motion, f15, f30, f45, f60, err, mean]``, motion
  ``"NEW"`` or ``[dir, speed]``, forecasts ``null`` for ``NO DATA`` or
  ``[az, ran]``; plus ``average_speed`` and ``average_direction``;
* product 59, tabular pages with ``PROBABILITY OF``: ``[id, posh, poh,
  [size, q]]``, ``null`` for ``UNKNOWN``;
* product 61, tabular pages titled ``Tornado Vortex Signature``:
  ``[type, id, az, ran, avgdv, lldv, mxdv, mxdv_hgt, [depth, q],
  [base, q], [top, q], mxshr, mxshr_hgt]``, ``q`` the ``<``/``>``
  qualifier or ``""``;
* product 141, tabular pages titled ``MESOCYCLONE DETECTION``:
  ``[circ, az, ran, sr, sr_type, stm, rv, dv, [base, q], [depth, q], stmrel, hgt,
  mxrv, tvs, motion, msi]``, motion ``null`` when blank;
* products 37 and 38, graphic page text packets after the ``STM ID``
  heading: ``[id, az, ran, tvs, mda, posh, poh, size, vil, dbzm, hgt,
  [top, q], motion, meso]``, ``null`` for ``NONE`` / ``NO`` / ``UNKNOWN``;
  ``mda`` the rank of an ``MDA`` column, ``meso`` the text of a ``MESO``
  column (1997-2003: ``YES``, ``MESO``, ``3DCO``, ``UNCO``).

* product 60, tabular pages with ``MESOCYCLONE``: ``[feature, storm, type,
  tvs, base, top, az, ran, hgt, rad, azdiam, shear]``, ``tvs`` the TVS ID
  column of the 1995-1997 layout or ``null`` when blank or absent.

``legacy`` (products 37, 38, 58, 59 and 61) reads the tables of the algorithms the
SCIT, HDA and TDA replaced (1995-1997), whose layouts follow their column
headings:

* product 58, tabular pages with ``TRACKVAR``, two lines per storm:
  ``[id, az, ran, motion, speed_x, speed_y, f15, f30, f45, f60, err, mean,
  trackvar_x, trackvar_y]``, forecasts ``null`` for ``NO DAT`` or ``[x, y]``;
  plus ``average_speed`` and ``average_direction`` (always ``null``: the page
  has none);
* product 59, tabular pages with ``HAIL-WEIGHT``: ``[id, status, positive,
  probable, confidence, score]``; plus ``average_speed`` and
  ``average_direction`` (``AVG. SPEED``, ``AVG. DIRECTION``);
* product 61, tabular pages with ``MAX SHEAR HGT``, or none when the
  adaptation page has ``SEARCH PERCENTAGE`` (no TVS): ``[tvs, meso, storm,
  base_hgt, az, ran, max_shear_hgt, az, ran, shear, ori, rot]``;
* products 37 and 38 whose ``STM ID`` heading has ``MW VOL`` (1995-1996):
  ``[id, az, ran, tvs, meso, hail, dbzm, hgt, vlow, [top, q], dir, speed,
  mwvol]``, ``tvs`` and ``meso`` booleans for ``YES`` / ``NO``.

``table`` is ``null`` when the product has no page of that layout (the
1990s products; for product 61, a product of the legacy algorithm), and
``legacy`` when the product is not of the legacy layout.
"""

import argparse
import json
import re
import sys
import tomllib
from datetime import datetime, timedelta
from pathlib import Path

import metpy.io.nexrad as nx
from metpy.io import Level3File

sys.path.insert(0, str(Path(__file__).resolve().parent))
import level3_golden as lg  # noqa: E402

nx.nexrad_to_datetime = lambda d, ms: datetime(1970, 1, 1) + timedelta(days=d - 1, milliseconds=ms)

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "testdata" / "level3" / "golden-tables.json"

NUM = r"-?\d+(?:\.\d+)?"
AZRAN = rf"({NUM})/\s*({NUM})"


def f(x):
    return float(x)


def q(text):
    m = re.fullmatch(rf"([<>]?)\s*({NUM})", text.strip())
    return [float(m.group(2)), m.group(1)]


def motion(text):
    text = text.strip()
    if text == "NEW":
        return "NEW"
    m = re.fullmatch(AZRAN, text)
    return [f(m.group(1)), f(m.group(2))]


def pages_with(pages, title):
    return [p for p in pages if title.upper() in p.upper()]


def sti(fobj):
    pages = pages_with(fobj.tab_pages, "STORM POSITION/FORECAST")
    if not pages:
        return None
    rows, speed, direction = [], None, None
    pos = rf"(?:NO DATA|{NUM}/\s*{NUM})"
    row = re.compile(
        rf"^\s*(\S\S)\s+{AZRAN}\s+(NEW|{NUM}/\s*{NUM})\s+({pos})\s+({pos})\s+({pos})\s+({pos})\s+({NUM})/\s*({NUM})\s*$")
    for page in pages:
        m = re.search(r"AVG SPEED\s+(\d+)", page)
        if m:
            speed = f(m.group(1))
        m = re.search(r"AVG DIRECTION\s+(\d+)", page)
        if m:
            direction = f(m.group(1))
        for line in page.splitlines():
            m = row.match(line)
            if not m:
                continue
            fc = []
            for k in range(5, 9):
                g = m.group(k)
                fc.append(None if g.startswith("NO") else [f(x) for x in re.split(r"/\s*", g)])
            rows.append([m.group(1), f(m.group(2)), f(m.group(3)), motion(m.group(4))] + fc +
                        [f(m.group(9)), f(m.group(10))])
    return {"average_speed": speed, "average_direction": direction, "rows": rows}


def hail(fobj):
    pages = pages_with(fobj.tab_pages, "PROBABILITY OF")
    if not pages:
        return None
    val = r"(\d+|UNKNOWN)"
    row = re.compile(rf"^\s*(\S\S)\s+{val}\s+{val}\s+([<>]?\s*\d+\.\d+|UNKNOWN)\s*$")
    rows = []
    for page in pages:
        for line in page.splitlines():
            m = row.match(line)
            if m:
                conv = lambda g, fn: None if g == "UNKNOWN" else fn(g)
                rows.append([m.group(1), conv(m.group(2), int), conv(m.group(3), int),
                             conv(m.group(4), q)])
    return {"rows": rows}


def legacy_tvs_product(fobj):
    return bool(pages_with(fobj.tab_pages, "MAX SHEAR HGT") or
                pages_with(fobj.tab_pages, "SEARCH PERCENTAGE"))


def tvs(fobj):
    pages = pages_with(fobj.tab_pages, "Tornado Vortex Signature")
    if not pages or legacy_tvs_product(fobj):
        return None
    qn = rf"[<>]?\s*{NUM}"
    row = re.compile(
        rf"^\s*(TVS|ETVS)\s+(\S\S)\s+{AZRAN}\s+({NUM})\s+({NUM})\s+({NUM})/\s*({NUM})\s+({qn})\s+({qn})/\s*({qn})\s+({NUM})/\s*({NUM})\s*$")
    rows = []
    for page in pages:
        for line in page.splitlines():
            m = row.match(line)
            if m:
                g = m.groups()
                rows.append([g[0], g[1], f(g[2]), f(g[3]), f(g[4]), f(g[5]), f(g[6]), f(g[7]),
                             q(g[8]), q(g[9]), q(g[10]), f(g[11]), f(g[12])])
    return {"rows": rows}


def mda(fobj):
    pages = pages_with(fobj.tab_pages, "MESOCYCLONE DETECTION")
    if not pages:
        return None
    qn = rf"[<>]?\s*{NUM}"
    row = re.compile(
        rf"^\s*(\d+)\s+{AZRAN}\s+(\d+)([LS]?)\s+(\S\S)\s+({NUM})\s+({NUM})\s+({qn})\s+({qn})\s+({NUM})\s+({NUM})\s+({NUM})\s+([YN])\s+(?:({NUM}/\s*{NUM})\s+)?(\d+)\s*$")
    rows = []
    for page in pages:
        for line in page.splitlines():
            m = row.match(line)
            if m:
                g = m.groups()
                rows.append([int(g[0]), f(g[1]), f(g[2]), int(g[3]), g[4], g[5], f(g[6]),
                             f(g[7]), q(g[8]), q(g[9]), f(g[10]), f(g[11]), f(g[12]),
                             g[13] == "Y", motion(g[14]) if g[14] else None, int(g[15])])
    return {"rows": rows}


def graphic_lines(fobj):
    return [p["text"] for page in fobj.graph_pages for p in page if "text" in p]


def attributes(fobj):
    lines = graphic_lines(fobj)
    headings = [line for line in lines if "STM ID" in line]
    if not headings or any("MW VOL" in line for line in headings):
        return None
    hail_part = r"(?:(\d+)/\s*(\d+)/\s*([<>]?\s*\d+\.\d+)|(UNKNOWN))"
    row = re.compile(
        rf"^\s*(\S\S)\s+{AZRAN}\s+(TVS|ETVS|NONE|NO|YES)\s+(\d+|NONE|NO|YES|MESO|3DCO|UNCO)\s+{hail_part}\s+({NUM})\s+({NUM})\s+({NUM})\s+([<>]?\s*{NUM})\s+(NEW|{NUM}/\s*{NUM})\s*$")
    rows = []
    for line in lines:
        m = row.match(line)
        if not m:
            continue
        g = m.groups()
        unknown = g[8] is not None
        mda = int(g[4]) if g[4].isdigit() else None
        meso = None if g[4] in ("NONE", "NO") or g[4].isdigit() else g[4]
        rows.append([g[0], f(g[1]), f(g[2]), None if g[3] in ("NONE", "NO") else g[3], mda,
                     None if unknown else int(g[5]), None if unknown else int(g[6]),
                     None if unknown else q(g[7]),
                     f(g[9]), f(g[10]), f(g[11]), q(g[12]), motion(g[13]), meso])
    return {"rows": rows}


def legacy_attributes(fobj):
    lines = graphic_lines(fobj)
    if not any("STM ID" in line and "MW VOL" in line for line in lines):
        return None
    row = re.compile(
        rf"^\s*{SID}\s+(\d+)\s+(\d+)\s+(YES|NO)\s+(YES|NO)\s+([A-Z]+)\s+({NUM})\s+({NUM})\s+({NUM})\s+([<>]?{NUM})\s+(\d+)\s+(\d+)\s+(\d+)\s*$")
    rows = []
    for line in lines:
        m = row.match(line)
        if m:
            g = m.groups()
            rows.append([g[0], f(g[1]), f(g[2]), g[3] == "YES", g[4] == "YES", g[5], f(g[6]), f(g[7]),
                         f(g[8]), q(g[9]), f(g[10]), f(g[11]), f(g[12])])
    return {"rows": rows}


NUMD = r"-?(?:\d+(?:\.\d+)?|\.\d+)"
SID = r"([A-Z0-9]{1,2})"


def legacy_sti(fobj):
    pages = pages_with(fobj.tab_pages, "TRACKVAR")
    if not pages:
        return None
    fc = rf"(NO DAT|{NUM})"
    first = re.compile(
        rf"^\s*{SID}\s+{AZRAN}\s+({NUM}/\s*{NUM})\s+({NUM})\s+{fc}\s+{fc}\s+{fc}\s+{fc}\s+({NUM})/\s*({NUM})\s+({NUM})\s*$")
    second = re.compile(rf"^\s*({NUM})\s+{fc}\s+{fc}\s+{fc}\s+{fc}\s+({NUM})\s*$")
    rows = []
    for page in pages:
        lines = page.splitlines()
        for a, b in zip(lines, lines[1:]):
            m1, m2 = first.match(a), second.match(b)
            if not (m1 and m2):
                continue
            g1, g2 = m1.groups(), m2.groups()
            fcs = []
            for k in range(4):
                x, y = g1[5 + k], g2[1 + k]
                fcs.append(None if x == "NO DAT" and y == "NO DAT" else [f(x), f(y)])
            rows.append([g1[0], f(g1[1]), f(g1[2]), motion(g1[3]), f(g1[4]), f(g2[0])] + fcs +
                        [f(g1[9]), f(g1[10]), f(g1[11]), f(g2[5])])
    return {"average_speed": None, "average_direction": None, "rows": rows}


def legacy_hail(fobj):
    pages = pages_with(fobj.tab_pages, "HAIL-WEIGHT")
    if not pages:
        return None
    row = re.compile(rf"^\s*{SID}\s+([A-Z]+(?: [A-Z]+)*)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s*$")
    rows, speed, direction = [], None, None
    for page in pages:
        m = re.search(r"AVG\. SPEED\s+(\d+)", page)
        if m:
            speed = f(m.group(1))
        m = re.search(r"AVG\. DIRECTION\s+(\d+)", page)
        if m:
            direction = f(m.group(1))
        for line in page.splitlines():
            m = row.match(line)
            if m:
                g = m.groups()
                rows.append([g[0], g[1], f(g[2]), f(g[3]), f(g[4]), f(g[5])])
    return {"average_speed": speed, "average_direction": direction, "rows": rows}


def legacy_tvs(fobj):
    if not legacy_tvs_product(fobj):
        return None
    row = re.compile(
        rf"^\s*(\d+)\s+(\d+)\s+{SID}\s+({NUM})\s+{AZRAN}\s+({NUM})\s+{AZRAN}\s+({NUM})\s+({NUMD})\s+({NUMD})\s*$")
    rows = []
    for page in pages_with(fobj.tab_pages, "MAX SHEAR HGT"):
        for line in page.splitlines():
            m = row.match(line)
            if m:
                g = m.groups()
                rows.append([int(g[0]), int(g[1]), g[2]] + [f(x) for x in g[3:]])
    return {"rows": rows}


def meso(fobj):
    pages = pages_with(fobj.tab_pages, "MESOCYCLONE")
    if not pages:
        return None
    row = re.compile(
        rf"^\s*(\d+)\s+-\s+{SID}\s+(MESO|3DC SHR|UNC SHR)\s+(?:(\d+)\s+)?({NUM})\s+({NUM})\s+{AZRAN}\s+({NUM})\s+({NUM})\s+({NUM})\s+({NUM})\s*$")
    rows = []
    for page in pages:
        for line in page.splitlines():
            m = row.match(line)
            if m:
                g = m.groups()
                rows.append([int(g[0]), g[1], g[2], None if g[3] is None else int(g[3])] +
                            [f(x) for x in g[4:]])
    return {"rows": rows}


READERS = {58: sti, 59: hail, 60: meso, 61: tvs, 141: mda, 101: sti, 102: hail, 103: meso,
           104: tvs, 35: attributes, 36: attributes, 37: attributes, 38: attributes,
           39: attributes}
LEGACY = {58: legacy_sti, 59: legacy_hail, 61: legacy_tvs, 101: legacy_sti, 102: legacy_hail,
          104: legacy_tvs, 35: legacy_attributes, 36: legacy_attributes,
          37: legacy_attributes, 38: legacy_attributes, 39: legacy_attributes}
STANDALONE = {101, 102, 103, 104}


class StandAlonePages:
    """``tab_pages`` of a stand-alone alphanumeric product (Figure 3-16): the
    page block at the symbology offset, pages as MetPy joins them."""

    def __init__(self, path):
        _, msg = lg.unwrap(path.read_bytes())
        sym = int.from_bytes(msg[108:112], "big")
        pages, _ = lg.read_pages(msg, 2 * sym, len(msg))
        self.tab_pages = ["\n".join(lines) for lines in pages]
        self.graph_pages = []


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    manifest = tomllib.loads((ROOT / "testdata" / "level3" / "manifest.toml").read_text())
    files = []
    for entry in manifest["file"]:
        codes = [int(t.split(":")[1]) for t in entry.get("tags", []) if re.fullmatch(r"product:\d+", t)]
        if not codes or codes[0] not in READERS:
            continue
        path = ROOT / "testdata" / entry["committed"]
        if codes[0] in STANDALONE:
            fobj = StandAlonePages(path)
        else:
            fobj = Level3File(str(path))
        item = {"id": entry["id"], "product": codes[0], "table": READERS[codes[0]](fobj)}
        if codes[0] in LEGACY:
            item["legacy"] = LEGACY[codes[0]](fobj)
        files.append(item)
    text = json.dumps({"files": files}, indent=1, sort_keys=True) + "\n"
    if args.check:
        if OUT.read_text() != text:
            sys.exit(f"{OUT} differs")
        return
    with open(OUT, "w", newline="\n") as out:
        out.write(text)
    for item in files:
        table = item["table"]
        legacy = item.get("legacy")
        print(item["id"], item["product"], None if table is None else len(table["rows"]),
              None if legacy is None else len(legacy["rows"]))


if __name__ == "__main__":
    main()
