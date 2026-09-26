#!/usr/bin/env python3
"""h5py goldens for the recast-radar-hdf5 real-file tests.

For every HDF5 file in the corpus (ODIM_H5, netCDF-4 CfRadial 1 and 2), h5py
(the HDF Group's C library, independent of the Rust crate under test) records:

- the superblock version and offset size (``get_create_plist().get_version()``
  and ``get_sizes()``),
- every path reached from the root group through hard links, with the object
  kind, object header address and header version (``h5o.get_info``),
- every attribute: datatype class, size and byte order, shape, and value,
- every group's link names and link kinds,
- every dataset: shape, maximum shape, datatype, layout, chunk shape, the
  chunk index (``H5Dget_chunk_index_type``, which h5py does not wrap, called
  in h5py's own HDF5 library through ctypes; left out when that library
  cannot be found), filter ids, fill value, the stored chunks
  (``get_chunk_info``: element offsets, filter mask, file offset, stored
  size), and a SHA-256 of the values in a canonical little-endian encoding
  (below) with a few leading values.

Canonical value encoding (hashed; the Rust test encodes its values the same
way): integers and floats as little-endian bytes of the stored width; strings
(fixed or variable length) as each element's text bytes followed by one NUL;
object references as the target's object header address (u64 LE, 0 = null).

Run with the reference venv:

    python tools/hdf5_golden.py [--id ID ...] [--no-download]

Writes testdata/golden/hdf5/<id>.json.
"""

import argparse
import ctypes
import hashlib
import json
import math
import os
import sys
import tomllib
import urllib.request
from pathlib import Path

import h5py
import numpy as np

ROOT = Path(__file__).resolve().parent.parent
TESTDATA = ROOT / "testdata"
OUT = TESTDATA / "golden" / "hdf5"

# Corpus ids covered, with why.
IDS = [
    # netCDF-4 (superblock v2, dense links and attributes, v2 B-trees,
    # fractal heaps with indirect blocks, creation-order indexes).
    "cfrad1-xsapr-sgp-20110520-ppi-netcdf4",
    "cfrad1-spol-20080604-002217-sur",
    "cfrad2-spol-20080604-002217-sur",
    "cfrad1-dow8-20211011-223602-rhi",
    # CfRadial 2 written by Radx (netCDF 4.9.2 / HDF5 1.10.10) and by
    # xradar (netCDF 4.9.3 / HDF5 1.14.6).
    "cfrad2-radx-irene-sr2-20110827-120420-sur-r30km",
    "cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32",
    "cfrad2-xradar-xsapr-sgp-20110520-ppi",
    "cfrad2-xradar-dow8-20211011-223602-rhi-r300",
    # ODIM_H5 (superblock v0/v1, old-style groups, v1 B-tree chunks, vlen
    # strings, v2 object headers in AEMET's dialect).
    "odim-bejab-20190606-0000-pvol",
    "odim-bewid-20130429-0430-pvol-dbzh-scan1",
    "odim-norst-20170421-0908-pvol",
    "odim-espdg-20260707-1927-pvol-dbzh-vradh",
    "odim-imgw-ram-20260711-0015-kdp-max",
    "odim-iesha-20260305-0115-pvol",
    "odim-dkrom-20260820-1130-pvol",
    # ODIM_H5 structures the ODIM decoder keeps: int16 planes and nested how
    # groups (SMHI), quality groups with code/class legends (FMI), root how
    # subgroups (DWD), a v2.4 key/value legend (ARPA Lombardia).
    "odim-seang-20260924-2130-qcvol-dataset1-trim",
    "odim-fianj-20260924-2130-pvol-dataset1-trim",
    "odim-deboo-20260924-2130-sweep-th-00",
    "odim-itdes-20260924-2135-pvol-class",
    # HDF5 1.10+ container of real ODIM data (superblock v3, every v4 chunk
    # index, dense attributes with huge fractal heap objects, Fletcher-32).
    "odim-dkrom-20260820-1130-pvol-h5latest-trim",
    # Paged extensible-array data blocks (sparsely written too), committed
    # datatypes, 4-byte addresses; 4-byte lengths (global heap padding) and
    # a deflated dense-link fractal heap.
    "odim-dkrom-20260820-1130-pvol-h5edge-paged-ea",
    "odim-dkrom-20260820-1130-pvol-h5edge-len4",
]

