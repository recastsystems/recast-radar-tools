"""Level II messages 3 and 18 golden from the danielway nexrad crates.

Writes testdata/level2/golden/nexrad_crate/<id>.json for every file of the
Level II status goldens (testdata/level2/golden/status): each accessor of
the first message 3 (Performance/Maintenance Data) and message 18 (RDA
Adaptation Data) that nexrad-decode decodes from the file (read with
nexrad-data), with the byte where the nexrad crate reads it and the value
it returns. The nexrad crates (https://github.com/danielway/nexrad, pinned
at NEXRAD_REV) are a Rust Level II decoder written independently of this
workspace; crates/recast-radar-io-nexrad/tests/nexrad_crate_real.rs checks
the model against these files.

The byte of each accessor comes from the nexrad source itself: for message
3, the offset of the raw struct field the accessor returns (the struct is
`#[repr(C)]` over big-endian byte arrays, 960 bytes); for message 18, the
offset the accessor reads (`read_real4(&self.data, 1092 - 44)` reads ICD
byte 1092) or the identity header field. Accessors that return the whole
data (`raw_data`, `total_size`) or combine other accessors (the site
latitude and longitude in degrees) are left out.

    python tools/nexrad_crate_golden.py --nexrad-src PATH [--check] [id ...]

PATH is a clone of the nexrad repository at NEXRAD_REV. The script
generates tools/nexrad_crate_golden/src/accessors.rs from it and runs that
tool with its git dependencies patched to the clone. --check writes nothing
and exits with status 1 unless every golden file (and accessors.rs) is
reproduced byte for byte.
"""

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import tomllib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TESTDATA = os.path.join(ROOT, "testdata")
GOLDEN_DIR = os.path.join(TESTDATA, "level2", "golden", "nexrad_crate")
STATUS_DIR = os.path.join(TESTDATA, "level2", "golden", "status")
TOOL = os.path.join(ROOT, "tools", "nexrad_crate_golden")
ACCESSORS = os.path.join(TOOL, "src", "accessors.rs")
NEXRAD_REV = "1591b64f7b34b14a88b1f020384c252052c8255f"
NEXRAD_GIT = "https://github.com/danielway/nexrad"

PERFORMANCE = "nexrad-decode/src/messages/performance_maintenance_data"
ADAPTATION = "nexrad-decode/src/messages/rda_adaptation_data"
SIZES = {"Code2": 2, "Integer2": 2, "SInteger2": 2, "Integer4": 4, "Real4": 4, "u8": 1}
ACCESSOR = re.compile(r"pub fn ([a-z0-9_]+)\(&self\) -> ([^{]+)\{(.*?)\n    \}", re.S)


def read(src, relative):
    with open(os.path.join(src, relative), encoding="utf-8") as f:
        return f.read()


def field_size(rust_type):
    rust_type = rust_type.strip()
    array = re.fullmatch(r"\[(\w+); (\d+)\]", rust_type)
    if array:
        return SIZES[array.group(1)] * int(array.group(2))
    return SIZES[rust_type]


def struct_offsets(text, name):
    """Byte offset and size of each field of a `#[repr(C)]` struct of
    byte-array aliases (no padding)."""
    body = text[text.index(f"pub struct {name} {{"):]
    body = body[: body.index("\n}")]
    offsets = {}
    offset = 0
    for field, rust_type in re.findall(r"pub (\w+): ([^,]+),", body):
        size = field_size(rust_type)
        offsets[field] = (offset, size)
        offset += size
    return offsets, offset


def element_size(return_type):
    return {"u16": 2, "i16": 2, "u32": 4, "f32": 4}.get(return_type)


def performance_accessors(src):
    raw = read(src, f"{PERFORMANCE}/raw/message.rs")
    offsets, total = struct_offsets(raw, "Message")
    if total != 960:
        sys.exit(f"message 3 raw struct is {total} bytes, not 960")
    out = []
    for name, return_type, body in ACCESSOR.findall(read(src, f"{PERFORMANCE}/message.rs")):
        fields = re.findall(r"self\.inner\.(\w+)", body)
        if len(fields) != 1:
            sys.exit(f"message 3 accessor {name} reads {fields}")
        offset, size = offsets[fields[0]]
        return_type = return_type.strip()
        element = element_size(return_type) or (2 if return_type.startswith("[u16") else 1)
        out.append({"name": name, "type": return_type, "byte": offset, "size": size,
                    "element": element})
    return out


