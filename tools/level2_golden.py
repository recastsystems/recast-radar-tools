"""Golden values for the Level II message decoders, read with MetPy and Py-ART.

Writes JSON files under testdata/level2/golden/<group>/, one per source. The
Rust tests in crates/recast-radar-io-nexrad/tests/ compare their decoded values
against these files.

Needs MetPy 1.7.1 and Py-ART (arm_pyart) 2.2.5: the
versions the committed files were written with. Other versions are refused,
because the files record them. Files come from the committed path in the
manifest, or else from the shared download cache that recast-radar-testdata
fills ($RECAST_RADAR_TESTDATA, else recast-radar-tools/testdata under
%LOCALAPPDATA% on Windows, $XDG_CACHE_HOME, or ~/.cache), and their sha256
must match the manifest. Run `cargo test -p recast-radar-io-nexrad` once to
download them.

Usage:
    python tools/level2_golden.py <group> [source ...]
    python tools/level2_golden.py all
    python tools/level2_golden.py --check <group|all> [source ...]
    python tools/level2_golden.py --list-sources <group|all>

A source is a manifest id; the msg31 and metadata groups also accept several
ids joined with "+", read as one concatenated file (a real-time volume is its
chunks concatenated). With no sources, the group's default list is
regenerated; `all` regenerates every group with its defaults.

--check writes nothing. It generates the same documents in memory and
compares them with the committed files byte for byte. With a group's default
sources it also reports committed files the script does not produce (and, for
the clutter group, files it would remove). It exits with status 1 on any
difference. `cargo test -p recast-radar-io-nexrad --test golden_script --
--ignored` (or tools/ci/level2-golden-check.sh) downloads every source listed
by --list-sources and runs `--check all`; see docs/level2/messages.md.

--list-sources prints the manifest ids a group reads with its defaults, one
per line.

Groups
------
status  (tests/messages_status.rs)
    Messages 2 (RDA Status Data), 3 (Performance/Maintenance Data) and 18
    (RDA Adaptation Data), the first of each in the file, as MetPy's
    `Level2File` exposes them in `rda_status`, `maintenance_data` and `rda`.
    Radial messages 1 and 31 are skipped (not needed, and faster).

    - `message_2.fields`: MetPy's values after its converters (bit fields as
      MetPy's names, scaled calibration corrections, the build as a string).
      `message_2.codes`: the same fields unpacked with MetPy's own
      `Level2File.msg2_fmt` struct before the converters (except the alarm
      array), because MetPy's bit-name lists drop bits past the names they
      know and repeat names.
      `additional` is MetPy's `msg2_additional_fmt` part (Build 18 and later
      bodies), or null. `channels` is the raw RDA channel byte of the message
      header and `size_hw` its size in halfwords.
    - `message_3.halfwords`: MetPy's `maintenance_data` keyed by the 1-based
      halfword where each field starts in MetPy's layout (its generated
      `_nexrad_msgs/msg3.py`), with MetPy's name and struct format. MetPy's
      layout predates Build 17, so some names differ from ICD 2620002AA at the
      same location; the Rust test compares by location and type. Null when
      MetPy skips the message (legacy 1040-byte bodies).
    - `message_18.bytes`: MetPy's `rda` dict keyed by byte offset in MetPy's
      layout (`_nexrad_msgs/msg18.py`), with name and format. The VCPAT
      entries (default VCP tables, parsed by MetPy into VCP records or dropped)
      are left out. Null when MetPy skips the message (legacy files).

    Floats that are NaN or infinite are written as the strings "NaN",
    "Infinity" and "-Infinity"; byte strings as Latin-1 text including NULs.

vcp  (tests/messages_vcp.rs)
    Message 5 (Volume Coverage Pattern, ICD 2620002AA Table XI) as MetPy's
    `Level2File.vcp_info` exposes it. MetPy decodes code fields to names;
    they are mapped back to the numeric codes with MetPy's own `Enum` and
    `BitField` tables, so the golden files hold what MetPy read from the
    bytes, not MetPy's naming. (MetPy's names for the super resolution bits
    predate the Build 24.0 table: bit 1 is "1/4 km reflectivity" and bit 2
    "Doppler to 300 km" in the ICD.) `message_5` is null when MetPy skips the
    message (its size halfword is 0).

    For volumes that also carry Message 32 (RDA PRF Data), which MetPy does
    not decode, `message_31_sweeps` lists per sweep the elevation number and
    the distinct unambiguous ranges (km) and Nyquist velocities (m/s) in the
    Message 31 radial (RAD) blocks, as MetPy reads them. The Rust test checks
    the PRFs selected through Messages 5 and 32 against them.

clutter  (tests/messages_clutter.rs)
    Message 15 (Level2File.clutter_filter_map) and Message 13
    (Level2File.clutter_filter_bypass_map). For each manifest id, MetPy's
    `Level2File` reads the Archive II metadata record: the same bytes
    `messages::metadata_record` returns in Rust (the first LDM record, or else
    the first 134 frames), with the file's 24-byte volume header in front.
    MetPy skips radial messages 1 and 31, because MetPy 1.7.1 raises an error
    on the Message 1 frames in the KLIX 2005 metadata record. The default ids
    are every id with format "nexrad-level2" in testdata/level2/manifest.toml
    that is not derived from another entry (trimmed fixtures, which
    tests/messages_clutter.rs compares with their source files instead); a
    golden file is written only where MetPy decoded one of the two messages
    or logged a note about them (and removed otherwise). See `clutter` below
    for where MetPy's output differs from the ICD.

msg31  (tests/messages_msg31.rs)
    Message 31 Data Header Block, VOL/ELV/RAD constant blocks and data moment
    block descriptors per sweep, the RDA build from message 2, and the
    message 5 SNR thresholds per elevation cut, read from whole files. For
    every sweep MetPy forms, `first` is the first radial's fields and
    `fields` summarizes each field over all radials (distinct values, or
    count/min/max/sum when there are more than 12). The JSON records what
    MetPy exposes, normalized only where MetPy's converters return
    Python-only types (bytes become text, BitField lists become "A|B"
    strings, None becomes ""). A "+"-joined source is written as
    <first-id>..<last chunk number>.json.

metadata  (tests/volume_metadata.rs)
    What Py-ART's own Level II reader (`pyart.io.nexrad_level2.
    NEXRADLevel2File`, used by `read_nexrad_archive`) reads for
    `read_volume_with_metadata`: the radial message type, the VCP number
    from message 5, and per scan the ray count, the message 5 target
    elevation angle code, and the first ray's Data Header Block fields and
    VOL, ELV and RAD blocks. Py-ART groups rays into scans by elevation
    number (scan i holds elevation number i + 1). Values are Py-ART's raw
    unpacked fields before its scaling; the two-byte spare fields (VOL
    processing status, RAD radial flags) are written as big-endian integers.
    Files are decompressed as `read_nexrad_archive` does; sources joined with
    "+" are concatenated and named as in msg31.

volume  (tests/volume_pyart.rs)
    What Py-ART's `NEXRADLevel2File` reads for `read_volume_from_bytes`:
    the volume header ICAO, the ray count, and per scan the ray count, the
    sums of the rays' collection times (ms) and azimuths (degrees), and for
    each moment Py-ART names (REF, VEL, SW, ZDR, PHI, RHO, CFP) the rays
    that carry it, the distinct gate counts, first gate ranges, gate
    spacings, word sizes, scales and offsets, the total gate count, and the
    sum of the raw gate codes with the counts of codes 0 and 1. Message 31
    files only. The sources include KVWX 2008-04-15, whose message 31 radar
    identifiers are four spaces.
"""