INLINE_MAX = 16  # arrays longer than this are hashed, not inlined


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
ALLOW_DOWNLOAD = True


def cache_dir():
    if os.environ.get("RECAST_RADAR_TESTDATA"):
        return Path(os.environ["RECAST_RADAR_TESTDATA"])
    for var, suffix in (("LOCALAPPDATA", ()), ("XDG_CACHE_HOME", ()), ("HOME", (".cache",))):
        if var == "LOCALAPPDATA" and os.name != "nt":
            continue
        if os.environ.get(var):
            return Path(os.environ[var], *suffix, "recast-radar-tools", "testdata")
    return ROOT / ".testdata-cache"


def corpus_path(entry_id):
    entry = MANIFEST[entry_id]
    if "committed" in entry:
        path = TESTDATA / entry["committed"].removeprefix("testdata/")
    else:
        path = cache_dir() / entry_id
        if not path.is_file():
            if not ALLOW_DOWNLOAD:
                raise FileNotFoundError(f"{entry_id} is not cached at {path}")
            path.parent.mkdir(parents=True, exist_ok=True)
            with urllib.request.urlopen(entry["urls"][0], timeout=600) as r:
                path.write_bytes(r.read())
    data = path.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    if digest != entry["sha256"] or len(data) != entry["size"]:
        raise ValueError(f"{entry_id}: sha256/size mismatch ({digest}, {len(data)})")
    return path


# ------------------------------------------------------------- datatypes ---

CLASS_NAMES = {
    h5py.h5t.INTEGER: "integer",
    h5py.h5t.FLOAT: "float",
    h5py.h5t.TIME: "time",
    h5py.h5t.STRING: "string",
    h5py.h5t.BITFIELD: "bitfield",
    h5py.h5t.OPAQUE: "opaque",
    h5py.h5t.COMPOUND: "compound",
    h5py.h5t.REFERENCE: "reference",
    h5py.h5t.ENUM: "enum",
    h5py.h5t.VLEN: "vlen",
    h5py.h5t.ARRAY: "array",
}


def describe_type(tid):
    cls = tid.get_class()
    out = {"class": CLASS_NAMES.get(cls, str(cls)), "size": tid.get_size()}
    if cls == h5py.h5t.STRING:
        if tid.is_variable_str():
            out["class"] = "vlen_string"
    if cls in (h5py.h5t.INTEGER, h5py.h5t.FLOAT, h5py.h5t.BITFIELD):
        out["order"] = "big" if tid.get_order() == h5py.h5t.ORDER_BE else "little"
    if cls == h5py.h5t.INTEGER:
        out["signed"] = tid.get_sign() == h5py.h5t.SGN_2
    if cls == h5py.h5t.ENUM:
        base = tid.get_super()
        out["base"] = describe_type(base)
        out["members"] = {tid.get_member_name(i).decode(): int(tid.get_member_value(i))
                          for i in range(tid.get_nmembers())}
    if cls == h5py.h5t.COMPOUND:
        out["members"] = [{"name": tid.get_member_name(i).decode(),
                           "offset": tid.get_member_offset(i),
                           "type": describe_type(tid.get_member_type(i))}
                          for i in range(tid.get_nmembers())]
    if cls == h5py.h5t.VLEN:
        out["base"] = describe_type(tid.get_super())
    if cls == h5py.h5t.ARRAY:
        out["dims"] = list(tid.get_array_dims())
        out["base"] = describe_type(tid.get_super())
    return out