def offset_expression(text):
    if not re.fullmatch(r"[0-9 +\-*]+", text):
        sys.exit(f"unexpected offset expression {text!r}")
    return eval(text)  # digits and + - * only


def adaptation_accessors(src):
    raw = read(src, f"{ADAPTATION}/raw/message.rs")
    header, total = struct_offsets(raw, "Header")
    if total != 44:
        sys.exit(f"message 18 header is {total} bytes, not 44")
    out = []
    for name, return_type, body in ACCESSOR.findall(read(src, f"{ADAPTATION}/message.rs")):
        return_type = return_type.strip()
        if name in ("raw_data", "total_size"):
            continue
        field = re.search(r"self\.header\.(\w+)", body)
        if field:
            offset, size = header[field.group(1)]
            out.append({"name": name, "type": return_type, "byte": offset, "size": size,
                        "element": size})
            continue
        read_call = re.search(r"read_(real4|integer4|sinteger4|string)\(&self\.data, ([^,)]+)(?:, (\d+))?\)", body)
        loop = re.search(r"\(0\.\.(\d+)\)", body)
        manual = re.search(r"let offset = ([0-9 +\-*]+);", body)
        if read_call and loop:
            base = offset_expression(read_call.group(2).split("+")[0].strip())
            count = int(loop.group(1))
            out.append({"name": name, "type": return_type, "byte": 44 + base, "size": 4 * count,
                        "element": 4})
        elif read_call:
            kind = read_call.group(1)
            offset = offset_expression(read_call.group(2))
            size = int(read_call.group(3)) if kind == "string" else 4
            out.append({"name": name, "type": return_type, "byte": 44 + offset, "size": size,
                        "element": size})
        elif re.search(r"self\.[a-z0-9_]+\(\)\?", body):
            # Derived from other accessors (site_latitude from SLATDEG,
            # SLATMIN, ...), which are compared themselves.
            continue
        elif manual and "f64::from_be_bytes" in body:
            offset = offset_expression(manual.group(1))
            out.append({"name": name, "type": return_type, "byte": 44 + offset, "size": 8,
                        "element": 8})
        else:
            sys.exit(f"message 18 accessor {name}: unrecognised body")
    return out


def accessors_rs(performance, adaptation):
    lines = [
        "//! Generated by tools/nexrad_crate_golden.py from the nexrad source at",
        f"//! {NEXRAD_REV}; do not edit.",
        "",
        "use nexrad_decode::messages::{performance_maintenance_data, rda_adaptation_data};",
        "use serde_json::{Map, Value};",
        "",
        "use crate::Golden;",
        "",
    ]
    for function, module, items in (
        ("performance", "performance_maintenance_data", performance),
        ("adaptation", "rda_adaptation_data", adaptation),
    ):
        lines.append(f"pub fn {function}(m: &{module}::Message<'_>) -> Map<String, Value> {{")
        lines.append("    let mut out = Map::new();")
        for item in items:
            lines.append(f'    out.insert("{item["name"]}".into(), m.{item["name"]}().golden());')
        lines.append("    out")
        lines.append("}")
        lines.append("")
    return "\n".join(lines)


def manifest_entries():
    entries = {}
    paths = [os.path.join(TESTDATA, "manifest.toml")]
    for name in sorted(os.listdir(TESTDATA)):
        candidate = os.path.join(TESTDATA, name, "manifest.toml")
        if os.path.isfile(candidate):
            paths.append(candidate)
    for path in paths:
        if os.path.isfile(path):
            with open(path, "rb") as f:
                for entry in tomllib.load(f).get("file", []):
                    entries[entry["id"]] = entry
    return entries


def cache_dir():
    override = os.environ.get("RECAST_RADAR_TESTDATA")
    if override:
        return override
    if os.name == "nt" and os.environ.get("LOCALAPPDATA"):
        base = os.environ["LOCALAPPDATA"]
    elif os.environ.get("XDG_CACHE_HOME"):
        base = os.environ["XDG_CACHE_HOME"]
    elif os.environ.get("HOME"):
        base = os.path.join(os.environ["HOME"], ".cache")
    else:
        return os.path.join(ROOT, ".testdata-cache")
    return os.path.join(base, "recast-radar-tools", "testdata")


