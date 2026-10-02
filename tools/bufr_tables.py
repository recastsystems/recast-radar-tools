"""Generate the BUFR tables recast-radar-io-bufr embeds.

    python tools/bufr_tables.py

Sources, pinned:

- WMO BUFR edition 4 master tables B and D, https://github.com/wmo-im/BUFR4
  (MIT), commit dcc0316bb9234684e47ae09de02a8b5204044c61 (2026-06-09).
  Table B and D entries keep their definitions across master table
  versions, so the latest serves files of every version; the entries the
  committed Meteo-France files use are the same in version 11, which they
  name.
- Meteo-France (originating centre 85) local tables B and D, versions 11
  and 12, as numpy_bufr ships them (https://github.com/Bram94/numpy_bufr,
  MIT, commit 0641ed67de0f80c358714540d6c0b3dedf24af18,
  Tables.zip: Tables/libdwd/local_00085_00000/table_{b,d}_0{11,12}).

Output, under crates/recast-radar-io-bufr/tables/:

- `*_b.tsv`: `FXY<TAB>scale<TAB>reference<TAB>width<TAB>unit<TAB>name`, one
  element per line; `unit` is `CCITT IA5` for character data.
- `*_d.tsv`: `FXY<TAB>FXY FXY ...`, one sequence per line.
"""

from __future__ import annotations

import csv
import io
import pathlib
import urllib.request
import zipfile

WMO_COMMIT = "dcc0316bb9234684e47ae09de02a8b5204044c61"
NUMPY_BUFR_COMMIT = "0641ed67de0f80c358714540d6c0b3dedf24af18"
OUT = pathlib.Path(__file__).resolve().parent.parent / "crates" / "recast-radar-io-bufr" / "tables"

B_CLASSES = ["00", "01", "02", "03", "04", "05", "06", "07", "08", "10", "11", "12", "13", "14",
             "15", "19", "20", "21", "22", "23", "24", "25", "26", "27", "28", "29", "30", "31",
             "33", "35", "40", "41", "42"]
D_CATEGORIES = ["00", "01", "02", "03", "04", "05", "06", "07", "08", "09", "10", "11", "12",
                "13", "14", "15", "16", "18", "21", "22", "25", "40"]


def fetch(url: str) -> bytes:
    with urllib.request.urlopen(url, timeout=60) as response:
        return response.read()


def clean(text: str) -> str:
    return " ".join(text.replace("\t", " ").split())


def wmo_tables() -> tuple[list[str], list[str]]:
    base = f"https://raw.githubusercontent.com/wmo-im/BUFR4/{WMO_COMMIT}"
    elements: dict[str, str] = {}
    for cls in B_CLASSES:
        rows = csv.DictReader(io.StringIO(fetch(f"{base}/BUFRCREX_TableB_en_{cls}.csv").decode("utf-8")))
        for row in rows:
            elements[row["FXY"]] = "\t".join([
                row["FXY"], row["BUFR_Scale"], row["BUFR_ReferenceValue"],
                row["BUFR_DataWidth_Bits"], clean(row["BUFR_Unit"]), clean(row["ElementName_en"]),
            ])
    sequences: dict[str, list[str]] = {}
    for cat in D_CATEGORIES:
        try:
            text = fetch(f"{base}/BUFR_TableD_en_{cat}.csv").decode("utf-8")
        except OSError:
            continue
        for row in csv.DictReader(io.StringIO(text)):
            sequences.setdefault(row["FXY1"], []).append(row["FXY2"])
    b = [elements[key] for key in sorted(elements)]
    d = [f"{key}\t{' '.join(value)}" for key, value in sorted(sequences.items())]
    return b, d


def meteofrance_tables(version: int, archive: zipfile.ZipFile) -> tuple[list[str], list[str]]:
    prefix = "Tables/libdwd/local_00085_00000"
    b = []
    for line in archive.read(f"{prefix}/table_b_{version:03d}").decode("latin1").splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        fxy, kind, unit, scale, reference, width, name = line.split("\t")[:7]
        unit = "CCITT IA5" if kind == "A" else clean(unit)
        b.append("\t".join([fxy, scale, reference, width, unit, clean(name)]))
    d: dict[str, list[str]] = {}
    current = None
    for line in archive.read(f"{prefix}/table_d_{version:03d}").decode("latin1").splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        head, member = line.split("\t")[:2]
        if head:
            current = head
            d[current] = []
        d[current].append(member.strip())
    return sorted(b), [f"{key}\t{' '.join(value)}" for key, value in sorted(d.items())]


def write(name: str, lines: list[str]) -> None:
    path = OUT / name
    path.write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")
    print(f"{path.relative_to(OUT.parent.parent.parent)}: {len(lines)} lines")


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    b, d = wmo_tables()
    write("wmo_b.tsv", b)
    write("wmo_d.tsv", d)
    archive = zipfile.ZipFile(io.BytesIO(fetch(
        f"https://github.com/Bram94/numpy_bufr/raw/{NUMPY_BUFR_COMMIT}/Tables.zip")))
    for version in (11, 12):
        b, d = meteofrance_tables(version, archive)
        write(f"meteofrance_v{version}_b.tsv", b)
        write(f"meteofrance_v{version}_d.tsv", d)


if __name__ == "__main__":
    main()