# ---------------------------------------------------------------- values ---

def text_of(value):
    if isinstance(value, bytes):
        return value.split(b"\0")[0].decode("utf-8", "replace")
    return str(value)


def canonical(values, dtype_desc, file):
    """Canonical little-endian encoding of a flat value array."""
    cls = dtype_desc["class"]
    if cls in ("integer", "float", "bitfield", "enum"):
        arr = np.asarray(values)
        return arr.astype(arr.dtype.newbyteorder("<"), copy=False).tobytes()
    if cls in ("string", "vlen_string"):
        return b"".join(text_of(v).encode("utf-8") + b"\0" for v in values)
    if cls == "reference":
        out = b""
        for ref in values:
            if not ref:
                out += (0).to_bytes(8, "little")
            else:
                out += h5py.h5o.get_info(file[ref].id).addr.to_bytes(8, "little")
        return out
    return None


def json_number(value):
    value = float(value) if isinstance(value, (float, np.floating)) else int(value)
    if isinstance(value, float):
        if math.isnan(value):
            return "NaN"
        if math.isinf(value):
            return "Infinity" if value > 0 else "-Infinity"
    return value


def encode_values(flat, dtype_desc, file):
    """JSON form: inline values for short arrays, SHA-256 + head otherwise."""
    cls = dtype_desc["class"]
    n = len(flat)
    out = {"len": n}
    if cls in ("integer", "float", "bitfield", "enum"):
        items = [json_number(v) for v in flat]
    elif cls in ("string", "vlen_string"):
        items = [text_of(v) for v in flat]
    elif cls == "reference":
        items = [0 if not r else h5py.h5o.get_info(file[r].id).addr for r in flat]
    elif cls == "compound":
        out["compound"] = {}
        names = flat.dtype.names if hasattr(flat, "dtype") and flat.dtype.names else []
        for member, desc in zip(names, dtype_desc["members"]):
            out["compound"][member] = encode_values(flat[member].reshape(-1), desc["type"], file)
        return out
    elif cls == "vlen":
        out["sequences"] = [encode_values(np.asarray(v).reshape(-1), dtype_desc["base"], file)
                            for v in flat[:INLINE_MAX]]
        return out
    elif cls == "array":
        base = dtype_desc["base"]
        return encode_values(np.asarray(flat).reshape(-1), base, file)
    else:
        return out
    blob = canonical(flat, dtype_desc, file)
    if blob is not None:
        out["sha256"] = hashlib.sha256(blob).hexdigest()
    if n <= INLINE_MAX:
        out["values"] = items
    else:
        out["head"] = items[:8]
    return out


def attribute_json(obj, name, file):
    aid = obj.attrs.get_id(name)
    desc = describe_type(aid.get_type())
    space = aid.get_space()
    shape = list(space.shape) if space.get_simple_extent_type() == h5py.h5s.SIMPLE else []
    null = space.get_simple_extent_type() == h5py.h5s.NULL
    out = {"name": name, "type": desc, "shape": shape, "null": null}
    if null:
        return out
    value = obj.attrs[name]
    flat = np.asarray(value, dtype=object if desc["class"] in ("vlen", "reference") else None)
    if desc["class"] == "compound":
        flat = np.asarray(value).reshape(-1)
    else:
        flat = flat.reshape(-1)
    out["value"] = encode_values(flat, desc, file)
    return out


def _chunk_index_function():
    """``H5Dget_chunk_index_type`` from the HDF5 library h5py loaded, or None."""
    package = Path(h5py.__file__).resolve().parent
    candidates = sorted(package.glob("hdf5.dll")) + sorted(package.glob("libhdf5*.so*"))
    candidates += sorted((package.parent / "h5py.libs").glob("libhdf5-*.so*"))
    candidates += sorted((package / ".dylibs").glob("libhdf5*.dylib"))
    for path in candidates:
        if "_hl" in path.name:
            continue
        try:
            function = ctypes.CDLL(str(path)).H5Dget_chunk_index_type
        except (OSError, AttributeError):
            continue
        function.argtypes = [ctypes.c_int64, ctypes.POINTER(ctypes.c_int)]
        function.restype = ctypes.c_int
        return function
    return None


