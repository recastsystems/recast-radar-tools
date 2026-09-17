#!/usr/bin/env python3
"""Generate FM301 conformance goldens (plan F.4) from xradar and Py-ART.

Usage (from the workspace root)::

    python tools/fm301_golden.py [--check] [--offline] [--list] [ID ...]

For every case in ``CASES`` (default: all) the script resolves the real file
through the testdata manifests (``testdata/manifest.toml`` and
``testdata/*/manifest.toml``): the committed copy under ``testdata/`` if the
entry has ``committed``, otherwise the download cache used by the
``recast-radar-testdata`` crate (``$RECAST_RADAR_TESTDATA``, else
``%LOCALAPPDATA%\\recast-radar-tools\\testdata`` on Windows, else
``$XDG_CACHE_HOME/recast-radar-tools/testdata``, else
``$HOME/.cache/recast-radar-tools/testdata``), downloading from the manifest
URLs when missing (not with ``--offline``).  The SHA-256 is always verified.
It then writes

* ``testdata/conformance/fm301/xradar/<id>.json``: what xradar produces,
* ``testdata/conformance/fm301/pyart/<id>.json``: what Py-ART produces,
* ``testdata/conformance/fm301/index.json``: the case list and statuses.

``--check`` regenerates in memory and fails if any file on disk differs.
``--list`` prints the cases.  ``--verify`` also cross-checks the two readers
(and, for NEXRAD, MetPy 1.7.1 as a third, independent reader) and fails
without writing if any check fails:

* NEXRAD ``metpy->pyart``: MetPy's native moments, evaluated as
  ``(raw - offset) / scale`` in float32 with raw 0 and 1 as NaN and placed on
  Py-ART's volume range with each native gate repeated ``gate_width /
  spacing`` times, hash equal to Py-ART's per-sweep field ``sha256``.  This is
  the evaluation and layout the FM301 view uses (design note 6.5, 7.1).
* NEXRAD ``xradar->pyart``: xradar's raw arrays (view ``time``) evaluated the
  same way and padded with NaN hash equal to Py-ART's, wherever xradar's
  range and the moment's native geometry both equal Py-ART's (xradar
  misplaces the rest; design note 6.2).
* ODIM and CfRadial ``xradar->pyart stats``: xradar's raw arrays masked at
  ``_FillValue``, ``_Undetect`` and NaN and scaled give Py-ART's per-sweep
  count of finite unmasked values exactly and its min, max and mean within
  1e-4 relative.

Requires Python 3.11+ (``tomllib``), numpy, xradar 0.12.0 and arm_pyart 2.2.5
(with their xarray, netCDF4, h5netcdf and h5py); golden values are specific
to those versions, which every golden records.

Readers
-------

``nexrad`` cases: ``xradar.io.open_nexradlevel2_datatree`` and
``pyart.io.read_nexrad_archive(path, linear_interp=False)``.  xradar 0.12
cannot open whole-file gzip input, so a gzip file is decompressed to a
temporary file for xradar only (``reader.input`` says so and gives the
decompressed SHA-256); Py-ART reads the manifest file itself.
``linear_interp=False`` makes Py-ART repeat each 1 km reflectivity gate over
the four 250 m gates it covers instead of interpolating (FM301 design note
``docs/design/fm301-model.md`` section 6.5); it makes no difference for
super-resolution volumes.

``odim`` cases: ``xradar.io.open_odim_datatree`` and
``pyart.aux_io.read_odim_h5``.  ``cfradial1`` cases:
``xradar.io.open_cfradial1_datatree`` and ``pyart.io.read_cfradial``.

xradar is opened three times, always with ``optional_groups=True``:

* view ``time``: ``first_dim="time"``, ``mask_and_scale=False``.  The encoded
  form (design note 12.3): packed integers with ``scale_factor``,
  ``add_offset`` and ``_FillValue`` left in ``attrs``; rays in acquisition
  order under dimension ``time``.
* view ``auto``: ``first_dim="auto"`` (xradar's default), ``mask_and_scale=False``.
  Rays sorted by angle under dimension ``azimuth`` or ``elevation``.
* decoded: ``first_dim="time"`` with xradar's defaults otherwise
  (``mask_and_scale=True``), for the attributes, encoding and value
  statistics a user of the default DataTree sees.

Common value conventions
------------------------

``sha256``
    SHA-256 of the array's elements in row-major (C) order, each encoded
    little-endian in the width of the reported ``dtype``: ``bool`` as one byte
    0/1, ``datetime64[ns]`` as int64 nanoseconds since 1970-01-01T00:00:00Z.
    Every NaN is first replaced by the canonical quiet NaN (f32 ``0x7FC00000``,
    f64 ``0x7FF8000000000000``), so NaN sign and payload never matter.
Non-finite floats
    JSON has no NaN or infinity; they are written as the strings ``"NaN"``,
    ``"Infinity"`` and ``"-Infinity"`` wherever a float may appear.
Typed attributes
    ``attrs`` holds attribute values as plain JSON, keys sorted; ``attr_types``
    (same keys) gives each value's Python/NumPy type: ``bool``, ``int``, ``float`` and
    ``str`` for Python objects; a NumPy dtype name (``uint8``, ``float32``,
    ...) for NumPy scalars; ``<dtype>[]`` for NumPy arrays (value is a list);
    ``bytes`` (value decoded as Latin-1), ``list``, ``dict``, ``none``.
    xradar 0.12 writes several NEXRAD attributes as Python ``bool``/``int``
    (design note A.2), which this preserves.
Value summary (``values``)
    Always ``dtype`` and ``shape``.  0-d: ``value``.  Text (unicode or bytes;
    bytes decoded as Latin-1) and object arrays: ``values``, the flattened
    elements, except that a bytes array of single characters with two or more
    dimensions (a netCDF character array) has ``strings`` instead: one string
    per row of the last dimension with NUL characters removed.
    Numeric: ``count`` (elements), ``sha256``; integers add ``min``, ``max``
    and ``sum`` (exact); floats add ``count_nan``, ``count_inf`` and ``min``,
    ``max``, ``mean`` over the finite elements (``null`` when there are none;
    ``mean`` computed in float64); ``datetime64`` arrays are summarized as
    float64 seconds since 1970-01-01T00:00:00Z (``min``, ``max``, ``mean``;
    ``count_nat``), with ``sha256`` over the int64 nanoseconds.  1-D arrays
    add ``first`` (up to 3 elements), ``last`` and, when numeric,
    ``non_decreasing``; 1-D arrays of at most 64 elements add ``values``
    (all elements).  Datetimes in ``first``/``last``/``values`` are float64
    seconds since the epoch.

xradar golden (``schema`` ``recast-radar-tools/fm301-golden/xradar/1``)
---------------------------------------------------------------------

``schema``, ``id``, ``format``, ``sha256``, ``size``, ``categories``, ``note``
    Case identity (from the manifest and ``CASES``).
``generator``
    ``script``, ``python`` (major.minor) and library versions.
``reader``
    ``function``, ``input`` (``"file"`` or ``"gunzip"``), ``input_sha256``,
    ``input_size``.
``status``
    ``"ok"`` or ``"error"`` (the reader raised; ``views`` and ``decoded`` are
    then null and ``error`` is ``{type, message}``).
``warnings``
    Distinct warnings raised while opening and reading, in order, as
    ``"<Category>: <message>"`` with the input path replaced by ``<input>``.
    NumPy's "numpy.ndarray size changed" binary-compatibility RuntimeWarning is
    environment noise and omitted.
``views``
    ``{"time": view, "auto": view}``.  A view is ``{kwargs, groups}``;
    ``groups`` lists every DataTree node in ``DataTree.subtree`` order as
    ``{path, dims, attrs, attr_types, variables}``, for the node's own dataset
    (``inherit=False``).  ``dims`` maps dimension name to size; ``variables``
    lists coordinates, then data variables, each ``{name, role
    ("coord"|"data"), dims, dtype, attrs, attr_types, encoding, values}``.
    Dimension names, variables (within a role) and attribute keys are sorted
    by name: xradar builds some datasets from sets, so its own order changes
    with Python's string hash seed (for example the ODIM root variables).
    ``dims`` of a variable keep xradar's axis order.  ``encoding`` holds the xarray encoding keys
    ``dtype``, ``units``, ``calendar``, ``_FillValue``, ``missing_value``,
    ``scale_factor``, ``add_offset`` and ``coordinates`` that are present
    (``encoding_types`` gives their types).  In the ``auto`` view ``attrs``,
    ``attr_types``, ``encoding`` and ``encoding_types`` are omitted from groups
    and variables when they are identical to the ``time`` view (the script
    compares them, keyed by group path and variable name; they are written in
    full wherever they differ).
``decoded``
    ``{kwargs, groups}``: every group of the decoded open as ``{path,
    variables}``, with variables in the same form as the views (``role``,
    ``dims``, ``dtype``, ``attrs``, ``encoding``, ...).  Here ``dtype`` is the
    decoded dtype (``float64`` for NEXRAD moments, ``float32`` where the file's
    ``scale_factor`` is float32), ``attrs`` no longer hold ``scale_factor``,
    ``add_offset`` or ``_FillValue`` (xarray moves them into ``encoding``), and
    ``values`` is the value summary of the decoded array without ``sha256``
    (the encoded views pin the values; the statistics show what CF decoding
    yields, for example -33.0 at NEXRAD below-threshold gates because xradar
    writes no ``_FillValue``).

Py-ART golden (``schema`` ``recast-radar-tools/fm301-golden/pyart/1``)
--------------------------------------------------------------------

``schema``, ``id``, ``format``, ``sha256``, ``size``, ``categories``, ``note``,
``generator``, ``status``, ``error``, ``warnings``
    As for xradar (``reader``: ``{function, input, kwargs}``).
``radar``
    ``scan_type``, ``nrays``, ``ngates``, ``nsweeps``; ``metadata`` and
    ``metadata_types`` (typed-attribute form); ``sweeps``: ``[{sweep, start,
    end, rays}]`` from ``sweep_start_ray_index``/``sweep_end_ray_index``;
    ``variables``: every Radar attribute that is a Py-ART data dictionary
    (``time``, ``range``, ``azimuth``, ``elevation``, ``fixed_angle``,
    ``sweep_mode``, ``latitude``, ...), each ``{meta, meta_types, values}``
    where ``meta`` is the dictionary without ``data`` and ``values``
    summarizes the data array ignoring any mask (``masked_count`` is added
    when a MaskedArray has masked elements).  A variable whose data is 1-D
    numeric with ``nrays`` elements also has ``per_sweep``: one value summary
    per sweep.  ``instrument_parameters`` and ``radar_calibration``: the same
    form keyed by name (``null`` when Py-ART has none).
``fields``
    Keyed by Py-ART field name, sorted (Py-ART's dictionary order is not
    stable across runs): ``{meta, meta_types, dtype, shape, masked, total,
    per_sweep}``.  ``masked`` says whether ``data`` is a MaskedArray.
    ``total`` and each ``per_sweep`` entry are field statistics over all rays
    or over the sweep's rays (``sweep``, ``rays`` and ``gates`` added per
    sweep): ``count_unmasked``, ``count_masked``, ``count_nan_unmasked``
    (unmasked NaN, as Py-ART's ODIM reader pads), ``min``, ``max``, ``mean``
    over finite unmasked values (float64 mean), ``extent`` (largest
    ``last unmasked gate index + 1`` over the rays, 0 when all masked) and
    ``sha256``: the value hash of the data with masked elements replaced by NaN
    (float dtypes) or by the field's ``_FillValue`` (other dtypes; ``null``
    if there is none), in Py-ART's ray order.
"""