import argparse
import bz2
import gzip
import hashlib
import io
import json
import logging
import math
import os
import re
import struct
import sys
import tomllib

import metpy
from metpy.io import Level2File
from metpy.io._nexrad_msgs import msg3 as metpy_msg3
from metpy.io._nexrad_msgs import msg18 as metpy_msg18

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TESTDATA = os.path.join(ROOT, 'testdata')
GOLDEN_DIR = os.path.join(TESTDATA, 'level2', 'golden')
sys.path.insert(0, os.path.join(ROOT, 'tools'))
sys.dont_write_bytecode = True  # keep tools/ free of __pycache__
import level2_message_scan as scan  # noqa: E402

EXPECTED_METPY = '1.7.1'
EXPECTED_PYART = '2.2.5'


# --- corpus files ---------------------------------------------------------------


def cache_dir():
    """The download cache, resolved like recast_radar_testdata::cache_dir."""
    override = os.environ.get('RECAST_RADAR_TESTDATA')
    if override:
        return override
    base = None
    if os.name == 'nt' and os.environ.get('LOCALAPPDATA'):
        base = os.environ['LOCALAPPDATA']
    elif os.environ.get('XDG_CACHE_HOME'):
        base = os.environ['XDG_CACHE_HOME']
    elif os.environ.get('HOME'):
        base = os.path.join(os.environ['HOME'], '.cache')
    if base is None:
        return os.path.join(ROOT, '.testdata-cache')
    return os.path.join(base, 'recast-radar-tools', 'testdata')


_ENTRIES = None


def manifest_entries():
    """Every manifest entry by id: testdata/manifest.toml, then
    testdata/*/manifest.toml in directory order."""
    global _ENTRIES
    if _ENTRIES is None:
        entries = {}
        paths = [os.path.join(TESTDATA, 'manifest.toml')]
        for name in sorted(os.listdir(TESTDATA)):
            candidate = os.path.join(TESTDATA, name, 'manifest.toml')
            if os.path.isfile(candidate):
                paths.append(candidate)
        for path in paths:
            with open(path, 'rb') as f:
                for entry in tomllib.load(f).get('file', []):
                    entries[entry['id']] = entry
        _ENTRIES = entries
    return _ENTRIES


_VERIFIED = {}


def source_path(file_id):
    """Path of a manifest id: its committed file, else the cached download.
    The sha256 must match the manifest."""
    if file_id in _VERIFIED:
        return _VERIFIED[file_id]
    entry = manifest_entries().get(file_id)
    if entry is None:
        raise SystemExit(f'{file_id}: not a manifest id')
    committed = entry.get('committed')
    if committed:
        path = os.path.join(TESTDATA, committed.removeprefix('testdata/'))
    else:
        path = os.path.join(cache_dir(), file_id)
    if not os.path.isfile(path):
        raise SystemExit(f'{file_id}: {path} does not exist (run `cargo test -p '
                         'recast-radar-io-nexrad` or the golden_script test to download it)')
    with open(path, 'rb') as f:
        digest = hashlib.sha256(f.read()).hexdigest()
    if digest != entry['sha256']:
        raise SystemExit(f'{file_id}: sha256 {digest} does not match the manifest')
    _VERIFIED[file_id] = path
    return path


def verified_bytes(file_id):
    with open(source_path(file_id), 'rb') as f:
        return f.read()


def source_bytes(source):
    """The bytes of a source: one id, or several joined with "+", concatenated."""
    return b''.join(verified_bytes(file_id) for file_id in source.split('+'))


def joined_source_name(source):
    """File name of a source: the id, or <first-id>..<last chunk number>."""
    ids = source.split('+')
    return ids[0] if len(ids) == 1 else ids[0] + '..' + ids[-1].rsplit('-', 2)[-2]


def require_metpy():
    if metpy.__version__ != EXPECTED_METPY:
        raise SystemExit(f'need metpy {EXPECTED_METPY}, found {metpy.__version__}')


def import_pyart():
    os.environ.setdefault('PYART_QUIET', '1')
    import pyart
    if pyart.__version__ != EXPECTED_PYART:
        raise SystemExit(f'need arm_pyart {EXPECTED_PYART}, found {pyart.__version__}')
    return pyart