CHUNK_INDEX_TYPE = _chunk_index_function()
# H5D_chunk_index_t (H5Dpublic.h).
CHUNK_INDEX_NAMES = {0: "btree_v1", 1: "single_chunk", 2: "implicit", 3: "fixed_array",
                     4: "extensible_array", 5: "btree_v2"}


def chunk_index_name(dsid):
    """The chunk index of a chunked dataset, as HDF5 reports it, or None."""
    if CHUNK_INDEX_TYPE is None:
        return None
    kind = ctypes.c_int(-1)
    if CHUNK_INDEX_TYPE(dsid.id, ctypes.byref(kind)) < 0:
        return None
    return CHUNK_INDEX_NAMES.get(kind.value)


def layout_name(layout):
    return {h5py.h5d.COMPACT: "compact", h5py.h5d.CONTIGUOUS: "contiguous",
            h5py.h5d.CHUNKED: "chunked", h5py.h5d.VIRTUAL: "virtual"}.get(layout, str(layout))


def dataset_json(ds, file):
    dsid = ds.id
    dcpl = dsid.get_create_plist()
    desc = describe_type(dsid.get_type())
    space = dsid.get_space()
    null = space.get_simple_extent_type() == h5py.h5s.NULL
    out = {
        "shape": list(ds.shape) if ds.shape is not None else [],
        "maxshape": [(-1 if m is None else m) for m in ds.maxshape] if ds.maxshape else [],
        "type": desc,
        "layout": layout_name(dcpl.get_layout()),
        "null": null,
    }
    if dcpl.get_layout() == h5py.h5d.CHUNKED:
        out["chunks"] = list(dcpl.get_chunk())
        index = chunk_index_name(dsid)
        if index is not None:
            out["chunk_index"] = index
        out["filters"] = [dcpl.get_filter(i)[0] for i in range(dcpl.get_nfilters())]
        # HDF5 2.0's H5Dget_chunk_info and H5Dget_chunk_info_by_coord report
        # the wrong element offsets for an extensible-array index whose
        # unlimited dimension is not the first one (the read path "swizzles"
        # the unlimited dimension to the front, the query path does not
        # match it): /dataset1/data5 of the h5latest fixture reports its
        # (8, 0) chunk at (0, 16), while ds[...] reads the bytes at the other
        # address. Only the stored chunks (mask, file offset, size) are
        # recorded there; the value hash checks the placement.
        unlimited = [i for i, m in enumerate(ds.maxshape or ()) if m is None]
        offsets_reliable = not (len(unlimited) == 1 and unlimited[0] != 0)
        out["chunk_offsets_reliable"] = offsets_reliable
        chunks = []
        stored = dsid.get_num_chunks()
        if stored <= 10_000:
            infos = [dsid.get_chunk_info(i) for i in range(stored)]
        else:
            # H5Dget_chunk_info(i) walks the index up to chunk i on every
            # call; H5Dchunk_iter visits every chunk once.
            infos = []
            dsid.chunk_iter(infos.append)
            assert len(infos) == stored
        for info in infos:
            offsets = list(info.chunk_offset) if offsets_reliable else []
            chunks.append([offsets, info.filter_mask, info.byte_offset, info.size])
        chunks.sort()
        out["num_chunks"] = len(chunks)
        blob = json.dumps(chunks).encode()
        out["chunk_info_sha256"] = hashlib.sha256(blob).hexdigest()
        out["chunk_info"] = chunks[:4]
    if null:
        return out
    if desc["class"] in ("integer", "float", "string", "vlen_string", "reference", "enum", "bitfield"):
        values = ds[()]
        flat = np.asarray(values, dtype=object if desc["class"] == "reference" else None).reshape(-1)
        out["value"] = encode_values(flat, desc, file)
        if desc["class"] in ("integer", "float") and flat.size:
            finite = flat[np.isfinite(flat)] if desc["class"] == "float" else flat
            if finite.size:
                out["min"] = json_number(finite.min())
                out["max"] = json_number(finite.max())
    elif desc["class"] == "compound":
        out["value"] = encode_values(np.asarray(ds[()]).reshape(-1), desc, file)
    return out