import argparse
import gc
import gzip
import hashlib
import json
import math
import os
import shutil
import sys
import tempfile
import tomllib
import traceback
import urllib.request
import warnings
from dataclasses import dataclass
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
TESTDATA = ROOT / 'testdata'
OUT_DIR = TESTDATA / 'conformance' / 'fm301'
SCRIPT = 'tools/fm301_golden.py'

SCHEMA_XRADAR = 'recast-radar-tools/fm301-golden/xradar/1'
SCHEMA_PYART = 'recast-radar-tools/fm301-golden/pyart/1'
SCHEMA_INDEX = 'recast-radar-tools/fm301-golden/index/1'

EXPECTED_VERSIONS = {'xradar': '0.12.0', 'arm_pyart': '2.2.5'}

FULL_VALUES_LIMIT = 64
ENCODING_KEYS = ('dtype', 'units', 'calendar', '_FillValue', 'missing_value', 'scale_factor',
                 'add_offset', 'coordinates')
NOISE_WARNINGS = ('numpy.ndarray size changed',)
GZIP_MAGIC = bytes([0x1F, 0x8B])


@dataclass(frozen=True)
class Case:
    id: str
    kind: str  # nexrad | odim | cfradial1
    categories: tuple
    note: str


CASES = (
    Case('l2-ktlx-20240315-000217', 'nexrad', ('nexrad-level2', 'modern-dualpol', 'sails'),
         'Build 22.0 Message 31, VCP 212 with SAILS x3, 20 sweeps. Surveillance cuts carry '
         'dual-pol moments on 1192 of 1832 gates; xradar pads them with raw 0 and writes no '
         '_FillValue (design note 6.1, A.2).'),
    Case('l2-kdvn-20200810-180401', 'nexrad', ('nexrad-level2', 'modern-dualpol', 'meso-sails'),
         'Build 18.2 Message 31, VCP 212 with MESO-SAILS x2, 21 sweeps, 8-bit ZDR, '
         'VOL 44 v2.0 / RAD 28 blocks, LDM bzip2 records.'),
    Case('l2-kpah-20080415-235014', 'nexrad', ('nexrad-level2', 'msg31-legacy-resolution'),
         'AR2V0004 Build 10.0 Message 31 at legacy resolution, VCP 32, 7 sweeps: REF 1 km gates '
         'from 500 m, VEL/SW 250 m from 125 m. xradar puts the mixed sweeps 4-6 on REF spacing '
         '(VRADH/WRADH misplaced x4); Py-ART repeats REF on 250 m gates (design note 6.2, A.3).'),
    Case('l2-klix-20050829-130035', 'nexrad', ('nexrad-level2', 'message1'),
         'AR2V0001 Message 1 with metadata messages, VCP 121, 20 sweeps: REF 1 km from 0 m, '
         'VEL/SW 250 m from -375 m. xradar needs the gunzipped file, emits no WRADH, puts mixed '
         'sweeps on REF spacing and reads the -375 m first gate as 65161 (design note A.3).'),
    Case('l2-ktlx-19990504-002218', 'nexrad', ('nexrad-level2', 'message1', 'nul-icao'),
         'ARCHIVE2.036 Message 1 with NUL ICAO and no site location, VCP 11, 16 sweeps. '
         'xradar 0.12 fails to open the volume; Py-ART reads it (design note A.3).'),
    Case('odim-dkrom-20260820-1130-pvol', 'odim', ('odim-h5', 'pvol', 'dualpol'),
         'DMI Romo PVOL, 10 sweeps x 8 quantities (VRAD not VRADH; TH in dBZ; LDR all nodata), '
         'equal ODIM start and end times, which xradar warns about (design note A.4).'),
    Case('odim-iesha-20260305-0115-pvol', 'odim', ('odim-h5', 'pvol', 'gate-tiers', 'vertical-sweep'),
         'Met Eireann Shannon PVOL, 10 sweeps with gate tiers 497/350/240/100 and a 90 deg top '
         'sweep, measured azimuths and ray times; first_dim=time rotates rays (design note A.4).'),
    Case('odim-espdg-20260707-1927-pvol-dbzh-vradh', 'odim', ('odim-h5', 'pvol', 'float64'),
         'AEMET Perdiguera PVOL, 2 sweeps, DBZH and VRADH as float64 planes (gain 1, offset 0, '
         'nodata 95.5, undetect -32; design note 7.2).'),
    Case('cfrad1-irene-sr2-20110827-120420-sur-sweeps01', 'cfradial1', ('cfradial1', 'ppi', 'int8-packed'),
         'SMART-R2 Radx CfRadial 1.3 PPI, 2 sweeps, int8 packed DBZ/VEL with _FillValue -128, '
         'per-ray instrument variables and r_calib_* calibration (design note A.5).'),
    Case('cfrad1-dow8-20211011-223602-rhi-trim3-classic', 'cfradial1',
         ('cfradial1', 'rhi', 'int16-packed', 'moving-platform'),
         'DOW8 Radx CfRadial 1.4 RHI, 1 sweep, int16 packed DBZHC/VEL/WIDTH with float32 '
         'scale_factor, georeference variables; xradar uses dimension azimuth (design note A.5).'),
    Case('cfrad1-xsapr-sgp-20110520-ppi-netcdf4', 'cfradial1', ('cfradial1', 'ppi', 'float32'),
         'ARM X-SAPR CfRadial 1.2 PPI (netCDF-4), 1 sweep of 40 rays x 42 gates, float32 '
         'reflectivity_horizontal with _FillValue -9999 (design note 7.3).'),
)