def jsonable(value):
    """Convert MetPy values to JSON: NaN/inf as strings, bytes as Latin-1."""
    if isinstance(value, float):
        if math.isnan(value):
            return 'NaN'
        if math.isinf(value):
            return 'Infinity' if value > 0 else '-Infinity'
        return value
    if isinstance(value, (bytes, bytearray)):
        return bytes(value).decode('latin-1')
    if isinstance(value, (list, tuple)):
        return [jsonable(item) for item in value]
    return value


# --- group: status ------------------------------------------------------------

STATUS_IDS = [
    'l2-ktlx-19910605-162126',
    'l2-ktlx-19990503-230052',
    'l2-klix-20050829-130035',
    'l2-kvwx-20080415-235337',
    'l2-kpah-20080415-235014',
    'l2-kdmx-20080525-205148',
    'l2-kvnx-20110315-000203',
    'l2-ktlx-20130520-201643',
    'l2-kgwx-20130601-235640',
    'l2-koax-20140616-205305',
    'l2-kewx-20160413-022531',
    'l2-kdvn-20200810-180401',
    'l2-klix-20210829-180425',
    'l2-kbox-20220129-150537',
    'l2-tjua-20220918-190621',
    'l2-kdgx-20230325-010651',
    'l2-kmaf-20230331-230843',
    'l2-tstl-20230331-230314',
    'l2-pgua-20230524-030945',
    'l2-tbwi-20230601-175101-stub',
    'l2-kmtx-20240301-212827',
    'l2-ktlx-20240315-000217',
    'l2-ktlx-20240515-000014',
    'l2-pahg-20250909-212549',
    'l2-kilx-20260418-013553',
    'l2-kiwa-20260917-003629',
    'l2chunk-kiwa-307-20260917-003629-001-s',
]


class StatusFile(Level2File):
    """Level2File that records the first messages 2, 3 and 18 as read."""

    def __init__(self, fobj):
        self.first_status = None
        self.first_maintenance = None
        self.first_adaptation = None
        super().__init__(fobj)

    def _decode_msg1(self, msg_hdr):
        pass

    def _decode_msg31(self, msg_hdr):
        pass

    def _decode_msg2(self, msg_hdr):
        start = self._buffer._offset
        # The 16-byte message header precedes the body; byte 2 is the channel.
        channels = self._buffer._data[start - 16 + 2]
        before = len(self.rda_status)
        super()._decode_msg2(msg_hdr)
        if self.first_status is not None:
            return
        parts = self.rda_status[before:]
        data = bytes(self._buffer._data[start:start + 2 * msg_hdr.size_hw - 16])
        self.first_status = {
            'channels': channels,
            'size_hw': msg_hdr.size_hw,
            'main': (parts[0], raw_fields(Level2File.msg2_fmt, data, 0)),
            'additional': (parts[1], raw_fields(Level2File.msg2_additional_fmt, data,
                                                Level2File.msg2_fmt.size))
            if len(parts) > 1 else None,
        }

    def _decode_msg3(self, msg_hdr):
        had = getattr(self, 'maintenance_data', None)
        super()._decode_msg3(msg_hdr)
        if self.first_maintenance is None and had is None:
            self.first_maintenance = getattr(self, 'maintenance_data', None)

    def _decode_msg18(self, msg_hdr):
        had = getattr(self, 'rda', None)
        super()._decode_msg18(msg_hdr)
        if self.first_adaptation is None and had is None:
            self.first_adaptation = getattr(self, 'rda', None)


def raw_fields(fmt, data, offset):
    """Unpack `fmt` from `data` without MetPy's converters: name -> raw value."""
    names = fmt._tuple._fields
    values = fmt._struct.unpack_from(data, offset)
    return dict(zip(names, values, strict=False))


def layout(fields):
    """(name, format, byte offset) for a MetPy DictStruct field list."""
    out, offset = [], 0
    for name, fmt in fields:
        if name:
            out.append((name, fmt, offset))
        offset += struct.calcsize('>' + fmt)
    return out