def object_kind(obj):
    if isinstance(obj, h5py.Group):
        return "group"
    if isinstance(obj, h5py.Dataset):
        return "dataset"
    if isinstance(obj, h5py.Datatype):
        return "datatype"
    return "other"


def dump(entry_id):
    path = corpus_path(entry_id)
    f = h5py.File(path, "r")
    fcpl = f.id.get_create_plist()
    result = {
        "id": entry_id,
        "sha256": MANIFEST[entry_id]["sha256"],
        "generator": f"tools/hdf5_golden.py (h5py {h5py.version.version}, HDF5 {h5py.version.hdf5_version})",
        "superblock_version": fcpl.get_version()[0],
        "offset_size": fcpl.get_sizes()[0],
        "objects": {},
    }
    visited = set()

    def visit(path, obj):
        info = h5py.h5o.get_info(obj.id)
        entry = {
            "kind": object_kind(obj),
            "address": info.addr,
            "header_version": info.hdr.version,
            "attributes": [attribute_json(obj, name, f) for name in obj.attrs.keys()],
        }
        if isinstance(obj, h5py.Group):
            links = []
            for name in obj.keys():
                link = obj.get(name, getlink=True)
                kind = ("hard" if isinstance(link, h5py.HardLink)
                        else "soft" if isinstance(link, h5py.SoftLink)
                        else "external" if isinstance(link, h5py.ExternalLink) else "other")
                links.append({"name": name, "kind": kind})
            entry["links"] = links
        elif isinstance(obj, h5py.Dataset):
            entry["dataset"] = dataset_json(obj, f)
        result["objects"][path] = entry
        if isinstance(obj, h5py.Group) and info.addr not in visited:
            visited.add(info.addr)
            for name in obj.keys():
                link = obj.get(name, getlink=True)
                if not isinstance(link, h5py.HardLink):
                    continue
                child_path = (path.rstrip("/") + "/" + name)
                if child_path in result["objects"]:
                    continue
                visit(child_path, obj[name])

    visit("/", f)
    return result


def compact_json(result):
    """Compact JSON with one line per object path (small, diffable)."""
    head = {k: v for k, v in result.items() if k != "objects"}
    lines = [json.dumps(head, separators=(",", ":"))[:-1] + ',"objects":{']
    items = list(result["objects"].items())
    for index, (path, entry) in enumerate(items):
        comma = "," if index + 1 < len(items) else ""
        lines.append(json.dumps(path) + ":" + json.dumps(entry, separators=(",", ":")) + comma)
    lines.append("}}")
    return "\n".join(lines) + "\n"


def main():
    global ALLOW_DOWNLOAD
    parser = argparse.ArgumentParser()
    parser.add_argument("--id", action="append", help="corpus id (default: all)")
    parser.add_argument("--no-download", action="store_true")
    args = parser.parse_args()
    ALLOW_DOWNLOAD = not args.no_download
    OUT.mkdir(parents=True, exist_ok=True)
    for entry_id in args.id or IDS:
        if entry_id not in MANIFEST:
            print(f"skip {entry_id}: not in the manifest", file=sys.stderr)
            continue
        result = dump(entry_id)
        target = OUT / f"{entry_id}.json"
        with open(target, "w", encoding="utf-8", newline="\n") as fh:
            fh.write(compact_json(result))
        print(f"{entry_id}: {len(result['objects'])} objects -> {target.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