XRADAR_OPENERS = {
    'nexrad': 'open_nexradlevel2_datatree',
    'odim': 'open_odim_datatree',
    'cfradial1': 'open_cfradial1_datatree',
}
XRADAR_VIEWS = (
    ('time', {'first_dim': 'time', 'mask_and_scale': False, 'optional_groups': True}),
    ('auto', {'first_dim': 'auto', 'mask_and_scale': False, 'optional_groups': True}),
)
XRADAR_DECODED = {'first_dim': 'time', 'optional_groups': True}

PYART_READERS = {
    'nexrad': ('pyart.io.read_nexrad_archive', {'linear_interp': False}),
    'odim': ('pyart.aux_io.read_odim_h5', {}),
    'cfradial1': ('pyart.io.read_cfradial', {}),
}


# --------------------------------------------------------------------------------------
# Manifest and file resolution
# --------------------------------------------------------------------------------------

def load_manifest():
    paths = []
    top = TESTDATA / 'manifest.toml'
    if top.is_file():
        paths.append(top)
    paths += sorted(p for p in TESTDATA.glob('*/manifest.toml') if p.is_file())
    entries = {}
    for path in paths:
        with open(path, 'rb') as f:
            doc = tomllib.load(f)
        for entry in doc.get('file', []):
            if entry['id'] in entries:
                raise SystemExit(f'duplicate testdata id {entry["id"]} in {path}')
            entries[entry['id']] = entry
    return entries


def cache_dir():
    for name in ('RECAST_RADAR_TESTDATA',):
        if os.environ.get(name):
            return Path(os.environ[name])
    base = None
    if os.name == 'nt' and os.environ.get('LOCALAPPDATA'):
        base = Path(os.environ['LOCALAPPDATA'])
    elif os.environ.get('XDG_CACHE_HOME'):
        base = Path(os.environ['XDG_CACHE_HOME'])
    elif os.environ.get('HOME'):
        base = Path(os.environ['HOME']) / '.cache'
    if base is None:
        return ROOT / '.testdata-cache'
    return base / 'recast-radar-tools' / 'testdata'


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for block in iter(lambda: f.read(1 << 20), b''):
            h.update(block)
    return h.hexdigest()


def resolve(entry, offline):
    committed = entry.get('committed')
    if committed:
        rel = committed[len('testdata/'):] if committed.startswith('testdata/') else committed
        path = TESTDATA / rel
    else:
        path = cache_dir() / entry['id']
        if not path.is_file():
            if offline:
                raise SystemExit(f'{entry["id"]}: not cached at {path} and --offline given')
            download(entry, path)
    digest = sha256_file(path)
    if digest != entry['sha256']:
        raise SystemExit(f'{entry["id"]}: sha256 {digest} != manifest {entry["sha256"]} ({path})')
    return path


def download(entry, path):
    path.parent.mkdir(parents=True, exist_ok=True)
    errors = []
    for url in entry.get('urls', []):
        tmp = path.with_name(path.name + '.part')
        try:
            print(f'  downloading {url}', file=sys.stderr)
            with urllib.request.urlopen(url, timeout=120) as resp, open(tmp, 'wb') as out:
                shutil.copyfileobj(resp, out)
            if sha256_file(tmp) != entry['sha256']:
                raise ValueError('sha256 mismatch')
            os.replace(tmp, path)
            return
        except Exception as exc:  # noqa: BLE001 - try the next URL
            errors.append(f'{url}: {exc}')
            if tmp.exists():
                tmp.unlink()
    raise SystemExit(f'{entry["id"]}: download failed: {"; ".join(errors) or "no urls"}')


# --------------------------------------------------------------------------------------
# JSON value helpers
# --------------------------------------------------------------------------------------

def jfloat(x):
    x = float(x)
    if math.isnan(x):
        return 'NaN'
    if math.isinf(x):
        return 'Infinity' if x > 0 else '-Infinity'
    return x


