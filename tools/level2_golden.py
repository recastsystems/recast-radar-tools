"""Golden values from MetPy's Level2File for the recast-radar-io-nexrad tests.

Each section writes one JSON file per source under
testdata/level2/golden/<section>/<name>.json, read by the Rust tests named in
the section's docstring. The JSON records what MetPy exposes, normalized only
where MetPy's converters return Python-only types (bytes become text,
BitField lists become "A|B" strings, None becomes "").

Usage:
    python tools/level2_golden.py <section> [source ...]

A source is a manifest id, or several ids joined with "+" that are read as one
concatenated file (a real-time volume is its chunks concatenated). With no
sources, the section's default list is used.

Sections:
    msg31   message 31 Data Header Block, VOL/ELV/RAD constant blocks and data
            moment block descriptors per sweep, the RDA build from message 2,
            and the message 5 SNR thresholds per elevation cut
            (crates/recast-radar-io-nexrad/tests/messages_msg31.rs).

Requires metpy (tested with 1.7.1). Files come from the recast-radar-testdata
cache (RECAST_RADAR_TESTDATA, else %LOCALAPPDATA%/recast-radar-tools/testdata)
or the committed path in the manifest; run the Rust tests once to download.
"""

import io
import json
import math
import os
import sys
import tomllib

import metpy
from metpy.io import Level2File

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TESTDATA = os.path.join(ROOT, 'testdata')
GOLDEN = os.path.join(TESTDATA, 'level2', 'golden')

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


def manifest_entries():
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
    return entries


def source_bytes(source, entries):
    data = b''
    for file_id in source.split('+'):
        entry = entries[file_id]
        committed = entry.get('committed')
        if committed:
            committed = committed.removeprefix('testdata/')
            path = os.path.join(TESTDATA, committed)
        else:
            cache = os.environ.get('RECAST_RADAR_TESTDATA') or os.path.join(
                os.environ['LOCALAPPDATA'], 'recast-radar-tools', 'testdata')
            path = os.path.join(cache, file_id)
        with open(path, 'rb') as f:
            data += f.read()
    return data


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


def msg31_golden(source, entries):
    f = Level2File(io.BytesIO(source_bytes(source, entries)))
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


def to_json(value, depth=0):
    """JSON with one line per member of the top-level object, of each sweep,
    and of each sweep's `first` and `fields` objects; anything deeper, and
    lists of scalars, stay on their member's line."""
    pad = ' ' * (depth + 1)
    if isinstance(value, dict) and value and depth < 4:
        members = [f'{pad}{json.dumps(key)}: {to_json(item, depth + 1)}'
                   for key, item in value.items()]
        return '{\n' + ',\n'.join(members) + '\n' + ' ' * depth + '}'
    if isinstance(value, list) and value and depth < 2 and isinstance(value[0], (dict, list)):
        items = [pad + to_json(item, depth + 1) for item in value]
        return '[\n' + ',\n'.join(items) + '\n' + ' ' * depth + ']'
    return json.dumps(value, separators=(', ', ': '))


SECTIONS = {
    'msg31': (msg31_golden, MSG31_SOURCES),
}


def main(argv):
    if not argv or argv[0] not in SECTIONS:
        sys.exit(f'usage: level2_golden.py {{{",".join(SECTIONS)}}} [source ...]')
    section, sources = argv[0], argv[1:]
    generate, defaults = SECTIONS[section]
    entries = manifest_entries()
    out_dir = os.path.join(GOLDEN, section)
    os.makedirs(out_dir, exist_ok=True)
    for source in sources or defaults:
        golden = generate(source, entries)
        name = source.split('+')[0] if '+' not in source else (
            source.split('+')[0] + '..' + source.split('+')[-1].rsplit('-', 2)[-2])
        path = os.path.join(out_dir, f'{name}.json')
        with open(path, 'w', encoding='utf-8', newline='\n') as out:
            out.write(to_json(golden) + '\n')
        print(path, flush=True)


if __name__ == '__main__':
    main(sys.argv[1:])