def status_golden(file_id):
    with open(source_path(file_id), 'rb') as f:
        level2 = StatusFile(f)
    doc = {'id': file_id, 'generator': 'tools/level2_golden.py status',
           'metpy': metpy.__version__}

    status = level2.first_status
    if status is None:
        doc['message_2'] = None
    else:
        exposed, raw = status['main']
        converted = set(Level2File.msg2_fmt.converters)
        names = Level2File.msg2_fmt._tuple._fields
        message_2 = {
            'channels': status['channels'],
            'size_hw': status['size_hw'],
            'fields': {name: jsonable(value) for name, value in exposed._asdict().items()},
            'codes': {names[index]: raw[names[index]]
                      for index in sorted(converted) if names[index] != 'alarms'},
        }
        if status['additional'] is None:
            message_2['additional'] = None
        else:
            extra, extra_raw = status['additional']
            message_2['additional'] = {
                'fields': {name: jsonable(value) for name, value in extra._asdict().items()},
                'codes': {name: jsonable(value) for name, value in extra_raw.items()},
            }
        doc['message_2'] = message_2

    maintenance = level2.first_maintenance
    if maintenance is None:
        doc['message_3'] = None
    else:
        halfwords = {}
        for name, fmt, offset in layout(metpy_msg3.fields):
            halfwords[str(offset // 2 + 1)] = {
                'name': name, 'format': fmt, 'value': jsonable(maintenance[name])}
        doc['message_3'] = {'halfwords': halfwords}

    adaptation = level2.first_adaptation
    if adaptation is None:
        doc['message_18'] = None
    else:
        offsets = {}
        for name, fmt, offset in layout(metpy_msg18.fields):
            if name.startswith('VCPAT'):
                continue
            offsets[str(offset)] = {
                'name': name, 'format': fmt, 'value': jsonable(adaptation[name])}
        doc['message_18'] = {'bytes': offsets}
    return doc


def status_format(value, indent):
    """Pretty-print nested dicts one key per line; lists and scalars inline."""
    leaf = isinstance(value, dict) and all(
        not isinstance(item, (dict, list)) for item in value.values())
    if isinstance(value, dict) and value and not (leaf and len(value) <= 3):
        pad = ' ' * (indent + 2)
        inner = [f'{pad}{json.dumps(key)}: {status_format(item, indent + 2)}'
                 for key, item in value.items()]
        return '{\n' + ',\n'.join(inner) + '\n' + ' ' * indent + '}'
    return json.dumps(value, allow_nan=False)


def status_documents(ids):
    require_metpy()
    for file_id in ids:
        yield file_id, status_format(status_golden(file_id), 0) + '\n'


# --- group: vcp ---------------------------------------------------------------

VCP_IDS = [
    'l2-klix-20050829-130035',
    'l2-kpah-20080415-235014',
    'l2-kdmx-20080525-205148',
    'l2-kvnx-20110315-000203',
    'l2-ktlx-20130520-201643',
    'l2-kgwx-20130601-235640',
    'l2-koax-20140616-205305',
    'l2-kewx-20160413-022531',
    'l2-kdvn-20200810-180401',
    'l2-klix-20210829-180425',
    'l2-kbox-20220129-150537',
    'l2-tjua-20220918-190621',
    'l2-kdgx-20230325-010651',
    'l2-kmaf-20230331-230843',
    'l2-tstl-20230331-230314',
    'l2-pgua-20230524-030945',
    'l2-kmtx-20240301-212827',
    'l2-ktlx-20240315-000217',
    'l2-ktlx-20240515-000014',
    'l2-pahg-20250909-212549',
    'l2-kilx-20260418-013553',
    'l2-kiwa-20260917-003629',
    'l2chunk-kiwa-307-20260917-003629-001-s',
]

# Volumes whose metadata record holds Message 32; the Message 31 sweeps are
# recorded for the PRF cross-check.
PRF_IDS = {
    'l2-pahg-20250909-212549',
    'l2-kilx-20260418-013553',
    'l2-kiwa-20260917-003629',
}


def open_level2(file_id):
    # A file object lets MetPy detect whole-file gzip/bzip2 by magic bytes;
    # cache files have no extension.
    with open(source_path(file_id), 'rb') as f:
        return Level2File(f)


def field_converter(fmt, name):
    """MetPy's converter for a NamedStruct field (keyed by field index)."""
    return fmt.converters[fmt._tuple._fields.index(name)]


def enum_code(converter, value):
    inverse = {name: code for code, name in converter.val_map.items()}
    return inverse[value]


def bitfield_code(converter, value):
    if value is None:
        return 0
    names = value if isinstance(value, list) else [value]
    code = 0
    for name in names:
        code |= 1 << converter._names.index(name)
    return code


def vcp_golden(file_id):
    f = open_level2(file_id)
    doc = {'id': file_id, 'generator': 'tools/level2_golden.py vcp',
           'metpy': metpy.__version__}
    info = getattr(f, 'vcp_info', None)
    if info is None:
        doc['message_5'] = None
    else:
        header_fmt, cut_fmt = Level2File.vcp_fmt, Level2File.vcp_el_fmt
        header = {
            'size_hw': info.size_hw,
            'pattern_type': info.pattern_type,
            'num': info.num,
            'num_el_cuts': info.num_el_cuts,
            'vcp_version': info.vcp_version,
            'clutter_map_group': info.clutter_map_group,
            'dop_res_code': bitfield_code(field_converter(header_fmt, 'dop_res'),
                                          info.dop_res),
            'pulse_width_code': bitfield_code(field_converter(header_fmt, 'pulse_width'),
                                              info.pulse_width),
            'vcp_sequencing': info.vcp_sequencing,
            'vcp_supplemental_info': info.vcp_supplemental_info,
        }
        cuts = []
        for el in info.els:
            cut = el._asdict()
            cut['channel_config'] = enum_code(
                field_converter(cut_fmt, 'channel_config'), el.channel_config)
            cut['waveform'] = enum_code(field_converter(cut_fmt, 'waveform'), el.waveform)
            cut['super_res'] = bitfield_code(field_converter(cut_fmt, 'super_res'),
                                             el.super_res)
            cuts.append(cut)
        header['els'] = cuts
        doc['message_5'] = header
    if file_id in PRF_IDS:
        sweeps = []
        for sweep in f.sweeps:
            el_nums = sorted({radial[0].el_num for radial in sweep})
            rads = [radial[3] for radial in sweep]
            sweeps.append({
                'el_num': el_nums[0] if len(el_nums) == 1 else el_nums,
                'radials': len(sweep),
                'unamb_range_km': sorted({rad.unamb_range for rad in rads}),
                'nyquist_mps': sorted({round(rad.nyq_vel, 2) for rad in rads}),
            })
        doc['message_31_sweeps'] = sweeps
    return doc


def vcp_format(value, indent):
    """One line per list element of dicts, for readable diffs."""
    pad = ' ' * indent
    if isinstance(value, dict) and any(isinstance(v, list) for v in value.values()):
        items = list(value.items())
        inner = []
        for index, (key, item) in enumerate(items):
            comma = ',' if index + 1 < len(items) else ''
            inner.append(f'{pad}  {json.dumps(key)}: {vcp_format(item, indent + 2)}{comma}')
        return '{\n' + '\n'.join(inner) + f'\n{pad}}}'
    if isinstance(value, list) and value and isinstance(value[0], dict):
        inner = [f'{pad}  {json.dumps(item)}' for item in value]
        return '[\n' + ',\n'.join(inner) + f'\n{pad}]'
    return json.dumps(value)


def vcp_documents(ids):
    require_metpy()
    for file_id in ids:
        doc = vcp_golden(file_id)
        lines = ['{']
        items = list(doc.items())
        for index, (key, value) in enumerate(items):
            comma = ',' if index + 1 < len(items) else ''
            lines.append(f'  {json.dumps(key)}: {vcp_format(value, 2)}{comma}')
        lines.append('}')
        yield file_id, '\n'.join(lines) + '\n'


# --- group: clutter -----------------------------------------------------------

LEVEL2_MANIFEST = os.path.join(TESTDATA, 'level2', 'manifest.toml')


class MetadataOnly(Level2File):
    """Level2File that skips radial messages (1 and 31)."""

    def _decode_msg1(self, msg_hdr):
        pass

    def _decode_msg31(self, msg_hdr):
        pass


class Capture(logging.Handler):
    """Collects MetPy's log messages while a file is read."""

    def __init__(self):
        super().__init__(logging.DEBUG)
        self.messages = []

    def emit(self, record):
        try:
            message = record.getMessage()
        except TypeError:
            # MetPy 1.7.1 logs one message-3 note with a missing argument.
            message = str(record.msg)
        self.messages.append(message)


def clutter_default_ids():
    """Every nexrad-level2 id of testdata/level2/manifest.toml that is not
    derived from another entry, in manifest order."""
    with open(LEVEL2_MANIFEST, 'rb') as f:
        entries = tomllib.load(f)['file']
    return [entry['id'] for entry in entries
            if entry['format'] == 'nexrad-level2' and not entry.get('derived_from')]


def read_metadata(raw):
    """Run MetPy on the volume header plus metadata record; return (file, log)."""
    if raw[:2] == b'\x1f\x8b':
        unwrapped = gzip.decompress(raw)
    elif raw[:3] == b'BZh':
        unwrapped = bz2.decompress(raw)
    else:
        unwrapped = raw
    header = unwrapped[:scan.volume_header_len(unwrapped)]
    _whole, metadata = scan.records(raw)
    capture = Capture()
    logger = logging.getLogger('metpy.io.nexrad')
    logger.addHandler(capture)
    logger.setLevel(logging.DEBUG)
    try:
        level2 = MetadataOnly(io.BytesIO(header + metadata), has_volume_header=bool(header))
    finally:
        logger.removeHandler(capture)
    return level2, capture.messages


def utc(dt):
    return None if dt is None else dt.strftime('%Y-%m-%dT%H:%M:%SZ')


def log_lines(log, message_types):
    """MetPy log lines that name one of `message_types`."""
    pattern = re.compile(r'[Mm]essage (?:type(?:\(s\))?:? )?(?:%s)\b' % '|'.join(
        str(t) for t in message_types))
    return [line for line in log if pattern.search(line)
            and not line.startswith('Unknown message')
            and not line.startswith('Got message')
            and not line.startswith('Total message size')]


def clutter(level2, log):
    """Messages 15 and 13.

    clutter_filter_map: generation time; number of elevation segments; for each
    segment, runs of consecutive azimuth segments with identical range zones,
    written as [first_azimuth, last_azimuth, [[op_code, end_range_km], ...]].
    That is Table XIV order (R1 op code, R2 end range). MetPy stores
    (end, code) tuples.

    clutter_filter_bypass_map: generation time (null for the legacy layout);
    number of elevation segments; radials per segment; bins per radial; and
    the 32 halfwords of radial 0 of each segment. MetPy 1.7.1 has two
    differences from Table IX:
    (1) Every radial of a segment is read from the halfwords of the segment's
        first radial, because the read offset only moves per segment. This
        script asserts that and keeps only radial 0.
    (2) MetPy lists the bits of each halfword least significant bit first, but
        note 4 says the MSB is the lowest-numbered bin. The halfwords are
        rebuilt from MetPy's bit lists in MetPy's order (bit i is list index i),
        so they equal the bytes in the file.
    """
    out = {}
    cfm = getattr(level2, 'clutter_filter_map', None)
    if cfm is not None:
        segments = []
        for segment in cfm['data']:
            runs = []
            for azimuth, zones in enumerate(segment):
                pairs = [[code, end] for end, code in zones]
                if runs and runs[-1][2] == pairs:
                    runs[-1][1] = azimuth
                else:
                    runs.append([azimuth, azimuth, pairs])
            segments.append(runs)
        out['clutter_filter_map'] = {
            'generation_time': utc(cfm['datetime']),
            'elevation_segments': len(cfm['data']),
            'azimuths_per_segment': [len(segment) for segment in cfm['data']],
            'azimuth_runs': segments,
        }
    bypass = getattr(level2, 'clutter_filter_bypass_map', None)
    if bypass is not None:
        radial0 = []
        for segment in bypass['data']:
            first = segment[0]
            assert all(radial == first for radial in segment), \
                'MetPy now reads each radial separately; compare all radials'
            radial0.append([sum(int(bit) << i for i, bit in enumerate(first[16 * k:16 * k + 16]))
                            for k in range(32)])
        out['clutter_filter_bypass_map'] = {
            'generation_time': utc(bypass['datetime']),
            'elevation_segments': len(bypass['data']),
            'radials_per_segment': [len(segment) for segment in bypass['data']],
            'bins_per_radial': len(bypass['data'][0][0]) if bypass['data'] else 0,
            'radial_0_halfwords': radial0,
        }
    notes = log_lines(log, (13, 15))
    if not out and not notes:
        return None
    out['metpy_log'] = notes
    return out


def clutter_dump(value, indent=0):
    """JSON with one object key per line and arrays kept on one line."""
    if isinstance(value, dict) and value:
        inner = ' ' * (indent + 2)
        items = ',\n'.join(f'{inner}{json.dumps(k)}: {clutter_dump(v, indent + 2)}'
                           for k, v in value.items())
        return '{\n' + items + '\n' + ' ' * indent + '}'
    return json.dumps(value, separators=(',', ':'))


def clutter_documents(ids):
    """(id, text), or (id, None) where MetPy finds nothing: no golden file."""
    require_metpy()
    entries = manifest_entries()
    for id_ in ids:
        level2, log = read_metadata(verified_bytes(id_))
        result = clutter(level2, log)
        if result is None:
            yield id_, None
            continue
        document = {
            'id': id_,
            'sha256': entries[id_]['sha256'],
            'generator': 'tools/level2_golden.py',
            'metpy': metpy.__version__,
            'input': 'volume header + metadata record; messages 1 and 31 skipped',
            **result,
        }
        yield id_, clutter_dump(document) + '\n'


# --- group: msg31 -------------------------------------------------------------

# Message 5 elevation cut SNR threshold fields, listed per cut in this order.
SNR_THRESHOLD_KEYS = ('ref_thresh', 'vel_thresh', 'sw_thresh', 'zdr_thresh', 'phidp_thresh',
                      'rhohv_thresh')

# Up to this many distinct values are listed; beyond it a field is summarized
# by count, min, max and the sum in radial order.
MAX_DISTINCT = 12

MSG31_SOURCES = [
    'l2-kvwx-20080415-235337',      # Build "19.96" status, blank ICAO, no azimuth indexing
    'l2-kpah-20080415-235014',      # Build 10.0, VOL 44, RAD 20, 68-byte header
    'l2-kdmx-20080525-205148',      # Build 10.0 super resolution, radial length 1 short
    'l2-kvnx-20110315-000203',      # Build 12.0 first dual-pol
    'l2-kgwx-20130601-235640',      # Build 13.1, recombined moments
    'l2-koax-20140616-205305',      # Build 14.0: VOL version 2, RAD 28
    'l2-kdvn-20200810-180401',      # Build 18.2: last 68-byte header
    'l2-klix-20210829-180425',      # Build 19.1: 72-byte header, CFP, VOL 44
    'l2-kbox-20220129-150537',      # Build 20.1: VOL 52 with ZDR bias estimate
    'l2-kmaf-20230331-230843',      # Build 21.0: ZDR bias estimate not available
    'l2-tstl-20230331-230314',      # TDWR
    'l2-ktlx-20240315-000217',      # Build 22.0 benchmark volume
    'l2-kiwa-20260917-003629',      # Build 24.1
    # Committed real-time chunks: start chunk plus the first two intermediate
    # chunks of the same volume, so the offline test has a golden.
    'l2chunk-kiwa-307-20260917-003629-001-s'
    '+l2chunk-kiwa-307-20260917-003629-002-i'
    '+l2chunk-kiwa-307-20260917-003629-003-i',
]


def normalize(value):
    """MetPy converter output as a JSON number or string."""
    if value is None:
        return ''
    if isinstance(value, bytes):
        return value.decode('latin-1')
    if isinstance(value, (list, tuple)):
        return '|'.join(str(item) for item in value)
    if isinstance(value, bool):
        return int(value)
    if isinstance(value, (int, float, str)):
        return value
    return float(value)


def summarize(values):
    distinct = sorted(set(values), key=lambda v: (isinstance(v, str), v))
    if len(distinct) <= MAX_DISTINCT:
        return {'count': len(values), 'distinct': distinct}
    if any(isinstance(v, str) for v in values):
        raise ValueError(f'too many distinct strings: {distinct[:MAX_DISTINCT]}')
    total = 0.0
    for v in values:
        total += v
    return {'count': len(values), 'min': min(values), 'max': max(values), 'sum': total}


def msg31_fields(radial):
    """Flatten one MetPy message 31 radial."""
    fields = {}
    for key, value in radial.header._asdict().items():
        fields[f'header.{key}'] = normalize(value)
    for prefix, block in (('vol', radial.vol_consts), ('elv', radial.elev_consts),
                          ('rad', radial.radial_consts)):
        if block is None:
            continue
        for key, value in block._asdict().items():
            if key in ('type', 'name'):
                continue
            fields[f'{prefix}.{key}'] = normalize(value)
    for name, (hdr, _values) in radial.moments.items():
        moment = name.decode('latin-1')
        for key, value in hdr._asdict().items():
            if key in ('type', 'name'):
                continue
            fields[f'moment.{moment}.{key}'] = normalize(value)
    return fields


def msg31_golden(source):
    f = Level2File(io.BytesIO(source_bytes(source)))
    sweeps = []
    for index, sweep in enumerate(f.sweeps):
        radials = [msg31_fields(radial) for radial in sweep]
        per_field = {}
        for fields in radials:
            for key, value in fields.items():
                per_field.setdefault(key, []).append(value)
        sweeps.append({
            'index': index,
            'radials': len(radials),
            'first': radials[0] if radials else {},
            'fields': {key: summarize(values) for key, values in sorted(per_field.items())},
        })
    rda_build = None
    for status in getattr(f, 'rda_status', []):
        if 'rda_build' in status._fields:
            rda_build = status.rda_build
            break
    thresholds = []
    vcp = getattr(f, 'vcp_info', None)
    if vcp is not None:
        thresholds = [[getattr(cut, key) for key in SNR_THRESHOLD_KEYS] for cut in vcp.els]
    for sweep in sweeps:
        for summary in sweep['fields'].values():
            for key in ('min', 'max', 'sum'):
                if key in summary and not math.isfinite(summary[key]):
                    raise ValueError(f'{source}: non-finite {key}')
    return {
        'source': source.split('+'),
        'generator': f'tools/level2_golden.py msg31 (MetPy {metpy.__version__})',
        'rda_build': rda_build,
        'vcp_pattern': vcp.num if vcp is not None else None,
        'vcp_snr_threshold_keys': list(SNR_THRESHOLD_KEYS),
        'vcp_snr_thresholds_db': thresholds,
        'sweeps': sweeps,
    }


def msg31_to_json(value, depth=0):
    """JSON with one line per member of the top-level object, of each sweep,
    and of each sweep's `first` and `fields` objects; anything deeper, and
    lists of scalars, stay on their member's line."""
    pad = ' ' * (depth + 1)
    if isinstance(value, dict) and value and depth < 4:
        members = [f'{pad}{json.dumps(key)}: {msg31_to_json(item, depth + 1)}'
                   for key, item in value.items()]
        return '{\n' + ',\n'.join(members) + '\n' + ' ' * depth + '}'
    if isinstance(value, list) and value and depth < 2 and isinstance(value[0], (dict, list)):
        items = [pad + msg31_to_json(item, depth + 1) for item in value]
        return '[\n' + ',\n'.join(items) + '\n' + ' ' * depth + ']'
    return json.dumps(value, separators=(', ', ': '))


def msg31_documents(sources):
    require_metpy()
    for source in sources:
        yield joined_source_name(source), msg31_to_json(msg31_golden(source)) + '\n'


# --- group: metadata ----------------------------------------------------------

METADATA_SOURCES = [
    'l2-ktlx-19910605-162126',
    'l2-ktlx-19990504-002218',
    'l2-ktlx-20030508-221041',
    'l2-klix-20050829-130035',
    'l2-kvwx-20080415-235337',
    'l2-kpah-20080415-235014',
    'l2-kdmx-20080525-205148',
    'l2-kvnx-20110315-000203',
    'l2-ktlx-20130520-201643',
    'l2-kgwx-20130601-235640',
    'l2-koax-20140616-205305',
    'l2-kewx-20160413-022531',
    'l2-kdvn-20200810-180401',
    'l2-klix-20210829-180425',
    'l2-kbox-20220129-150537',
    'l2-tjua-20220918-190621',
    'l2-kdgx-20230325-010651',
    'l2-kmaf-20230331-230843',
    'l2-tstl-20230331-230314',
    'l2-pgua-20230524-030945',
    'l2-kmtx-20240301-212827',
    'l2-ktlx-20240315-000217',
    'l2-ktlx-20240515-000014',
    'l2-pahg-20250909-212549',
    'l2-kilx-20260418-013553',
    'l2-kiwa-20260917-003629',
    'l2chunk-kiwa-307-20260917-003629-001-s'
    '+l2chunk-kiwa-307-20260917-003629-002-i'
    '+l2chunk-kiwa-307-20260917-003629-003-i',
]

# Data Header Block fields written per scan (Py-ART MSG_31 names).
PYART_HEADER_KEYS = ('collect_ms', 'collect_date', 'azimuth_number', 'azimuth_angle',
                     'radial_length', 'azimuth_resolution', 'radial_spacing',
                     'elevation_number', 'cut_sector', 'elevation_angle', 'block_count')


def pyart_block(block):
    """A Py-ART VOL/ELV/RAD dict without type and name; spare bytes as an int."""
    out = {}
    for key, value in block.items():
        if key in ('block_type', 'data_name'):
            continue
        if isinstance(value, bytes):
            value = int.from_bytes(value, 'big')
        out[key] = value
    return out


def pyart_level2_file(source):
    """Py-ART's NEXRADLevel2File on a source, decompressed as
    read_nexrad_archive's prepare_for_read does (whole-file gzip or bzip2)."""
    import warnings

    from pyart.io.nexrad_level2 import NEXRADLevel2File

    data = source_bytes(source)
    if data[:2] == b'\x1f\x8b':
        data = gzip.decompress(data)
    elif data[:3] == b'BZh':
        data = bz2.decompress(data)
    with warnings.catch_warnings():
        warnings.simplefilter('ignore')
        return NEXRADLevel2File(io.BytesIO(data))


def metadata_golden(source, pyart):
    f = pyart_level2_file(source)
    vcp = f.vcp
    cuts = vcp['cut_parameters'] if vcp is not None else []
    scans = []
    for index, messages in enumerate(f.scan_msgs):
        scan = {'elevation_number': index + 1, 'nrays': len(messages),
                'target_angle_code': cuts[index]['elevation_angle'] if index < len(cuts) else None}
        if len(messages):
            ray = f.radial_records[messages[0]]
            if f._msg_type == '31':
                scan['header'] = {key: ray['msg_header'][key] for key in PYART_HEADER_KEYS}
                for name in ('VOL', 'ELV', 'RAD'):
                    scan[name] = pyart_block(ray[name]) if name in ray else None
        scans.append(scan)
    return {
        'source': source.split('+'),
        'generator': 'tools/level2_golden.py metadata',
        'pyart': pyart.__version__,
        'msg_type': int(f._msg_type),
        'vcp_pattern': f.get_vcp_pattern(),
        'scans': scans,
    }


def metadata_documents(sources):
    pyart = import_pyart()
    for source in sources:
        yield joined_source_name(source), scans_document_lines(metadata_golden(source, pyart))


def scans_document_lines(doc):
    """One line per top-level member, and one per element of `scans`."""
    lines = ['{']
    items = list(doc.items())
    for index, (key, value) in enumerate(items):
        comma = ',' if index + 1 < len(items) else ''
        if key == 'scans':
            inner = ',\n'.join(f'    {json.dumps(scan)}' for scan in value)
            lines.append(f'  "scans": [\n{inner}\n  ]{comma}')
        else:
            lines.append(f'  {json.dumps(key)}: {json.dumps(value)}{comma}')
    lines.append('}')
    return '\n'.join(lines) + '\n'


# --- group: volume ------------------------------------------------------------

VOLUME_SOURCES = [
    'l2-kvwx-20080415-235337',      # four-space radar identifier in every message 31
    'l2-kpah-20080415-235014',      # Build 10.0, the same evening, identifier "KPAH"
    'l2-ktlx-20240315-000217',      # Build 22.0 benchmark volume: 16-bit ZDR/PHI, CFP
    'l2chunk-kiwa-307-20260917-003629-001-s'
    '+l2chunk-kiwa-307-20260917-003629-002-i'
    '+l2chunk-kiwa-307-20260917-003629-003-i',
]

PYART_MOMENTS = ('REF', 'VEL', 'SW', 'ZDR', 'PHI', 'RHO', 'CFP')


def volume_golden(source, pyart):
    """The rays and moment data Py-ART's NEXRADLevel2File reads, per scan."""
    import numpy as np

    f = pyart_level2_file(source)
    if f._msg_type != '31':
        raise SystemExit(f'{source}: the volume group covers message 31 files')
    scans = []
    for index, messages in enumerate(f.scan_msgs):
        rays = [f.radial_records[m] for m in messages]
        scan = {
            'elevation_number': index + 1,
            'nrays': len(rays),
            'collect_ms_sum': sum(int(ray['msg_header']['collect_ms']) for ray in rays),
            'azimuth_sum': math.fsum(ray['msg_header']['azimuth_angle'] for ray in rays),
            'moments': {},
        }
        for name in PYART_MOMENTS:
            blocks = [ray[name] for ray in rays if name in ray]
            if not blocks:
                continue
            codes = [np.asarray(block['data'][:block['ngates']], dtype=np.int64)
                     for block in blocks]
            scan['moments'][name] = {
                'rays': len(blocks),
                'ngates': sorted({int(block['ngates']) for block in blocks}),
                'gates': sum(int(block['ngates']) for block in blocks),
                'first_gate': sorted({int(block['first_gate']) for block in blocks}),
                'gate_spacing': sorted({int(block['gate_spacing']) for block in blocks}),
                'word_size': sorted({int(block['word_size']) for block in blocks}),
                'scale': sorted({float(block['scale']) for block in blocks}),
                'offset': sorted({float(block['offset']) for block in blocks}),
                'code_sum': sum(int(c.sum()) for c in codes),
                'code_0': sum(int((c == 0).sum()) for c in codes),
                'code_1': sum(int((c == 1).sum()) for c in codes),
            }
        scans.append(scan)
    return {
        'source': source.split('+'),
        'generator': 'tools/level2_golden.py volume',
        'pyart': pyart.__version__,
        'icao': f.volume_header['icao'].decode('latin-1'),
        'nrays': len(f.radial_records),
        'scans': scans,
    }


def volume_documents(sources):
    pyart = import_pyart()
    for source in sources:
        yield joined_source_name(source), scans_document_lines(volume_golden(source, pyart))


# --- dispatch -----------------------------------------------------------------

# group -> (documents(sources), default sources)
GROUPS = {
    'status': (status_documents, lambda: STATUS_IDS),
    'vcp': (vcp_documents, lambda: VCP_IDS),
    'clutter': (clutter_documents, clutter_default_ids),
    'msg31': (msg31_documents, lambda: MSG31_SOURCES),
    'metadata': (metadata_documents, lambda: METADATA_SOURCES),
    'volume': (volume_documents, lambda: VOLUME_SOURCES),
}


def golden_file(group, name):
    return os.path.join(GOLDEN_DIR, group, f'{name}.json')


def write_group(group, sources):
    documents, defaults = GROUPS[group]
    for name, text in documents(sources or defaults()):
        path = golden_file(group, name)
        if text is None:
            if os.path.exists(path):
                os.remove(path)
                print(f'{group}: {name}: removed', flush=True)
            continue
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, 'w', encoding='utf-8', newline='\n') as out:
            out.write(text)
        print(f'{group}: {name}', flush=True)