def typed(v):
    """Return (plain JSON value, type name) for an attribute or metadata value."""
    if v is None:
        return None, 'none'
    if isinstance(v, bool):
        return v, 'bool'
    if isinstance(v, int):
        return v, 'int'
    if isinstance(v, float):
        return jfloat(v), 'float'
    if isinstance(v, str):
        return v, 'str'
    if isinstance(v, bytes):
        return v.decode('latin-1'), 'bytes'
    if isinstance(v, np.ndarray):
        return [typed(x)[0] for x in v.ravel().tolist()], f'{dtype_name(v.dtype)}[]'
    if isinstance(v, np.generic):
        if isinstance(v, np.str_):
            return str(v), 'str'
        if isinstance(v, np.bytes_):
            return bytes(v).decode('latin-1'), 'bytes'
        item = v.item()
        if isinstance(item, float):
            item = jfloat(item)
        elif isinstance(item, bytes):
            item = item.decode('latin-1')
        return item, dtype_name(v.dtype)
    if isinstance(v, (list, tuple)):
        return [typed(x)[0] for x in v], 'list'
    if isinstance(v, dict):
        return {str(k): typed(v[k])[0] for k in sorted(v, key=str)}, 'dict'
    if isinstance(v, np.dtype):
        return dtype_name(v), 'dtype'
    return str(v), type(v).__name__


def typed_map(d):
    """Typed attributes, keys sorted (source dictionary order is not stable across runs)."""
    values, types = {}, {}
    for k in sorted(d, key=str):
        values[str(k)], types[str(k)] = typed(d[k])
    return values, types


def dtype_name(dt):
    dt = np.dtype(dt)
    if dt.kind in 'US':
        return dt.str.lstrip('<>|=')
    return dt.name


def canonical_bytes(arr):
    """Row-major little-endian bytes of `arr` with NaN canonicalized (see module doc)."""
    a = np.asarray(arr)
    if a.dtype.kind == 'M':
        a = a.astype('datetime64[ns]').view(np.int64)
    elif a.dtype.kind == 'b':
        a = a.astype(np.uint8)
    elif a.dtype.kind == 'f':
        a = np.where(np.isnan(a), a.dtype.type(np.nan), a)
    if a.dtype.itemsize > 1:
        a = a.astype(a.dtype.newbyteorder('<'), copy=False)
    return np.ascontiguousarray(a).tobytes()


def sha256_array(arr):
    return hashlib.sha256(canonical_bytes(arr)).hexdigest()


def element(x, kind):
    if kind == 'M':
        ns = np.asarray(x).astype('datetime64[ns]').view(np.int64).item()
        return None if ns == np.iinfo(np.int64).min else ns / 1e9
    if kind == 'S':
        return bytes(x).decode('latin-1')
    if kind == 'U':
        return str(x)
    if kind == 'O':
        return typed(x)[0]
    if kind == 'f':
        return jfloat(x)
    if kind == 'b':
        return bool(x)
    return int(x)


def summarize(arr, with_hash=True):
    """Value summary of an array (see module doc)."""
    a = np.asarray(arr)
    kind = a.dtype.kind
    out = {'dtype': dtype_name(a.dtype), 'shape': list(a.shape)}
    if a.ndim == 0:
        out['value'] = element(a.item() if kind not in 'M' else a, kind)
        return out
    flat = a.ravel()
    if kind in 'USO':
        if kind == 'S' and a.dtype.itemsize == 1 and a.ndim >= 2:
            # netCDF character array: one string per row of the last dimension, NULs removed.
            rows = a.reshape(-1, a.shape[-1])
            out['strings'] = [b''.join(row.tolist()).decode('latin-1').replace(chr(0), '') for row in rows]
        else:
            out['values'] = [element(x, kind) for x in flat.tolist()]
        return out
    out['count'] = int(a.size)
    if with_hash:
        out['sha256'] = sha256_array(a)
    if kind in 'iub':
        if a.size:
            out['min'] = int(flat.min())
            out['max'] = int(flat.max())
            if a.dtype.itemsize < 8:
                # int64 cannot overflow: fewer than 2**31 elements of at most 32 bits.
                out['sum'] = int(flat.astype(np.int64).sum())
            else:
                out['sum'] = int(flat.astype(object).sum())
        else:
            out['min'] = out['max'] = None
            out['sum'] = 0
    elif kind == 'f':
        finite_mask = np.isfinite(flat)
        out['count_nan'] = int(np.isnan(flat).sum())
        out['count_inf'] = int(np.isinf(flat).sum())
        fin = flat[finite_mask].astype(np.float64)
        out['min'] = jfloat(fin.min()) if fin.size else None
        out['max'] = jfloat(fin.max()) if fin.size else None
        out['mean'] = jfloat(fin.mean()) if fin.size else None
    elif kind == 'M':
        ns = flat.astype('datetime64[ns]').view(np.int64)
        nat = ns == np.iinfo(np.int64).min
        out['count_nat'] = int(nat.sum())
        secs = ns[~nat] / 1e9
        out['min'] = jfloat(secs.min()) if secs.size else None
        out['max'] = jfloat(secs.max()) if secs.size else None
        out['mean'] = jfloat(secs.mean()) if secs.size else None
    else:
        raise TypeError(f'unsupported dtype {a.dtype}')
    if a.ndim == 1:
        out['first'] = [element(x, kind) for x in (a[:3] if kind == 'M' else a[:3].tolist())]
        out['last'] = element(a[-1] if kind == 'M' else a[-1].item(), kind)
        if kind == 'M':
            ns = a.astype('datetime64[ns]').view(np.int64)
            out['non_decreasing'] = bool(np.all(np.diff(ns) >= 0))
        else:
            with np.errstate(invalid='ignore'):
                out['non_decreasing'] = bool(np.all(np.diff(a.astype(np.float64)) >= 0))
        if a.size <= FULL_VALUES_LIMIT:
            out['values'] = [element(x, kind) for x in (a if kind == 'M' else a.tolist())]
    return out


# --------------------------------------------------------------------------------------
# Warning capture
# --------------------------------------------------------------------------------------

class WarningLog:
    def __init__(self, replacements):
        self.replacements = [r for r in replacements if r]
        self.items = []

    def add(self, caught):
        for w in caught:
            msg = str(w.message)
            if any(noise in msg for noise in NOISE_WARNINGS):
                continue
            for old, new in self.replacements:
                msg = msg.replace(old, new)
            text = f'{w.category.__name__}: {msg}'
            if text not in self.items:
                self.items.append(text)


def path_replacements(*paths):
    out = []
    for p in paths:
        if p is None:
            continue
        for s in {str(p), str(p).replace('\\', '/'), str(p).replace('/', '\\')}:
            out.append((s, '<input>'))
    return out


def error_record(exc, replacements):
    msg = str(exc)
    for old, new in replacements:
        msg = msg.replace(old, new)
    return {'type': type(exc).__name__, 'message': msg}


# --------------------------------------------------------------------------------------
# xradar
# --------------------------------------------------------------------------------------

def xr_encoding(var):
    enc = {k: var.encoding[k] for k in ENCODING_KEYS if k in var.encoding}
    return typed_map(enc)


