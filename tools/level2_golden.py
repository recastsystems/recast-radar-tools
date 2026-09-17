"""Golden values for the Level II message decoders, read with MetPy.

Writes one JSON file per manifest id to testdata/level2/golden/<group>/<id>.json.
The Rust tests in crates/recast-radar-io-nexrad/tests/ compare their decoded
values against these files.

Needs MetPy (1.7.1 was used for the committed files). Test files are read from
the shared download cache that recast-radar-testdata fills
(%LOCALAPPDATA%\\recast-radar-tools\\testdata, or $RECAST_RADAR_TESTDATA).
Run `cargo test -p recast-radar-io-nexrad` once to download them.

Usage:
    python tools/level2_golden.py <group> [manifest-id ...]

With no ids, the group's default id list is regenerated.

Groups
------
status
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
"""

import json
import math
import os
import struct
import sys

import metpy
from metpy.io import Level2File
from metpy.io._nexrad_msgs import msg3 as metpy_msg3
from metpy.io._nexrad_msgs import msg18 as metpy_msg18

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GOLDEN_DIR = os.path.join(ROOT, 'testdata', 'level2', 'golden')


def cache_path(file_id):
    root = os.environ.get('RECAST_RADAR_TESTDATA') or os.path.join(
        os.environ['LOCALAPPDATA'], 'recast-radar-tools', 'testdata')
    return os.path.join(root, file_id)


def write_json(group, file_id, doc):
    """Write `doc` with one line per leaf entry, for readable diffs."""
    directory = os.path.join(GOLDEN_DIR, group)
    os.makedirs(directory, exist_ok=True)
    text = format_value(doc, 0)
    with open(os.path.join(directory, f'{file_id}.json'), 'w', newline='\n') as f:
        f.write(text + '\n')


def format_value(value, indent):
    """Pretty-print nested dicts one key per line; lists and scalars inline."""
    leaf = all(not isinstance(item, (dict, list)) for item in value.values())         if isinstance(value, dict) else False
    if isinstance(value, dict) and value and not (leaf and len(value) <= 3):
        pad = ' ' * (indent + 2)
        inner = [f'{pad}{json.dumps(key)}: {format_value(item, indent + 2)}'
                 for key, item in value.items()]
        return '{\n' + ',\n'.join(inner) + '\n' + ' ' * indent + '}'
    return json.dumps(value, allow_nan=False)


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
    with open(cache_path(file_id), 'rb') as f:
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


GROUPS = {
    'status': (STATUS_IDS, status_golden),
}


def main(argv):
    if not argv or argv[0] not in GROUPS:
        sys.exit(f'usage: level2_golden.py <{"|".join(GROUPS)}> [manifest-id ...]')
    default_ids, build = GROUPS[argv[0]]
    for file_id in argv[1:] or default_ids:
        write_json(argv[0], file_id, build(file_id))
        print(f'{argv[0]}: {file_id}')


if __name__ == '__main__':
    main(sys.argv[1:])
