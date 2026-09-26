"""JMA polar radar GRIB2 golden from ecCodes, an independent GRIB2 reader.

Writes testdata/golden/jma/eccodes.json: for each committed single-station
JMA tar, what ecCodes 2.48.0 (the `eccodes` Python package) decodes from
every field of its GRIB2 message (one field per sweep, sections 3 to 7
repeated): the section 0 and 1 keys, the section 3 and 4 header keys, the
data representation template 5.200 keys with the level values, the section
6 bitmap indicator, and a summary of the decoded data values. The Rust
decoder is checked against it in
crates/recast-radar-io-jma/tests/eccodes_real.rs.

ecCodes has no definition for JMA's local grid definition template 3.50120
or product definition template 4.51022 and stops at the first one. This
script gives it a placeholder for each that reads nothing (a label only),
so ecCodes skips each template's body by the section length and decodes
the rest with its own definitions: nothing of the JMA format document is
transcribed here, and the template bodies are not compared. ecCodes prints
"Unable to get isGridded" once per field, because the grid type is then
unknown; the keys read here do not depend on it.

The golden also lists the key ecCodes' own definitions give each octet of
the WMO azimuth-range grid definition template 3.120 (octets 15 to 39, read
from a GRIB2 sample whose template number is set to 120), which JMA's local
template 3.50120 follows octet for octet: octets 35-38 are the offset from
the origin to the inner bound of the first bin, the JMA decoder's range
start, so the first gate's centre lies half a bin spacing beyond it.

The data values are summarised per field, not copied: the number of
missing points, a histogram of the stored level values and the sum over
points of (point index + 1) times the stored level value, where the stored
level value is round(value * 10^D), the level value as ecCodes reads it
(an unsigned integer; GRIB2 stores a negative value with its top bit set).

    python tools/jma_eccodes_golden.py [--check]

--check writes nothing and exits with status 1 unless the committed file
is reproduced byte for byte.
"""

import argparse
import hashlib
import json
import os
import sys
import tarfile
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "testdata" / "golden" / "jma" / "eccodes.json"
ECCODES_VERSION = "2.48.0"
FIXTURES = {
    "jma-n5-20191012-090000-rs47773": (
        "files/other/jma/Z__C_RJTD_20191012090000_RDR_JMAGPV_N5_grib2.RS47773.tar",
        "01def3095deefd9545142d94f1b9f0495a37c403a22fdf3ef5ba09a7d9cd57bb",
    ),
    "jma-n6-20191012-090000-rs47773": (
        "files/other/jma/Z__C_RJTD_20191012090000_RDR_JMAGPV_N6_grib2.RS47773.tar",
        "c087c9e14d6d6e6083a1e8ae0e1630b2947a4433ee0ae7e68c27bd4bac24487e",
    ),
}
# Placeholder local templates: a label, no octets read.
PLACEHOLDERS = {
    "grib2/local/rjtd/template.3.50120.def": 'label "jma_placeholder_3_50120";\n',
    "grib2/local/rjtd/template.4.51022.def": 'label "jma_placeholder_4_51022";\n',
}
MESSAGE_KEYS = [
    "discipline",
    "editionNumber",
    "centre",
    "subCentre",
    "tablesVersion",
    "localTablesVersion",
    "significanceOfReferenceTime",
    "year",
    "month",
    "day",
    "hour",
    "minute",
    "second",
    "productionStatusOfProcessedData",
    "typeOfProcessedData",
    "grib2LocalSectionPresent",
]
FIELD_KEYS = [
    "sourceOfGridDefinition",
    "numberOfDataPoints",
    "numberOfOctectsForNumberOfPoints",
    "interpretationOfNumberOfPoints",
    "gridDefinitionTemplateNumber",
    "NV",
    "productDefinitionTemplateNumber",
    "numberOfValues",
    "dataRepresentationTemplateNumber",
    "bitsPerValue",
    "maxLevelValue",
    "numberOfLevelValues",
    "decimalScaleFactor",
    "bitMapIndicator",
]


def definitions_dir():
    base = Path(tempfile.mkdtemp(prefix="jma_eccodes_defs_"))
    for relative, text in PLACEHOLDERS.items():
        path = base / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    return base


def member_bytes(entry_id):
    relative, sha256 = FIXTURES[entry_id]
    path = ROOT / "testdata" / relative
    data = path.read_bytes()
    if hashlib.sha256(data).hexdigest() != sha256:
        sys.exit(f"{entry_id}: sha256 mismatch for {path}")
    with tarfile.open(path) as tar:
        members = [m for m in tar.getmembers() if m.isfile()]
        if len(members) != 1:
            sys.exit(f"{entry_id}: expected one member, found {len(members)}")
        return members[0].name, tar.extractfile(members[0]).read()


