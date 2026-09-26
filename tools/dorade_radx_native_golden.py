"""DORADE descriptor golden from LROSE RadxPrint -native, an independent reader.

Writes testdata/golden/dorade/radxprint_native.json.

`RadxPrint -native` prints every DORADE block of a sweepfile with LROSE's own
structure layouts (lrose-core DoradeData.hh and DoradeData.cc). For each
committed DORADE sweepfile this keeps, as printed, the members of the
descriptor blocks: SSWB (super_SWIB), VOLD (volume), RADD (radar), CFAC
(correction), CSFD (cell spacing), CELV (cell), SWIB (sweepinfo) and every
PARM (parameter). The ray blocks (RYIB, ASIB) are checked against
`RadxPrint -rays` in radxprint.json (tools/dorade_radx_golden.py).
crates/recast-radar-io-dorade/tests/descriptors_real.rs compares the model's
`dorade_<block>_<member>` values with these.

RadxPrint prints a member of a short block (the 144-byte RADD, the 104-byte
PARM) from its zeroed structure, so members past the block end read 0 or
blank here; the test compares only the members the block holds. Floats are
printed with about six significant digits, the SSWB times as UTC date and
time, and the RADD radar type and scan mode and the PARM binary format as
their enum names.

RadxPrint runs in a Docker container with LROSE installed (the `nexbench`
container of this project, LROSE release 20250811):

    python tools/dorade_radx_native_golden.py [--container nexbench] [--out PATH]

The files are copied into the container with `docker cp`; nothing is read
from this workspace's decoders.
"""

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIXTURES = {
    "dorade-cow2-20260521-225514-sur-head24": "swp.1260521225514.COW2.229.1.0_SUR_v215.head24",
    "dorade-noxp-20090501-190244-ppi": "swp.1090501190244.NOXPRVP.0.0.5_PPI_v1",
    "dorade-noxp-20090501-190324-ppi": "swp.1090501190324.NOXPRVP.0.0.5_PPI_v1",
    "dorade-noxp-20090525-203211-sector": "swp.1090525203211.NOXPRVP.0.0.5_PPI_v1",
    "dorade-dow6-20211230-222139-rhi-head41": "swp.1211230222139.DOW6low.648.144.0_RHI_v169.head41",
    "dorade-noxp-20090610-003210-ppi-head6": "swp.1090610003210.NOXPRVP.0.0.5_PPI_v1.head6",
    "dorade-noxp-20090610-003222-ppi-head6": "swp.1090610003222.NOXPRVP.0.1.0_PPI_v1.head6",
    "dorade-noxp-20090610-003226-ppi-head6": "swp.1090610003226.NOXPRVP.0.2.0_PPI_v1.head6",
    "dorade-n42rf-ts-20181010-122951-air-head24":
        "swp.1181010122951.N42RF-TS.196.-20.0_AIR_v3394.head24",
    "dorade-n42rf-tm-20181010-123925-air-head48":
        "swp.1181010123925.N42RF-TM.137.20.0_AIR_v3532.head48",
}
# RadxPrint section titles (after "DoradeData ") and the block they print.
SECTIONS = {
    "super_SWIB": "sswb",
    "super_SWIB_32bit": "sswb",
    "volume": "vold",
    "radar": "radd",
    "correction": "cfac",
    "cell spacing": "csfd",
    "cell": "celv",
    "sweepinfo": "swib",
    "parameter": "parm",
}
HEADER = re.compile(r"^=+ DoradeData (.+?) =+$")


def run(cmd):
    return subprocess.run(cmd, check=True, capture_output=True, text=True).stdout


def radxprint_native(container, path):
    remote = f"/tmp/dorade_radx_golden/{path.name}"
    run(["docker", "exec", container, "mkdir", "-p", "/tmp/dorade_radx_golden"])
    run(["docker", "cp", str(path), f"{container}:{remote}"])
    return run([
        "docker", "exec", container, "bash", "-c",
        "export PATH=/usr/local/lrose/bin:$PATH LD_LIBRARY_PATH=/usr/local/lrose/lib; "
        f"RadxPrint -f '{remote}' -native",
    ])


def parse(output):
    """The descriptor blocks: one dict of member -> printed text per block
    (a list of dicts for PARM), the SSWB key tables as a list and the CELV
    cell distances as a list."""
    blocks = {}
    current = None
    for line in output.splitlines():
        match = HEADER.match(line.strip())
        if match:
            name = SECTIONS.get(match.group(1))
            current = None
            if name == "parm":
                current = {}
                blocks.setdefault("parm", []).append(current)
            elif name is not None:
                if name in blocks:
                    raise ValueError(f"second {name} block")
                current = blocks[name] = {}
            continue
        if line.startswith("====="):
            current = None
            continue
        if current is None or ":" not in line:
            continue
        key, _, text = line.strip().partition(":")
        text = text.strip()
        if key == "Key table num":
            current.setdefault("key_tables", []).append({})
            continue
        if line.startswith("    ") and current.get("key_tables"):
            current["key_tables"][-1][key] = text
            continue
        if key.startswith("cell distance["):
            current.setdefault("cell_distances", []).append(text)
            continue
        if key in current:
            raise ValueError(f"repeated member {key}")
        current[key] = text
    for required in ("sswb", "vold", "radd", "swib", "parm"):
        if required not in blocks:
            raise ValueError(f"no {required} block")
    return blocks


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--container", default="nexbench")
    parser.add_argument(
        "--out", default=str(ROOT / "testdata/golden/dorade/radxprint_native.json")
    )
    args = parser.parse_args()
    files = {}
    for entry, name in FIXTURES.items():
        path = ROOT / "testdata/files/other/dorade" / name
        files[entry] = parse(radxprint_native(args.container, path))
        print(f"{entry}: {sorted(files[entry])}", file=sys.stderr)
    golden = {
        "source": "LROSE RadxPrint -native (release 20250811), tools/dorade_radx_native_golden.py",
        "note": "members as RadxPrint prints them: floats with about six significant digits, "
                "SSWB times as UTC 'YYYY/MM/DD hh:mm:ss', enum names for radar_type, scan_mode "
                "and binary_format; members past the end of a short block print as 0 or blank",
        "files": files,
    }
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(golden, indent=1) + "\n", encoding="utf-8", newline="\n")


if __name__ == "__main__":
    main()