def xr_group(node):
    ds = node.to_dataset(inherit=False)
    group = {'path': node.path, 'dims': {str(k): int(ds.sizes[k]) for k in sorted(ds.sizes, key=str)}}
    group['attrs'], group['attr_types'] = typed_map(ds.attrs)
    variables = []
    for role, names in (('coord', sorted(ds.coords, key=str)), ('data', sorted(ds.data_vars, key=str))):
        for name in names:
            var = ds[name]
            item = {'name': str(name), 'role': role, 'dims': [str(d) for d in var.dims],
                    'dtype': dtype_name(var.dtype)}
            item['attrs'], item['attr_types'] = typed_map(var.attrs)
            item['encoding'], item['encoding_types'] = xr_encoding(var)
            item['values'] = summarize(var.values)
            variables.append(item)
    group['variables'] = variables
    return group


def strip_same_meta(auto_groups, time_groups):
    """Drop attrs/encoding from the auto view where they equal the time view."""
    meta_keys = ('attrs', 'attr_types')
    var_keys = ('attrs', 'attr_types', 'encoding', 'encoding_types')
    by_path = {g['path']: g for g in time_groups}
    for g in auto_groups:
        tg = by_path.get(g['path'])
        if tg is not None and all(g[k] == tg[k] for k in meta_keys):
            for k in meta_keys:
                del g[k]
        tvars = {v['name']: v for v in tg['variables']} if tg else {}
        for v in g['variables']:
            tv = tvars.get(v['name'])
            if tv is not None and all(v[k] == tv[k] for k in var_keys):
                for k in var_keys:
                    del v[k]


def xradar_golden(case, entry, path, versions, tmp):
    import xradar as xd

    opener = getattr(xd.io, XRADAR_OPENERS[case.kind])
    golden = header(SCHEMA_XRADAR, case, entry, versions['xradar_stack'])
    input_path, transform = path, 'file'
    with open(path, 'rb') as f:
        magic = f.read(2)
    if case.kind == 'nexrad' and magic == GZIP_MAGIC:
        # xradar 0.12 cannot read whole-file gzip; hand it the decompressed bytes.
        input_path = Path(tmp) / (entry['id'] + '.raw')
        with gzip.open(path, 'rb') as src, open(input_path, 'wb') as dst:
            shutil.copyfileobj(src, dst)
        transform = 'gunzip'
    golden['reader'] = {
        'function': f'xradar.io.{XRADAR_OPENERS[case.kind]}',
        'input': transform,
        'input_sha256': sha256_file(input_path),
        'input_size': input_path.stat().st_size,
    }
    replacements = path_replacements(input_path, path, Path(tmp))
    log = WarningLog(replacements)
    golden.update({'status': 'ok', 'error': None, 'warnings': log.items, 'views': None, 'decoded': None})
    caught = []
    try:
        views = {}
        for name, kwargs in XRADAR_VIEWS:
            with warnings.catch_warnings(record=True) as caught:
                warnings.simplefilter('always')
                dt = opener(str(input_path), **kwargs)
                groups = [xr_group(node) for node in dt.subtree]
                dt.close()
            log.add(caught)
            caught = []
            views[name] = {'kwargs': kwargs, 'groups': groups}
        strip_same_meta(views['auto']['groups'], views['time']['groups'])
        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter('always')
            dt = opener(str(input_path), **XRADAR_DECODED)
            decoded_groups = []
            for node in dt.subtree:
                ds = node.to_dataset(inherit=False)
                variables = []
                for role, names in (('coord', sorted(ds.coords, key=str)),
                                    ('data', sorted(ds.data_vars, key=str))):
                    for name in names:
                        var = ds[name]
                        item = {'name': str(name), 'role': role, 'dims': [str(d) for d in var.dims],
                                'dtype': dtype_name(var.dtype)}
                        item['attrs'], item['attr_types'] = typed_map(var.attrs)
                        item['encoding'], item['encoding_types'] = xr_encoding(var)
                        item['values'] = summarize(var.values, with_hash=False)
                        variables.append(item)
                decoded_groups.append({'path': node.path, 'variables': variables})
            dt.close()
        log.add(caught)
        golden['views'] = views
        golden['decoded'] = {'kwargs': XRADAR_DECODED, 'groups': decoded_groups}
    except Exception as exc:  # noqa: BLE001 - recorded in the golden
        log.add(caught)
        golden['status'] = 'error'
        golden['error'] = error_record(exc, replacements)
        golden['views'] = None
        golden['decoded'] = None
        traceback.print_exc(limit=2, file=sys.stderr)
    return golden, input_path


# --------------------------------------------------------------------------------------
# Py-ART
# --------------------------------------------------------------------------------------

PYART_DICT_ATTRS = (
    'time', 'range', 'azimuth', 'elevation', 'fixed_angle', 'sweep_number', 'sweep_mode',
    'sweep_start_ray_index', 'sweep_end_ray_index', 'target_scan_rate', 'rays_are_indexed',
    'ray_angle_res', 'scan_rate', 'antenna_transition', 'latitude', 'longitude', 'altitude',
    'altitude_agl', 'rotation', 'tilt', 'roll', 'drift', 'heading', 'pitch', 'georefs_applied',
)


def pyart_variable(d, nrays, sweeps):
    meta = {k: v for k, v in d.items() if k != 'data'}
    item = {}
    item['meta'], item['meta_types'] = typed_map(meta)
    data = d.get('data')
    arr = np.ma.getdata(data) if np.ma.isMaskedArray(data) else np.asarray(data)
    if np.ma.isMaskedArray(data) and np.ma.getmaskarray(data).any():
        item['masked_count'] = int(np.ma.getmaskarray(data).sum())
    item['values'] = summarize(arr)
    if arr.ndim == 1 and arr.shape[0] == nrays and arr.dtype.kind not in 'USO':
        item['per_sweep'] = [summarize(arr[s['start']:s['end'] + 1]) for s in sweeps]
    return item


def field_stats(data, fill_value):
    mask = np.ma.getmaskarray(data)
    vals = np.ma.getdata(data)
    out = {'count_unmasked': int((~mask).sum()), 'count_masked': int(mask.sum())}
    unmasked = vals[~mask]
    if vals.dtype.kind == 'f':
        out['count_nan_unmasked'] = int(np.isnan(unmasked).sum())
        fin = unmasked[np.isfinite(unmasked)].astype(np.float64)
    else:
        out['count_nan_unmasked'] = 0
        fin = unmasked.astype(np.float64)
    out['min'] = jfloat(fin.min()) if fin.size else None
    out['max'] = jfloat(fin.max()) if fin.size else None
    out['mean'] = jfloat(fin.mean()) if fin.size else None
    if vals.ndim == 2 and vals.shape[1]:
        valid = ~mask
        any_valid = valid.any(axis=1)
        last = vals.shape[1] - np.argmax(valid[:, ::-1], axis=1)
        out['extent'] = int(last[any_valid].max()) if any_valid.any() else 0
    else:
        out['extent'] = 0
    if vals.dtype.kind == 'f':
        filled = np.where(mask, vals.dtype.type(np.nan), vals)
        out['sha256'] = sha256_array(filled)
    elif fill_value is not None:
        filled = np.where(mask, np.asarray(fill_value).astype(vals.dtype), vals)
        out['sha256'] = sha256_array(filled)
    else:
        out['sha256'] = None if mask.any() else sha256_array(vals)
    return out


