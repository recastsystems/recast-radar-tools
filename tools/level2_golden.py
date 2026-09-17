"""Golden values for the Level II message decoders, read with MetPy.

Writes JSON files under testdata/level2/golden/<group>/, one per source. The
Rust tests in crates/recast-radar-io-nexrad/tests/ compare their decoded values
against these files.

Needs MetPy (1.7.1 was used for the committed files). Test files are read from
the shared download cache that recast-radar-testdata fills
(%LOCALAPPDATA%\\recast-radar-tools\\testdata, or $RECAST_RADAR_TESTDATA).
Run `cargo test -p recast-radar-io-nexrad` once to download them.

Usage:
    python tools/level2_golden.py <group> [manifest-id ...]
    python tools/level2_golden.py all

With no ids, the group's default id list is regenerated; `all` regenerates
every group with its defaults.

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


def golden_path(group, name):
    directory = os.path.join(GOLDEN_DIR, group)
    os.makedirs(directory, exist_ok=True)
    return os.path.join(directory, f'{name}.json')


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


def run_status(ids):
    for file_id in ids or STATUS_IDS:
        with open(golden_path('status', file_id), 'w', newline='\n') as f:
            f.write(status_format(status_golden(file_id), 0) + '\n')
        print(f'status: {file_id}')


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
    with open(cache_path(file_id), 'rb') as f:
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


def run_vcp(ids):
    for file_id in ids or VCP_IDS:
        doc = vcp_golden(file_id)
        lines = ['{']
        items = list(doc.items())
        for index, (key, value) in enumerate(items):
            comma = ',' if index + 1 < len(items) else ''
            lines.append(f'  {json.dumps(key)}: {vcp_format(value, 2)}{comma}')
        lines.append('}')
        with open(golden_path('vcp', file_id), 'w', newline='\n') as f:
            f.write('\n'.join(lines) + '\n')
        print(f'vcp: {file_id}')


# --- dispatch -----------------------------------------------------------------

GROUPS = {
    'status': run_status,
    'vcp': run_vcp,
}


def main(argv):
    if not argv or (argv[0] not in GROUPS and argv[0] != 'all'):
        sys.exit(f'usage: level2_golden.py <{"|".join(GROUPS)}|all> [manifest-id ...]')
    if argv[0] == 'all':
        if argv[1:]:
            sys.exit('level2_golden.py all takes no ids')
        for run in GROUPS.values():
            run([])
    else:
        GROUPS[argv[0]](argv[1:])


if __name__ == '__main__':
    main(sys.argv[1:])