def fields(eccodes, message):
    """Every field of the GRIB2 message, with multi-field support on."""
    eccodes.codes_grib_multi_support_on()
    out = []
    with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as directory:
        name = Path(directory) / "message.grib2"
        name.write_bytes(message)
        with open(name, "rb") as stream:
            while True:
                gid = eccodes.codes_grib_new_from_file(stream)
                if gid is None:
                    break
                try:
                    out.append(field(eccodes, gid))
                finally:
                    eccodes.codes_release(gid)
            eccodes.codes_grib_multi_support_reset_file(stream)
    return out


def field(eccodes, gid):
    record = {"message": {}, "field": {}}
    for key in MESSAGE_KEYS:
        record["message"][key] = eccodes.codes_get_long(gid, key)
    for key in FIELD_KEYS:
        record["field"][key] = eccodes.codes_get_long(gid, key)
    record["field"]["levelValues"] = [
        int(v) for v in eccodes.codes_get_array(gid, "levelValues")
    ]
    scale = record["field"]["decimalScaleFactor"]
    missing_value = eccodes.codes_get_double(gid, "missingValue")
    values = eccodes.codes_get_values(gid)
    histogram = {}
    weighted = 0
    missing = 0
    for index, value in enumerate(values):
        if value == missing_value:
            missing += 1
            continue
        level = int(round(float(value) * 10**scale))
        histogram[level] = histogram.get(level, 0) + 1
        weighted += (index + 1) * level
    if missing != eccodes.codes_get_long(gid, "numberOfMissing"):
        sys.exit("missing count disagrees with ecCodes' numberOfMissing")
    record["values"] = {
        "count": len(values),
        "missing": missing,
        "histogram": [[level, histogram[level]] for level in sorted(histogram)],
        "weighted_sum": str(weighted),
    }
    return record


def template_3_120_octets(eccodes):
    """ecCodes' key at each octet 15 to 39 of grid definition template 3.120
    (the first key in its iteration order where several share an octet)."""
    gid = eccodes.codes_grib_new_from_samples("GRIB2")
    try:
        eccodes.codes_set(gid, "gridDefinitionTemplateNumber", 120)
        start = eccodes.codes_get_offset(gid, "section3Length")
        names = {}
        iterator = eccodes.codes_keys_iterator_new(gid)
        try:
            while eccodes.codes_keys_iterator_next(iterator):
                key = eccodes.codes_keys_iterator_get_name(iterator)
                try:
                    octet = eccodes.codes_get_offset(gid, key) - start + 1
                except eccodes.CodesInternalError:
                    continue
                if 15 <= octet <= 39 and not key.endswith("InDegrees") and key not in (
                        "gridDefinitionDescription", "isGridded"):
                    names.setdefault(str(octet), key)
        finally:
            eccodes.codes_keys_iterator_delete(iterator)
        return names
    finally:
        eccodes.codes_release(gid)


def build():
    os.environ["ECCODES_EXTRA_DEFINITION_PATH"] = str(definitions_dir())
    import eccodes  # noqa: E402  (the definition path must be set first)

    version = eccodes.codes_get_api_version()
    if version != ECCODES_VERSION:
        sys.exit(f"ecCodes {ECCODES_VERSION} required, found {version}")
    document = {
        "generator": "tools/jma_eccodes_golden.py",
        "eccodes": version,
        "placeholder_templates": sorted(PLACEHOLDERS),
        "wmo_grid_template_3_120_octets": template_3_120_octets(eccodes),
        "files": {},
    }
    for entry_id, (_, sha256) in FIXTURES.items():
        member, message = member_bytes(entry_id)
        document["files"][entry_id] = {
            "sha256": sha256,
            "member": member,
            "fields": fields(eccodes, message),
        }
    return render(document)


def render(document):
    """Indented JSON with each field on one line."""
    compact = {"separators": (",", ":"), "sort_keys": True}
    lines = ["{"]
    for key, value in sorted(document.items()):
        if key != "files":
            lines.append(f" {json.dumps(key)}: {json.dumps(value, **compact)},")
    lines.append(' "files": {')
    files = sorted(document["files"].items())
    for index, (entry_id, entry) in enumerate(files):
        lines.append(f"  {json.dumps(entry_id)}: {{")
        lines.append(f'   "member": {json.dumps(entry["member"])},')
        lines.append(f'   "sha256": {json.dumps(entry["sha256"])},')
        lines.append('   "fields": [')
        for number, field in enumerate(entry["fields"]):
            comma = "," if number + 1 < len(entry["fields"]) else ""
            lines.append(f"    {json.dumps(field, **compact)}{comma}")
        lines.append("   ]")
        lines.append("  }" + ("," if index + 1 < len(files) else ""))
    lines.append(" }")
    lines.append("}")
    text = "\n".join(lines) + "\n"
    json.loads(text)
    return text


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    text = build()
    if args.check:
        if not OUT.exists() or OUT.read_text() != text:
            sys.exit(f"{OUT} is not reproduced")
        print(f"{OUT} reproduced")
        return
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(text, newline="\n")
    print(f"wrote {OUT}")


if __name__ == "__main__":
    main()