def pyart_golden(case, entry, path, versions):
    function, kwargs = PYART_READERS[case.kind]
    golden = header(SCHEMA_PYART, case, entry, versions['pyart_stack'])
    golden['reader'] = {'function': function, 'input': 'file', 'kwargs': kwargs}
    replacements = path_replacements(path)
    log = WarningLog(replacements)
    golden.update({'status': 'ok', 'error': None, 'warnings': log.items, 'radar': None, 'fields': None})
    import pyart

    reader = {'pyart.io.read_nexrad_archive': pyart.io.read_nexrad_archive,
              'pyart.aux_io.read_odim_h5': pyart.aux_io.read_odim_h5,
              'pyart.io.read_cfradial': pyart.io.read_cfradial}[function]
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter('always')
        try:
            radar = reader(str(path), **kwargs)
        except Exception as exc:  # noqa: BLE001 - recorded in the golden
            radar = None
            golden['status'] = 'error'
            golden['error'] = error_record(exc, replacements)
            traceback.print_exc(limit=2, file=sys.stderr)
        if radar is not None:
            golden['radar'], golden['fields'] = pyart_radar(radar)
    log.add(caught)
    return golden


def pyart_radar(radar):
    starts = np.asarray(radar.sweep_start_ray_index['data']).tolist()
    ends = np.asarray(radar.sweep_end_ray_index['data']).tolist()
    sweeps = [{'sweep': i, 'start': int(s), 'end': int(e), 'rays': int(e) - int(s) + 1}
              for i, (s, e) in enumerate(zip(starts, ends))]
    out = {'scan_type': radar.scan_type, 'nrays': int(radar.nrays), 'ngates': int(radar.ngates),
           'nsweeps': int(radar.nsweeps)}
    out['metadata'], out['metadata_types'] = typed_map(radar.metadata)
    out['sweeps'] = sweeps
    variables = {}
    for name in PYART_DICT_ATTRS:
        d = getattr(radar, name, None)
        if isinstance(d, dict) and 'data' in d:
            variables[name] = pyart_variable(d, radar.nrays, sweeps)
    out['variables'] = variables
    for group in ('instrument_parameters', 'radar_calibration'):
        g = getattr(radar, group, None)
        if g is None:
            out[group] = None
            continue
        out[group] = {str(k): pyart_variable(g[k], radar.nrays, sweeps) for k in sorted(g)}
    fields = {}
    for name in sorted(radar.fields):
        d = radar.fields[name]
        data = d['data']
        meta = {k: v for k, v in d.items() if k != 'data'}
        item = {}
        item['meta'], item['meta_types'] = typed_map(meta)
        item['dtype'] = dtype_name(np.ma.getdata(data).dtype)
        item['shape'] = list(np.shape(data))
        item['masked'] = bool(np.ma.isMaskedArray(data))
        fill = meta.get('_FillValue')
        item['total'] = field_stats(data, fill)
        per_sweep = []
        for s in sweeps:
            st = field_stats(data[s['start']:s['end'] + 1], fill)
            per_sweep.append({'sweep': s['sweep'], 'rays': s['rays'], 'gates': int(np.shape(data)[1]), **st})
        item['per_sweep'] = per_sweep
        fields[str(name)] = item
    return out, fields


# --------------------------------------------------------------------------------------
# Cross-reader verification (--verify)
# --------------------------------------------------------------------------------------

NEXRAD_MOMENTS = (
    # (xradar name, ICD block name, Py-ART name)
    ('DBZH', 'REF', 'reflectivity'),
    ('VRADH', 'VEL', 'velocity'),
    ('WRADH', 'SW', 'spectrum_width'),
    ('ZDR', 'ZDR', 'differential_reflectivity'),
    ('PHIDP', 'PHI', 'differential_phase'),
    ('RHOHV', 'RHO', 'cross_correlation_ratio'),
    ('CCORH', 'CFP', 'clutter_filter_power_removed'),
)
# Names aux_io.read_odim_h5 gives each ODIM quantity (design note 8.2 note 3).
PYART_ODIM_NAMES = {
    'DBZH': 'reflectivity_horizontal', 'TH': 'total_power_horizontal', 'VRADH': 'velocity_horizontal',
    'VRAD': 'velocity', 'WRAD': 'spectrum_width', 'ZDR': 'differential_reflectivity',
    'RHOHV': 'cross_correlation_ratio', 'PHIDP': 'differential_phase', 'LDR': 'linear_polarization_ratio',
}
VERIFY_REL_TOL = 1e-4


class Verify:
    def __init__(self, case_id):
        self.case_id = case_id
        self.counts = {}
        self.failures = []

    def result(self, check, ok, detail):
        c = self.counts.setdefault(check, [0, 0, 0])
        c[0 if ok is True else 1 if ok is False else 2] += 1
        if ok is False:
            self.failures.append(f'{self.case_id} {check}: {detail}')

    def report(self):
        parts = [f'{k} ok={v[0]} fail={v[1]} skipped={v[2]}' for k, v in self.counts.items()]
        return f'{self.case_id}: ' + ('; '.join(parts) if parts else 'nothing to verify')


def metpy_sweeps(path):
    """MetPy Level2File sweeps: per ray, {moment name: (header, scaled values)}."""
    import io
    import logging

    from metpy.io import Level2File

    logging.getLogger('metpy').setLevel(logging.CRITICAL)
    data = Path(path).read_bytes()
    if data[:2] == GZIP_MAGIC:
        data = gzip.decompress(data)
    with warnings.catch_warnings():
        warnings.simplefilter('ignore')
        f = Level2File(io.BytesIO(data))
    out = []
    for sweep in f.sweeps:
        rays = []
        for ray in sweep:
            moments = ray[-1]
            rays.append({(k.decode() if isinstance(k, bytes) else str(k)).strip(): val
                         for k, val in moments.items()})
        out.append(rays)
    return out


def icd_physical(raw, scale, offset):
    """(raw - offset) / scale in float32 with raw 0 and 1 as NaN: Py-ART's NEXRAD evaluation."""
    raw = np.asarray(raw)
    phys = (raw.astype(np.float32) - np.float32(offset)) / np.float32(scale)
    return np.where(raw <= 1, np.float32(np.nan), phys).astype(np.float32)