def first_difference(committed, generated):
    """Line number and both versions of the first differing line."""
    old = committed.split(b'\n')
    new = generated.split(b'\n')
    for number, (a, b) in enumerate(zip(old, new), start=1):
        if a != b:
            return (f'line {number}:\n      committed: {a[:160]!r}\n'
                    f'      generated: {b[:160]!r}')
    return f'lengths differ: committed {len(committed)} bytes, generated {len(generated)} bytes'


def check_group(group, sources):
    """Problems found comparing generated documents with the committed files."""
    documents, defaults = GROUPS[group]
    problems = []
    produced = set()
    for name, text in documents(sources or defaults()):
        produced.add(name)
        path = golden_file(group, name)
        label = f'{group}/{name}.json'
        if text is None:
            if os.path.exists(path):
                problems.append(f'{label}: committed, but the script writes no file for it')
            else:
                print(f'ok {group}: {name} (no file)', flush=True)
            continue
        if not os.path.exists(path):
            problems.append(f'{label}: generated but not committed')
            continue
        with open(path, 'rb') as f:
            committed = f.read()
        generated = text.encode('utf-8')
        if committed == generated:
            print(f'ok {label}', flush=True)
        else:
            problems.append(f'{label}: differs at {first_difference(committed, generated)}')
    if not sources:
        directory = os.path.join(GOLDEN_DIR, group)
        committed_names = sorted(name[:-5] for name in os.listdir(directory)
                                 if name.endswith('.json')) if os.path.isdir(directory) else []
        for name in committed_names:
            if name not in produced:
                problems.append(f'{group}/{name}.json: committed, but not produced by the '
                                'default sources')
    return problems


