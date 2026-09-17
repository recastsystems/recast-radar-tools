"""Golden values for Level II metadata messages, taken from MetPy's Level2File.

Used by the tests in crates/recast-radar-io-nexrad/tests/messages_*.rs.

For each manifest id, MetPy's `Level2File` reads the Archive II metadata
record: the same bytes `messages::metadata_record` returns in Rust (the first
LDM record, or else the first 134 frames). The file's 24-byte volume header
goes in front of it. MetPy skips radial messages 1 and 31, because MetPy 1.7.1
raises an error on the Message 1 frames in the KLIX 2005 metadata record.
Everything else is MetPy's own decoding. Where MetPy's output is known to
differ from the ICD, the group function says so and records only the values
that can be compared.

The groups are independent. Each group writes testdata/level2/golden/<group>/<id>.json
for the ids where MetPy decoded something for that group:

- clutter: Message 15 (Level2File.clutter_filter_map) and Message 13
  (Level2File.clutter_filter_bypass_map).

Usage: python tools/level2_golden.py [--group NAME ...] [ID ...]
The defaults are every group and every id with format "nexrad-level2" in
testdata/level2/manifest.toml. Downloaded files are read from the shared
testdata cache ($RECAST_RADAR_TESTDATA or
%LOCALAPPDATA%/recast-radar-tools/testdata). Run the Rust tests once to fill
the cache. Needs metpy==1.7.1.
"""

import argparse
import bz2
import gzip
import hashlib
import io
import json
import logging
import os
import re
import sys
import tomllib

import metpy
from metpy.io import Level2File

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, 'tools'))
sys.dont_write_bytecode = True  # keep tools/ free of __pycache__
import level2_message_scan as scan  # noqa: E402

GOLDEN_DIR = os.path.join(ROOT, 'testdata', 'level2', 'golden')
MANIFEST = os.path.join(ROOT, 'testdata', 'level2', 'manifest.toml')
EXPECTED_METPY = '1.7.1'

GROUPS = {}


def group(name):
    """Register a golden group: fn(level2file, log_messages) -> dict or None."""
    def register(fn):
        GROUPS[name] = fn
        return fn
    return register


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


def manifest_entries():
    with open(MANIFEST, 'rb') as f:
        return {entry['id']: entry for entry in tomllib.load(f)['file']}


def file_bytes(entry):
    if entry.get('committed'):
        path = os.path.join(ROOT, 'testdata', entry['committed'].removeprefix('testdata/'))
    else:
        path = scan.cache_path(entry['id'])
    with open(path, 'rb') as f:
        raw = f.read()
    digest = hashlib.sha256(raw).hexdigest()
    if digest != entry['sha256']:
        raise SystemExit(f"{entry['id']}: sha256 {digest} does not match the manifest")
    return raw


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


@group('clutter')
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


def dump(value, indent=0):
    """JSON with one object key per line and arrays kept on one line."""
    if isinstance(value, dict) and value:
        inner = ' ' * (indent + 2)
        items = ',\n'.join(f'{inner}{json.dumps(k)}: {dump(v, indent + 2)}'
                           for k, v in value.items())
        return '{\n' + items + '\n' + ' ' * indent + '}'
    return json.dumps(value, separators=(',', ':'))


def main():
    parser = argparse.ArgumentParser(description=__doc__.split('\n\n')[0])
    parser.add_argument('--group', action='append', choices=sorted(GROUPS))
    parser.add_argument('ids', nargs='*')
    args = parser.parse_args()
    if metpy.__version__ != EXPECTED_METPY:
        raise SystemExit(f'need metpy {EXPECTED_METPY}, found {metpy.__version__}')
    entries = manifest_entries()
    ids = args.ids or [i for i, e in entries.items() if e['format'] == 'nexrad-level2']
    groups = args.group or sorted(GROUPS)
    for id_ in ids:
        raw = file_bytes(entries[id_])
        level2, log = read_metadata(raw)
        for name in groups:
            result = GROUPS[name](level2, log)
            path = os.path.join(GOLDEN_DIR, name, f'{id_}.json')
            if result is None:
                if os.path.exists(path):
                    os.remove(path)
                continue
            document = {
                'id': id_,
                'sha256': entries[id_]['sha256'],
                'generator': 'tools/level2_golden.py',
                'metpy': metpy.__version__,
                'input': 'volume header + metadata record; messages 1 and 31 skipped',
                **result,
            }
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, 'w', newline='\n') as f:
                f.write(dump(document) + '\n')
            print(f'{path}: {", ".join(k for k in result)}')


if __name__ == '__main__':
    main()