def verify_nexrad(case, path, xradar_input, xg, pg, v):
    """Two checks against Py-ART's per-sweep field hashes (read with linear_interp=False).

    ``metpy->pyart``: MetPy's native moments, evaluated as ICD float32, placed on Py-ART's
    volume range with each native gate repeated gate_width / spacing times (design note 6.5).
    ``xradar->pyart``: xradar's raw array (view ``time``) evaluated the same way and padded, for
    sweeps whose xradar range starts and steps like Py-ART's and fields whose MetPy native
    geometry equals that range (xradar misplaces the others; design note 6.2).
    """
    if pg['status'] != 'ok':
        v.result('metpy->pyart', None, 'Py-ART failed')
        return
    radar, fields = pg['radar'], pg['fields']
    rng = radar['variables']['range']['values']['first']
    r0, dr, ngates = rng[0], rng[1] - rng[0], radar['ngates']
    msweeps = metpy_sweeps(path)
    if len(msweeps) != radar['nsweeps']:
        v.result('metpy->pyart', False, f'MetPy has {len(msweeps)} sweeps, Py-ART {radar["nsweeps"]}')
        return
    geometry = []  # per sweep: block -> (first gate centre m, gate width m, scale, offset)
    for s, rays in enumerate(msweeps):
        info = radar['sweeps'][s]
        if len(rays) != info['rays']:
            v.result('metpy->pyart', False, f'sweep {s}: MetPy {len(rays)} rays, Py-ART {info["rays"]}')
            geometry.append({})
            continue
        geo = {}
        for _, block, pname in NEXRAD_MOMENTS:
            if pname not in fields:
                continue
            out = np.full((len(rays), ngates), np.float32(np.nan), dtype=np.float32)
            for r, moments in enumerate(rays):
                if block not in moments:
                    continue
                hdr, vals = moments[block]
                # MetPy returns scaled values with raw 0 and 1 as NaN; both are masked by Py-ART.
                raw = np.where(np.isnan(vals), 0, np.rint(vals * hdr.scale + hdr.offset)).astype(np.int64)
                first, width = hdr.first_gate * 1000.0, hdr.gate_width * 1000.0
                geo.setdefault(block, (first, width, hdr.scale, hdr.offset))
                k = int(round(width / dr))
                m = int(round(((first - width / 2) - (r0 - dr / 2)) / dr))
                repeated = np.repeat(icd_physical(raw, hdr.scale, hdr.offset), k)
                lo, hi = max(m, 0), min(m + repeated.size, ngates)
                out[r, lo:hi] = repeated[lo - m:hi - m]
            want = fields[pname]['per_sweep'][s]['sha256']
            v.result('metpy->pyart', sha256_array(out) == want, f'sweep {s} {pname}')
        geometry.append(geo)

    if xg['status'] != 'ok':
        v.result('xradar->pyart', None, 'xradar failed')
        return
    import xradar as xd

    nsweeps = sum(1 for g in xg['views']['time']['groups'] if g['path'].startswith('/sweep_'))
    if nsweeps != radar['nsweeps']:
        v.result('xradar->pyart', None, f'xradar has {nsweeps} sweeps, Py-ART {radar["nsweeps"]}')
        return
    with warnings.catch_warnings():
        warnings.simplefilter('ignore')
        dt = xd.io.open_nexradlevel2_datatree(str(xradar_input), **dict(XRADAR_VIEWS)['time'])
    try:
        for s in range(nsweeps):
            ds = dt[f'sweep_{s}'].to_dataset(inherit=False)
            xr_rng = ds['range'].values.astype(np.float64)
            xr0 = float(xr_rng[0])
            xdr = float(xr_rng[1] - xr_rng[0]) if xr_rng.size > 1 else dr
            for xname, block, pname in NEXRAD_MOMENTS:
                if xname not in ds or pname not in fields:
                    continue
                geo = geometry[s].get(block)
                if (xr0, xdr) != (r0, dr) or geo is None or (geo[0], geo[1]) != (xr0, xdr):
                    v.result('xradar->pyart', None, f'sweep {s} {xname}: geometry differs')
                    continue
                raw = ds[xname].values
                out = np.full((raw.shape[0], ngates), np.float32(np.nan), dtype=np.float32)
                out[:, :raw.shape[1]] = icd_physical(raw, geo[2], geo[3])
                want = fields[pname]['per_sweep'][s]['sha256']
                v.result('xradar->pyart', sha256_array(out) == want, f'sweep {s} {xname}')
    finally:
        dt.close()


def close_rel(a, b):
    if a is None or b is None:
        return a is None and b is None
    return abs(a - b) <= VERIFY_REL_TOL * max(abs(b), 1e-6)


def verify_stats(case, path, xg, pg, v):
    """ODIM and CfRadial: xradar's raw array (view ``time``), masked at ``_FillValue``,
    ``_Undetect`` and NaN and scaled by ``scale_factor``/``add_offset``, against Py-ART's
    per-sweep count of finite unmasked values (exact) and min, max, mean (relative 1e-4)."""
    if xg['status'] != 'ok' or pg['status'] != 'ok':
        v.result('xradar->pyart stats', None, 'a reader failed')
        return
    import xradar as xd

    opener = getattr(xd.io, XRADAR_OPENERS[case.kind])
    with warnings.catch_warnings():
        warnings.simplefilter('ignore')
        dt = opener(str(path), **dict(XRADAR_VIEWS)['time'])
    try:
        sweeps = sorted((c for c in dt.children if c.startswith('sweep_')), key=lambda c: int(c[6:]))
        if len(sweeps) != pg['radar']['nsweeps']:
            v.result('xradar->pyart stats', False,
                     f'{len(sweeps)} xradar sweeps, {pg["radar"]["nsweeps"]} Py-ART sweeps')
            return
        for s, name in enumerate(sweeps):
            ds = dt[name].to_dataset(inherit=False)
            for var_name in sorted(ds.data_vars):
                var = ds[var_name]
                if var.ndim != 2:
                    continue
                pname = PYART_ODIM_NAMES.get(var_name, var_name) if case.kind == 'odim' else var_name
                if pname not in pg['fields']:
                    v.result('xradar->pyart stats', False, f'sweep {s} {var_name}: no Py-ART field {pname}')
                    continue
                raw = var.values
                attrs = var.attrs
                mask = np.zeros(raw.shape, dtype=bool)
                for key in ('_FillValue', '_Undetect'):
                    if key in attrs:
                        mask |= raw == np.asarray(attrs[key]).astype(raw.dtype)
                if raw.dtype.kind == 'f':
                    mask |= np.isnan(raw)
                scale = float(attrs.get('scale_factor', 1.0))
                offset = float(attrs.get('add_offset', 0.0))
                vals = raw.astype(np.float64)[~mask] * scale + offset
                ps = pg['fields'][pname]['per_sweep'][s]
                count = ps['count_unmasked'] - ps['count_nan_unmasked']
                ok = int(vals.size) == count
                if ok and vals.size:
                    ok = (close_rel(float(vals.min()), ps['min']) and close_rel(float(vals.max()), ps['max'])
                          and close_rel(float(vals.mean()), ps['mean']))
                v.result('xradar->pyart stats', ok,
                         f'sweep {s} {var_name}: xradar n={vals.size}; Py-ART n={count} '
                         f'min={ps["min"]} max={ps["max"]} mean={ps["mean"]}')
    finally:
        dt.close()