def list_sources(groups):
    seen = []
    for group in groups:
        for source in GROUPS[group][1]():
            for file_id in source.split('+'):
                if file_id not in seen:
                    seen.append(file_id)
    return seen


def main(argv):
    parser = argparse.ArgumentParser(
        description='Write or check the Level II golden files (see the module docstring).')
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--check', action='store_true',
                      help='compare with the committed files instead of writing them')
    mode.add_argument('--list-sources', action='store_true',
                      help="print the manifest ids the groups' default sources read")
    parser.add_argument('group', choices=[*GROUPS, 'all'])
    parser.add_argument('sources', nargs='*')
    args = parser.parse_args(argv)
    groups = list(GROUPS) if args.group == 'all' else [args.group]
    if args.group == 'all' and args.sources:
        parser.error('`all` takes no sources')

    if args.list_sources:
        if args.sources:
            parser.error('--list-sources takes no sources')
        for file_id in list_sources(groups):
            print(file_id)
        return
    if args.check:
        # MetPy's warnings about the files are expected; keep the report short.
        logging.getLogger('metpy').addHandler(logging.NullHandler())
        problems = []
        for group in groups:
            problems.extend(check_group(group, args.sources))
        if problems:
            print(f'{len(problems)} golden file problem(s):', file=sys.stderr)
            for problem in problems:
                print(f'  {problem}', file=sys.stderr)
            sys.exit(1)
        print(f'every golden file of {", ".join(groups)} matches', flush=True)
        return
    for group in groups:
        write_group(group, args.sources)


if __name__ == '__main__':
    main(sys.argv[1:])