def source_path(entries, file_id):
    entry = entries[file_id]
    committed = entry.get("committed")
    if committed:
        path = os.path.join(TESTDATA, committed.removeprefix("testdata/"))
    else:
        path = os.path.join(cache_dir(), file_id)
    with open(path, "rb") as f:
        digest = hashlib.sha256(f.read()).hexdigest()
    if digest != entry["sha256"]:
        sys.exit(f"{file_id}: sha256 mismatch for {path}")
    return path, digest


def run_tool(src, sources):
    patch = []
    for crate in ("nexrad-decode", "nexrad-data"):
        path = os.path.join(src, crate).replace("\\", "/")
        patch += ["--config", f'patch."{NEXRAD_GIT}".{crate}.path="{path}"']
    command = ["cargo", "run", "--release", "--quiet", "--manifest-path",
               os.path.join(TOOL, "Cargo.toml"), *patch, "--"]
    command += [f"{file_id}={path}" for file_id, (path, _) in sources.items()]
    env = dict(os.environ)
    env.pop("CARGO_TARGET_DIR", None)
    result = subprocess.run(command, check=True, capture_output=True, text=True, env=env)
    return json.loads(result.stdout)


def golden_text(file_id, digest, decoded, performance, adaptation):
    document = {
        "id": file_id,
        "sha256": digest,
        "generator": "tools/nexrad_crate_golden.py",
        "nexrad_rev": NEXRAD_REV,
    }
    if "error" in decoded:
        document["error"] = decoded["error"]
    for key, items in (("message_3", performance), ("message_18", adaptation)):
        values = decoded.get(key)
        if values is None:
            document[key] = None
            continue
        document[key] = [dict(item, value=values[item["name"]]) for item in items]
    lines = ["{"]
    keys = list(document)
    for index, key in enumerate(keys):
        comma = "," if index + 1 < len(keys) else ""
        value = document[key]
        if isinstance(value, list):
            lines.append(f" {json.dumps(key)}: [")
            for number, item in enumerate(value):
                inner = "," if number + 1 < len(value) else ""
                lines.append(f"  {json.dumps(item, separators=(',', ':'))}{inner}")
            lines.append(f" ]{comma}")
        else:
            lines.append(f" {json.dumps(key)}: {json.dumps(value)}{comma}")
    lines.append("}")
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--nexrad-src", required=True)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("ids", nargs="*")
    args = parser.parse_args()

    rev = subprocess.run(["git", "-C", args.nexrad_src, "rev-parse", "HEAD"], check=True,
                         capture_output=True, text=True).stdout.strip()
    if rev != NEXRAD_REV:
        sys.exit(f"{args.nexrad_src} is at {rev}, not {NEXRAD_REV}")
    dirty = subprocess.run(["git", "-C", args.nexrad_src, "status", "--porcelain", "--",
                            "nexrad-decode", "nexrad-data"], check=True, capture_output=True,
                           text=True).stdout.strip()
    if dirty:
        sys.exit(f"{args.nexrad_src} has local changes:\n{dirty}")

    performance = performance_accessors(args.nexrad_src)
    adaptation = adaptation_accessors(args.nexrad_src)
    generated = accessors_rs(performance, adaptation)
    ids = args.ids or sorted(name[:-5] for name in os.listdir(STATUS_DIR)
                             if name.endswith(".json"))
    entries = manifest_entries()
    sources = {file_id: source_path(entries, file_id) for file_id in ids}

    failed = False
    if args.check:
        with open(ACCESSORS, encoding="utf-8", newline="") as f:
            if f.read() != generated:
                print(f"{ACCESSORS} is not reproduced")
                failed = True
    else:
        with open(ACCESSORS, "w", encoding="utf-8", newline="\n") as f:
            f.write(generated)
    decoded = run_tool(args.nexrad_src, sources)
    os.makedirs(GOLDEN_DIR, exist_ok=True)
    for file_id, (_, digest) in sources.items():
        text = golden_text(file_id, digest, decoded[file_id], performance, adaptation)
        path = os.path.join(GOLDEN_DIR, f"{file_id}.json")
        if args.check:
            with open(path, encoding="utf-8", newline="") as f:
                if f.read() != text:
                    print(f"{path} is not reproduced")
                    failed = True
        else:
            with open(path, "w", encoding="utf-8", newline="\n") as f:
                f.write(text)
    if failed:
        sys.exit(1)
    print(("checked " if args.check else "wrote ") + f"{len(sources)} files")


if __name__ == "__main__":
    main()