def verify_case(case, path, xradar_input, xg, pg):
    v = Verify(case.id)
    if case.kind == 'nexrad':
        verify_nexrad(case, path, xradar_input, xg, pg, v)
    else:
        verify_stats(case, path, xg, pg, v)
    return v


# --------------------------------------------------------------------------------------
# Driver
# --------------------------------------------------------------------------------------

def header(schema, case, entry, generator):
    return {
        'schema': schema,
        'id': case.id,
        'format': entry['format'],
        'sha256': entry['sha256'],
        'size': entry['size'],
        'categories': list(case.categories),
        'note': case.note,
        'generator': generator,
    }


def software_versions():
    import importlib.metadata as md

    def v(dist):
        try:
            return md.version(dist)
        except md.PackageNotFoundError:
            return None

    python = f'{sys.version_info.major}.{sys.version_info.minor}'
    common = {'script': SCRIPT, 'python': python, 'numpy': v('numpy')}
    xradar_stack = {**common, 'xradar': v('xradar'), 'xarray': v('xarray'), 'netCDF4': v('netCDF4'),
                    'h5netcdf': v('h5netcdf'), 'h5py': v('h5py')}
    pyart_stack = {**common, 'arm_pyart': v('arm_pyart'), 'netCDF4': v('netCDF4'), 'h5py': v('h5py')}
    for dist, want in EXPECTED_VERSIONS.items():
        have = v(dist)
        if have != want:
            print(f'warning: {dist} {have} installed, goldens are defined for {want}', file=sys.stderr)
    return {'xradar_stack': xradar_stack, 'pyart_stack': pyart_stack}


def _compact(obj):
    return json.dumps(obj, ensure_ascii=True, separators=(', ', ': '), allow_nan=False)


def to_json(obj, indent=0, width=100):
    """Pretty JSON: containers whose one-line form fits `width` stay on one line."""
    one = _compact(obj)
    if not isinstance(obj, (dict, list)) or len(one) + indent <= width or not obj:
        return one
    pad = ' ' * (indent + 1)
    if isinstance(obj, dict):
        items = [f'{pad}{json.dumps(str(k), ensure_ascii=True)}: {to_json(v, indent + 1, width)}'
                 for k, v in obj.items()]
        return '{\n' + ',\n'.join(items) + '\n' + ' ' * indent + '}'
    items = [f'{pad}{to_json(v, indent + 1, width)}' for v in obj]
    return '[\n' + ',\n'.join(items) + '\n' + ' ' * indent + ']'


def render(obj):
    return to_json(obj) + '\n'


def index_doc(versions, statuses):
    files = []
    for case in CASES:
        st = statuses.get(case.id, {})
        files.append({
            'id': case.id,
            'reader_kind': case.kind,
            'categories': list(case.categories),
            'xradar': f'xradar/{case.id}.json',
            'xradar_status': st.get('xradar'),
            'pyart': f'pyart/{case.id}.json',
            'pyart_status': st.get('pyart'),
            'note': case.note,
        })
    return {
        'schema': SCHEMA_INDEX,
        'generator': {'script': SCRIPT, 'xradar': versions['xradar_stack'],
                      'pyart': versions['pyart_stack']},
        'xradar_views': {name: kwargs for name, kwargs in XRADAR_VIEWS},
        'xradar_decoded': XRADAR_DECODED,
        'pyart_readers': {kind: {'function': f, 'kwargs': kw} for kind, (f, kw) in PYART_READERS.items()},
        'files': files,
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    ap.add_argument('--check', action='store_true', help='fail if golden files on disk differ')
    ap.add_argument('--offline', action='store_true', help='never download; fail if a file is not cached')
    ap.add_argument('--list', action='store_true', help='print the cases and exit')
    ap.add_argument('--verify', action='store_true',
                    help='also cross-check the goldens between readers (needs MetPy); fail on a mismatch')
    ap.add_argument('ids', nargs='*')
    args = ap.parse_args()

    if args.list:
        for case in CASES:
            print(f'{case.id}\t{case.kind}\t{",".join(case.categories)}')
        return 0

    os.environ.setdefault('PYART_QUIET', '1')
    known = {c.id: c for c in CASES}
    unknown = [i for i in args.ids if i not in known]
    if unknown:
        raise SystemExit(f'unknown case id(s): {", ".join(unknown)}')
    selected = [known[i] for i in args.ids] if args.ids else list(CASES)

    manifest = load_manifest()
    versions = software_versions()
    outputs = {}
    statuses = {}
    verify_failures = []
    # One scratch directory for the run: on Windows xradar can keep a decompressed input open
    # until its objects are collected, so cleanup errors are ignored.
    with tempfile.TemporaryDirectory(prefix='fm301_golden_', ignore_cleanup_errors=True) as tmp:
        for case in selected:
            entry = manifest.get(case.id)
            if entry is None:
                raise SystemExit(f'{case.id}: not in the testdata manifests')
            path = resolve(entry, args.offline)
            print(f'{case.id}: {path}', file=sys.stderr)
            xg, xradar_input = xradar_golden(case, entry, path, versions, tmp)
            gc.collect()
            pg = pyart_golden(case, entry, path, versions)
            gc.collect()
            outputs[OUT_DIR / 'xradar' / f'{case.id}.json'] = render(xg)
            outputs[OUT_DIR / 'pyart' / f'{case.id}.json'] = render(pg)
            statuses[case.id] = {'xradar': xg['status'], 'pyart': pg['status']}
            print(f'  xradar {xg["status"]}, pyart {pg["status"]}', file=sys.stderr)
            if args.verify:
                result = verify_case(case, path, xradar_input, xg, pg)
                gc.collect()
                print(f'  verify {result.report()}', file=sys.stderr)
                for failure in result.failures:
                    print(f'  VERIFY FAILED {failure}', file=sys.stderr)
                verify_failures += result.failures

    for case in CASES:
        if case.id in statuses:
            continue
        st = {}
        for reader in ('xradar', 'pyart'):
            p = OUT_DIR / reader / f'{case.id}.json'
            if p.is_file():
                st[reader] = json.loads(p.read_text(encoding='utf-8')).get('status')
        statuses[case.id] = st
    outputs[OUT_DIR / 'index.json'] = render(index_doc(versions, statuses))

    if verify_failures:
        print(f'{len(verify_failures)} cross-reader verification failures; nothing written', file=sys.stderr)
        return 1
    if args.check:
        stale = []
        for p, text in outputs.items():
            if not p.is_file() or p.read_text(encoding='utf-8') != text:
                stale.append(p.relative_to(ROOT).as_posix())
        if stale:
            print('stale goldens:\n  ' + '\n  '.join(stale), file=sys.stderr)
            return 1
        print(f'{len(outputs)} golden files up to date', file=sys.stderr)
        return 0
    for p, text in outputs.items():
        p.parent.mkdir(parents=True, exist_ok=True)
        with open(p, 'w', encoding='utf-8', newline='\n') as f:
            f.write(text)
    print(f'wrote {len(outputs)} files under {OUT_DIR.relative_to(ROOT).as_posix()}', file=sys.stderr)
    return 0


if __name__ == '__main__':
    sys.exit(main())
