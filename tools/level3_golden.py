#!/usr/bin/env python3
"""Generate golden JSON for the NEXRAD/TDWR Level III test corpus.

Usage (from the workspace root)::

    python tools/level3_golden.py [--check] [ID ...]

Reads ``testdata/level3/manifest.toml``, loads each committed file from
``testdata/files/level3/<id>`` (sha256 verified) and writes
``testdata/level3/golden/<id>.json``.  ``--check`` regenerates in memory and
fails if any golden file on disk differs.  Requires Python 3.11+ (``tomllib``),
numpy and MetPy 1.7.1 (golden values are specific to that MetPy version).

Two readers contribute to each golden file (the walker is a second reading by
the Rust decoder's author; MetPy is third-party):

* An ICD walker in this script (RPG to Class 1 User ICD 2620001AD, sections
  3.3.1-3.3.2, Figures 3-3, 3-6, 3-7 to 3-15c, 3-16, Appendix D) that
  recovers framing, the Message Header Block, raw Product Description Block
  halfwords, block/page structure and the packet codes present.  MetPy does not
  expose packet codes, so ``blocks`` and ``packet_codes`` always come from this
  walker.
* ``metpy.io.Level3File`` (MetPy 1.7.1), which supplies the decoded header
  fields, product metadata, raw data levels and ``map_data`` physical values.

Golden JSON schema (all keys always present unless noted):

``id``, ``sha256``, ``size``
    Manifest identity of the file.
``framing``
    ``noaaport_soh`` (bool), ``noaaport_sequence`` (str|null),
    ``wmo_heading`` (str|null), ``awips_id`` (str|null),
    ``trailer`` ("noaaport-etx"|null: 4 bytes ``\\r\\r\\n\\x03`` removed),
    ``zlib_frames`` (int), ``zlib_uncompressed_bytes`` (int),
    ``ccb_bytes`` (NOAAPort CCB length inside the zlib stream, 0 if none),
    ``inner_wmo_heading``/``inner_awips_id`` (heading repeated inside zlib),
    ``text_only`` (true for pure-text products without a message header),
    ``message_offset`` (byte offset of the Message Header Block in the
    unwrapped message, always 0) and ``message_bytes`` (length of the
    unwrapped message).
``message_header``
    Figure 3-3 fields: ``code``, ``date``, ``time``, ``length``,
    ``source_id``, ``destination_id``, ``num_blocks`` (null when text_only).
``halfwords``
    Raw big-endian unsigned halfwords 1..60 of the Message Header Block and
    Product Description Block: ``halfwords[n-1]`` is ICD halfword ``n``
    (null for text-only products and General Status Messages).
``product_code``
    PDB halfword 16 as a signed integer (null when there is no PDB).
``compression``
    ``hw51`` (PDB halfword 51, unsigned), ``bzip2`` (bool: data after the PDB
    started with ``BZh`` and decompressed), ``compressed_bytes``,
    ``uncompressed_bytes`` (bzip2 output length), ``hw52_53_uncompressed_size``.
``blocks``
    Walker result.  ``symbology``: ``{length, num_layers, layers:[{length,
    packets:[code,...]}], nested:[{layer, index, code, packets:[...]}]}`` where
    ``packets`` lists top-level packet codes in file order and ``nested``
    lists the packets inside SCIT packets 23/24.  ``graphic``: ``{length,
    num_pages, pages:[{page, length, packets:[...]}]}``.  ``tabular``:
    ``{length, message_code, product_code, num_pages, lines_per_page:[...],
    text_sha256}``, or for the radar coded message in the block of product 83
    (IRM, packets 30-32, no ICD) ``{length, message_code, product_code,
    rcm_text_bytes, rcm_text_sha256}``.  ``standalone_tabular``: ``{num_pages, lines_per_page,
    text_sha256}``.  ``cell_trend``: ``{offset_halfwords, packets}`` for
    product 62, whose graphic offset points one halfword past the first packet
    code.  ``rcm``: ``{text_bytes, text_sha256}`` for product 74.  Any block
    may be null.  ``unknown``: list of ``{where, code, byte_offset}`` for
    packets the walker could not size.
``packet_codes``
    Sorted unique packet codes found anywhere (symbology, nested, graphic,
    cell trend).
``data``
    One entry per data packet (codes 16, 0xAF1F, 0xBA07, 0xBA0F, 17, 18 and
    generic 28/29 radial components) in file order: ``{layer, index, packet,
    header, rows, cols, dtype, raw_sha256, histogram, physical}``.
    ``header`` holds the walker's packet header fields.  ``rows`` x ``cols``
    is the raw level grid: radials x bins for radial packets (bins =
    "number of range bins" from the packet header, trailing pad bytes
    dropped), rows x columns for raster/precipitation arrays.  When radials
    hold fewer levels than that (observed: product 46 of 1994) they are padded
    with level 0 and ``short_radials`` (count) and ``max_levels`` (longest
    radial) are added.
    ``raw_sha256`` is the SHA-256 of the grid in row-major order encoded as
    ``u8`` bytes, or ``u16`` big-endian for generic packets.  ``histogram``
    maps level (decimal string) to count.  ``physical`` is null for packet 18
    and unless MetPy maps this product with a product-specific mapper; otherwise
    ``{min, max, mean, finite, masked}`` over ``Level3File.map_data`` applied
    to the grid (NaN counts as masked; for product 135 ``topped`` is added).
    ``data`` is null when MetPy could not read the file or the product is
    text only.
``metpy``
    ``"ok"`` (read with no warnings), ``"partial"`` (read, but MetPy logged
    warnings, e.g. default metadata for an unknown product) or
    ``"unsupported"`` (MetPy raised).
``metpy_detail``
    ``version``, ``warnings``, ``error``, ``product_name``, ``max_range``,
    ``mapper``, ``header``/``prod_desc`` (MetPy namedtuples as dicts),
    ``thresholds``, ``dep_vals``, ``metadata`` (datetimes as ISO-8601 UTC),
    ``sym_layer_packet_counts``, ``graph_page_packet_counts``,
    ``tab_pages`` (count), ``crosscheck`` (walker top-level packet counts equal
    MetPy's, or null when not comparable).

MetPy compatibility shim: ``metpy.io.nexrad.nexrad_to_datetime`` uses
``datetime.fromtimestamp``, which raises ``OSError`` on Windows for the
all-zero message dates of archived 1990s products.  The shim computes the same
value with ``datetime(1970, 1, 1) + timedelta`` so those files can be read; it
does not change any value MetPy produces on other platforms.
"""

import argparse
import bz2
import hashlib
import io
import json
import logging
import re
import struct
import sys
import tomllib
import warnings
import zlib
from datetime import datetime, timedelta
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = ROOT / 'testdata' / 'level3' / 'manifest.toml'
GOLDEN_DIR = ROOT / 'testdata' / 'level3' / 'golden'

WMO_RE = re.compile(rb'([A-Z]{4}\d{2}) ([A-Z]{4}) (\d{6})(?: ([A-Z]{3}))?\r\r\n')
AWIPS_RE = re.compile(rb'([A-Z0-9]{3,6}) *\r\r\n')
SEQ_RE = re.compile(rb'(\d{3,5}) ?\r\r\n')
ETX_TRAILER = b'\r\r\n\x03'

# Products whose symbology offset points at a stand-alone tabular page block
# (Figure 3-16) rather than a symbology block, plus the alphanumeric message
# codes 100-111 (section 3.3.1.4) when they are distributed on their own.
STANDALONE_CANDIDATES = {62, 73, 75, 77, 82} | set(range(100, 112))
RADIAL_PACKETS = {16, 0xAF1F}
RASTER_PACKETS = {0xBA07, 0xBA0F}
LENGTH_PREFIXED = {1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 19, 20, 21, 22, 25, 26}


# --------------------------------------------------------------------------------------
# Framing
# --------------------------------------------------------------------------------------

def _strip_headings(buf, framing, prefix=''):
    pos = 0
    if not prefix and buf[:4] == b'\x01\r\r\n':
        framing['noaaport_soh'] = True
        pos = 4
        m = SEQ_RE.match(buf, pos)
        if m:
            framing['noaaport_sequence'] = m.group(1).decode()
            pos = m.end()
    m = WMO_RE.match(buf, pos)
    if m:
        framing[prefix + 'wmo_heading'] = m.group(0)[:-3].decode()
        pos = m.end()
        m2 = AWIPS_RE.match(buf, pos)
        if m2:
            framing[prefix + 'awips_id'] = m2.group(1).decode()
            pos = m2.end()
    return pos


def unwrap(raw):
    """Remove NOAAPort/WMO framing and zlib; return (framing, message bytes)."""
    framing = {
        'noaaport_soh': False, 'noaaport_sequence': None, 'wmo_heading': None,
        'awips_id': None, 'trailer': None, 'zlib_frames': 0, 'zlib_uncompressed_bytes': 0,
        'ccb_bytes': 0, 'inner_wmo_heading': None, 'inner_awips_id': None, 'text_only': False,
        'message_offset': 0, 'message_bytes': 0,
    }
    pos = _strip_headings(raw, framing)
    body = raw[pos:]
    if body.endswith(ETX_TRAILER):
        framing['trailer'] = 'noaaport-etx'
        body = body[:-len(ETX_TRAILER)]
    if len(body) >= 2 and body[0] == 0x78 and ((body[0] << 8) | body[1]) % 31 == 0:
        out = bytearray()
        rest = body
        while rest:
            d = zlib.decompressobj()
            try:
                out += d.decompress(rest)
            except zlib.error:
                break
            framing['zlib_frames'] += 1
            rest = d.unused_data
        if rest:
            raise ValueError(f'{len(rest)} bytes after the last zlib frame')
        framing['zlib_uncompressed_bytes'] = len(out)
        body = bytes(out)
        # NOAAPort Communications Control Block: flag byte 0x40, length in halfwords.
        if len(body) >= 2 and body[0] == 0x40 and 2 * body[1] <= len(body):
            framing['ccb_bytes'] = 2 * body[1]
            body = body[2 * body[1]:]
        body = body[_strip_headings(body, framing, 'inner_'):]
    heading = framing['wmo_heading'] or ''
    if heading.startswith('NOUS'):
        framing['text_only'] = True
    framing['message_bytes'] = len(body)
    return framing, body


# --------------------------------------------------------------------------------------
# ICD walker
# --------------------------------------------------------------------------------------

def u16(d, o):
    return struct.unpack_from('>H', d, o)[0]


def i16(d, o):
    return struct.unpack_from('>h', d, o)[0]


def u32(d, o):
    return struct.unpack_from('>I', d, o)[0]


class Walk:
    def __init__(self, data):
        self.d = data
        self.unknown = []
        self.data_packets = []  # (where, layer, index, code, header)

    def packets(self, start, end, where, layer=None):
        """Walk packets in [start, end). Returns (codes, nested)."""
        d = self.d
        codes = []
        nested = []
        p = start
        while p < end:
            if p + 2 > end:
                self.unknown.append({'where': where, 'code': None, 'byte_offset': p})
                break
            c = u16(d, p)
            header = None
            try:
                if c in (23, 24):
                    n = u16(d, p + 2)
                    sub, _ = self.packets(p + 4, p + 4 + n, where + f'/scit{len(codes)}')
                    nested.append({'layer': layer, 'index': len(codes), 'code': c, 'packets': sub})
                    q = p + 4 + n
                elif c in LENGTH_PREFIXED:
                    q = p + 4 + u16(d, p + 2)
                elif c in RADIAL_PACKETS:
                    header = {'first_bin': u16(d, p + 2), 'num_bins': u16(d, p + 4),
                              'i_center': i16(d, p + 6), 'j_center': i16(d, p + 8),
                              'scale_factor': u16(d, p + 10), 'num_radials': u16(d, p + 12)}
                    q = p + 14
                    for _ in range(header['num_radials']):
                        n = u16(d, q)
                        q += 6 + (n if c == 16 else 2 * n)
                elif c in RASTER_PACKETS:
                    header = {'op_flags': [u16(d, p + 2), u16(d, p + 4)],
                              'i_start': i16(d, p + 6), 'j_start': i16(d, p + 8),
                              'x_scale_int': i16(d, p + 10), 'x_scale_frac': i16(d, p + 12),
                              'y_scale_int': i16(d, p + 14), 'y_scale_frac': i16(d, p + 16),
                              'num_rows': u16(d, p + 18), 'packing': u16(d, p + 20)}
                    q = p + 22
                    for _ in range(header['num_rows']):
                        q += 2 + u16(d, q)
                elif c in (17, 18):
                    header = {'num_boxes': u16(d, p + 6), 'num_rows': u16(d, p + 8)}
                    q = p + 10
                    for _ in range(header['num_rows']):
                        q += 2 + u16(d, q)
                elif c == 33:
                    header = {'num_cells': u16(d, p + 10), 'num_rows': u16(d, p + 12)}
                    q = p + 14
                    for _ in range(header['num_rows']):
                        q += 2 + u16(d, q)
                elif c in (28, 29):
                    header = {'reserved': u16(d, p + 2), 'num_bytes': u32(d, p + 4)}
                    q = p + 8 + header['num_bytes']
                elif c == 0x0802:
                    q = p + 6
                elif c == 0x0E03:
                    q = p + 10 + u16(d, p + 8)
                elif c == 0x3501:
                    q = p + 4 + u16(d, p + 2)
                elif c == 30:  # IRM (product 83, no ICD): code, five Real*4 values
                    q = p + 22
                elif c == 31:  # IRM: code, count of the packet 15/2 pairs that follow
                    q = p + 4
                elif c == 32:  # IRM: code, rows, then rows of byte count + run/level bytes
                    header = {'num_rows': u16(d, p + 2)}
                    q = p + 4
                    for _ in range(header['num_rows']):
                        q += 2 + u16(d, q)
                else:
                    self.unknown.append({'where': where, 'code': c, 'byte_offset': p})
                    break
            except struct.error:
                self.unknown.append({'where': where, 'code': c, 'byte_offset': p})
                break
            if q > end:
                self.unknown.append({'where': where, 'code': c, 'byte_offset': p})
                break
            if header is not None:
                self.data_packets.append((where, layer, len(codes), c, header))
            codes.append(c)
            p = q
        return codes, nested


def read_pages(d, o, end):
    """Figure 3-16 page block starting at the (-1) divider. Returns (pages, next)."""
    if i16(d, o) != -1:
        raise ValueError('page block divider missing')
    num_pages = i16(d, o + 2)
    p = o + 4
    pages = []
    for _ in range(num_pages):
        lines = []
        while True:
            n = i16(d, p)
            p += 2
            if n == -1:
                break
            if n < 0 or p + n > end:
                raise ValueError('bad line length')
            lines.append(d[p:p + n].decode('latin-1'))
            p += n
        pages.append(lines)
    return pages, p


def pages_summary(pages):
    text = '\x0c'.join('\n'.join(lines) for lines in pages)
    return {'num_pages': len(pages), 'lines_per_page': [len(p) for p in pages],
            'text_sha256': hashlib.sha256(text.encode('latin-1')).hexdigest()}


def looks_like_symbology(d, o):
    if o + 16 > len(d) or i16(d, o) != -1 or i16(d, o + 2) != 1:
        return False
    length = u32(d, o + 4)
    layers = u16(d, o + 8)
    return 10 <= length <= len(d) - o and 1 <= layers <= 18 and i16(d, o + 10) == -1


def walk_message(msg):
    """Walk a Level III message. Returns dict with header/halfwords/blocks/etc."""
    out = {'message_header': None, 'halfwords': None, 'product_code': None,
           'compression': None, 'blocks': None, 'packet_codes': [], 'walker_data': []}
    if len(msg) < 18:
        raise ValueError('message shorter than a Message Header Block')
    code, date, time, length, src, dst, nblk = struct.unpack_from('>hHiIhhH', msg, 0)
    out['message_header'] = {'code': code, 'date': date, 'time': time, 'length': length,
                             'source_id': src, 'destination_id': dst, 'num_blocks': nblk}
    if code == 2 or len(msg) < 120 or i16(msg, 18) != -1:
        return out
    out['halfwords'] = list(struct.unpack_from('>60H', msg, 0))

    def hw(n):
        return out['halfwords'][n - 1]

    pc = i16(msg, 30)
    out['product_code'] = pc
    data = msg
    comp = {'hw51': hw(51), 'bzip2': False, 'compressed_bytes': None,
            'uncompressed_bytes': None, 'hw52_53_uncompressed_size': (hw(52) << 16) | hw(53)}
    if msg[120:123] == b'BZh':
        dec = bz2.BZ2Decompressor()
        body = dec.decompress(msg[120:])
        comp.update(bzip2=True, compressed_bytes=len(msg) - 120 - len(dec.unused_data),
                    uncompressed_bytes=len(body))
        data = msg[:120] + body
    out['compression'] = comp
    sym, gra, tab = struct.unpack_from('>III', data, 108)
    w = Walk(data)
    blocks = {'symbology': None, 'graphic': None, 'tabular': None, 'standalone_tabular': None,
              'cell_trend': None, 'rcm': None, 'unknown': w.unknown}
    all_codes = set()

    def note(codes, nested):
        all_codes.update(codes)
        for n in nested:
            all_codes.update(n['packets'])

    if pc == 74 and sym and data[2 * sym:2 * sym + 10] == b'1234 ROBUU':
        text = data[2 * sym:]
        blocks['rcm'] = {'text_bytes': len(text), 'text_sha256': hashlib.sha256(text).hexdigest()}
    elif sym and pc in STANDALONE_CANDIDATES and not looks_like_symbology(data, 2 * sym):
        pages, _ = read_pages(data, 2 * sym, len(data))
        blocks['standalone_tabular'] = pages_summary(pages)
        if gra:
            start = 2 * (gra - 1)
            if i16(data, start) in (21, 22):
                codes, nested = w.packets(start, len(data), 'cell_trend')
                blocks['cell_trend'] = {'offset_halfwords': gra, 'packets': codes}
                note(codes, nested)
    else:
        if sym:
            o = 2 * sym
            if i16(data, o) != -1 or i16(data, o + 2) != 1:
                raise ValueError('symbology block divider/id missing')
            blen = u32(data, o + 4)
            nlayers = u16(data, o + 8)
            q = o + 10
            layers = []
            nested_all = []
            for li in range(nlayers):
                if i16(data, q) != -1:
                    raise ValueError('layer divider missing')
                llen = u32(data, q + 2)
                q += 6
                codes, nested = w.packets(q, q + llen, f'symbology/layer{li}', li)
                layers.append({'length': llen, 'packets': codes})
                nested_all.extend(nested)
                note(codes, nested)
                q += llen
            blocks['symbology'] = {'length': blen, 'num_layers': nlayers, 'layers': layers,
                                   'nested': nested_all}
        if gra:
            o = 2 * gra
            if i16(data, o) != -1 or i16(data, o + 2) != 2:
                raise ValueError('graphic block divider/id missing')
            blen = u32(data, o + 4)
            npages = u16(data, o + 8)
            q = o + 10
            pages = []
            for _ in range(npages):
                pnum, plen = u16(data, q), u16(data, q + 2)
                q += 4
                codes, nested = w.packets(q, q + plen, f'graphic/page{pnum}')
                pages.append({'page': pnum, 'length': plen, 'packets': codes})
                note(codes, nested)
                q += plen
            blocks['graphic'] = {'length': blen, 'num_pages': npages, 'pages': pages}
        # Observed (NCEI 1993-1994): the tabular offset can name the end of the
        # message (its halfwords 5-6 length) when no block was sent.
        if tab and 2 * tab != len(data):
            o = 2 * tab
            if i16(data, o) != -1 or i16(data, o + 2) != 3:
                raise ValueError('tabular block divider/id missing')
            blen = u32(data, o + 4)
            mcode = i16(data, o + 8)
            tpc = i16(data, o + 8 + 18 + 12)
            start = o + 8 + 18 + 102
            if mcode == 74 and data[start:start + 10] == b'1234 ROBUU':
                # IRM (product 83): the radar coded message after the second headers.
                text = data[start:o + blen]
                blocks['tabular'] = {'length': blen, 'message_code': mcode, 'product_code': tpc,
                                     'rcm_text_bytes': len(text),
                                     'rcm_text_sha256': hashlib.sha256(text).hexdigest()}
            else:
                pages, _ = read_pages(data, start, len(data))
                blocks['tabular'] = {'length': blen, 'message_code': mcode, 'product_code': tpc,
                                     **pages_summary(pages)}
    out['blocks'] = blocks
    out['packet_codes'] = sorted(all_codes)
    out['walker_data'] = w.data_packets
    return out


# --------------------------------------------------------------------------------------
# MetPy
# --------------------------------------------------------------------------------------

def install_metpy_shim():
    import metpy.io.nexrad as nx

    def nexrad_to_datetime(julian_date, ms_midnight):
        return datetime(1970, 1, 1) + timedelta(days=julian_date - 1, milliseconds=ms_midnight)

    nx.nexrad_to_datetime = nexrad_to_datetime


class _Capture(logging.Handler):
    def __init__(self):
        super().__init__(logging.WARNING)
        self.messages = []

    def emit(self, record):
        self.messages.append(record.getMessage())


def jsonable(v):
    if isinstance(v, datetime):
        return v.strftime('%Y-%m-%dT%H:%M:%S.%fZ')
    if isinstance(v, (np.integer,)):
        return int(v)
    if isinstance(v, (np.floating, float)):
        f = float(v)
        return f if np.isfinite(f) else None
    if isinstance(v, (np.bool_,)):
        return bool(v)
    if isinstance(v, bytes):
        return v.decode('latin-1')
    if hasattr(v, '_asdict'):
        return jsonable(v._asdict())
    if isinstance(v, (list, tuple)):
        return [jsonable(x) for x in v]
    if isinstance(v, dict):
        return {str(k): jsonable(x) for k, x in v.items()}
    return v


def read_metpy(raw, file_id):
    import metpy
    from metpy.io import Level3File

    cap = _Capture()
    logger = logging.getLogger('metpy.io.nexrad')
    logger.addHandler(cap)
    old_level, old_prop = logger.level, logger.propagate
    logger.setLevel(logging.WARNING)
    logger.propagate = False
    detail = {'version': metpy.__version__, 'warnings': [], 'error': None}
    f = None
    try:
        with warnings.catch_warnings():
            warnings.simplefilter('ignore')
            f = Level3File(io.BytesIO(raw))
    except Exception as e:  # noqa: BLE001 - any MetPy failure marks the file unsupported
        detail['error'] = f'{type(e).__name__}: {e}'
    finally:
        logger.removeHandler(cap)
        logger.setLevel(old_level)
        logger.propagate = old_prop
    detail['warnings'] = [m.replace('No File', file_id) for m in cap.messages]
    if f is None:
        return 'unsupported', detail, None
    status = 'partial' if detail['warnings'] else 'ok'
    detail['product_name'] = getattr(f, 'product_name', None)
    detail['max_range'] = jsonable(getattr(f, 'max_range', None))
    mapper = getattr(f, 'map_data', None)
    detail['mapper'] = type(mapper).__name__ if mapper is not None else None
    detail['header'] = jsonable(getattr(f, 'header', None))
    detail['prod_desc'] = jsonable(getattr(f, 'prod_desc', None))
    detail['thresholds'] = jsonable(getattr(f, 'thresholds', None))
    detail['dep_vals'] = jsonable(getattr(f, 'depVals', None))
    detail['metadata'] = jsonable(getattr(f, 'metadata', None))
    if hasattr(f, 'gsm'):
        detail['gsm'] = jsonable(f.gsm)
    if hasattr(f, 'text'):
        detail['text_sha256'] = hashlib.sha256(f.text.encode('latin-1')).hexdigest()
    sym = getattr(f, 'sym_block', None)
    detail['sym_layer_packet_counts'] = [len(layer) for layer in sym] if sym is not None else None
    graph = getattr(f, 'graph_pages', None)
    detail['graph_page_packet_counts'] = [len(p) for p in graph] if graph is not None else None
    tabp = getattr(f, 'tab_pages', None)
    detail['tab_pages'] = len(tabp) if tabp is not None else None
    return status, detail, f


# --------------------------------------------------------------------------------------
# Data grids
# --------------------------------------------------------------------------------------

def grid_from_metpy(packet, code, header):
    """(array, dtype, notes). Observed: the radials of product 46 of 1994 hold
    fewer levels than the header's bin count; they are padded with level 0 (the
    decoder's fill) and ``notes`` records ``short_radials`` and ``max_levels``."""
    if code in RADIAL_PACKETS:
        nbins = header['num_bins']
        rows = []
        short, longest = 0, 0
        for radial in packet['data']:
            vals = list(radial)[:nbins]
            longest = max(longest, len(vals))
            if len(vals) < nbins:
                short += 1
                vals = vals + [0] * (nbins - len(vals))
            rows.append(vals)
        notes = {'short_radials': short, 'max_levels': longest} if short else {}
        return np.array(rows, dtype=np.uint8).reshape(len(rows), nbins), 'u8', notes
    if code in RASTER_PACKETS or code in (17, 18):
        rows = [list(r) for r in packet['data']]
        width = {len(r) for r in rows}
        if len(width) != 1:
            raise ValueError(f'ragged raster rows: {sorted(width)}')
        return np.array(rows, dtype=np.uint8), 'u8', {}
    raise ValueError(f'no grid for packet {code}')


def grids_from_generic(packet):
    comps = packet.get('components') if isinstance(packet, dict) else None
    if comps is None:
        return []
    if not isinstance(comps, list):
        comps = [comps]
    out = []
    for comp in comps:
        radials = getattr(comp, 'radials', None)
        if not radials:
            continue
        nb = {r.num_bins for r in radials}
        if len(nb) != 1:
            raise ValueError(f'generic radials with differing bin counts {sorted(nb)}')
        arr = np.array([list(r.data)[:r.num_bins] for r in radials], dtype=np.int64)
        if arr.min() < 0 or arr.max() > 0xFFFF:
            raise ValueError('generic radial values outside u16')
        info = {'description': comp.description, 'gate_width': float(comp.gate_width),
                'first_gate': float(comp.first_gate)}
        out.append((arr.astype(np.uint16), info))
    return out


def summarize(arr, dtype, f, product_code):
    enc = arr.astype('>u2') if dtype == 'u16be' else arr.astype(np.uint8)
    levels, counts = np.unique(arr, return_counts=True)
    entry = {'rows': int(arr.shape[0]), 'cols': int(arr.shape[1]), 'dtype': dtype,
             'raw_sha256': hashlib.sha256(enc.tobytes(order='C')).hexdigest(),
             'histogram': {str(int(k)): int(v) for k, v in zip(levels, counts, strict=True)},
             'physical': None}
    import metpy.io.nexrad as nx
    if f is None or product_code not in nx.Level3File.prod_spec_map:
        return entry
    try:
        mapped = f.map_data(arr.astype(np.int64))
    except Exception as e:  # noqa: BLE001
        entry['physical_error'] = f'{type(e).__name__}: {e}'
        return entry
    topped = None
    if isinstance(mapped, tuple):
        mapped, topped = mapped
    vals = np.asarray(mapped, dtype=np.float64)
    finite = np.isfinite(vals)
    phys = {'finite': int(finite.sum()), 'masked': int((~finite).sum()),
            'min': None, 'max': None, 'mean': None}
    if finite.any():
        fv = vals[finite]
        phys.update(min=float(fv.min()), max=float(fv.max()), mean=float(fv.mean()))
    if topped is not None:
        phys['topped'] = int(np.asarray(topped, dtype=bool).sum())
    entry['physical'] = phys
    return entry


def build_data(walk, f):
    if f is None:
        return None
    sym = getattr(f, 'sym_block', None)
    out = []
    for where, layer, index, code, header in walk['walker_data']:
        if not where.startswith('symbology') or sym is None or layer is None:
            continue
        try:
            packet = sym[layer][index]
        except IndexError:
            continue
        base = {'layer': layer, 'index': index, 'packet': code, 'header': header}
        try:
            if code in (28, 29):
                for arr, info in grids_from_generic(packet):
                    e = dict(base, generic_component=info)
                    e.update(summarize(arr, 'u16be', f, walk['product_code']))
                    out.append(e)
                continue
            arr, dtype, notes = grid_from_metpy(packet, code, header)
        except Exception as e:  # noqa: BLE001
            out.append(dict(base, error=f'{type(e).__name__}: {e}'))
            continue
        e = dict(base, **notes)
        pc = walk['product_code'] if code != 18 else None  # packet 18 is not map_data's scale
        e.update(summarize(arr, dtype, f, pc))
        out.append(e)
    return out


def crosscheck(walk, detail):
    """Compare walker structure with MetPy's; None when nothing is comparable."""
    blocks = walk['blocks'] or {}
    checks = []
    sym = blocks.get('symbology')
    if sym is not None and detail.get('sym_layer_packet_counts') is not None:
        checks.append([len(layer['packets']) for layer in sym['layers']]
                      == detail['sym_layer_packet_counts'])
    graph = blocks.get('graphic')
    trend = blocks.get('cell_trend')
    if detail.get('graph_page_packet_counts') is not None:
        if graph is not None:
            checks.append([len(p['packets']) for p in graph['pages']]
                          == detail['graph_page_packet_counts'])
        elif trend is not None:
            checks.append([len(trend['packets'])] == detail['graph_page_packet_counts'])
    pages = blocks.get('tabular') or blocks.get('standalone_tabular')
    if pages is not None and detail.get('tab_pages') is not None:
        checks.append(pages['num_pages'] == detail['tab_pages'])
    return all(checks) if checks else None


# --------------------------------------------------------------------------------------
# Driver
# --------------------------------------------------------------------------------------

def committed_path(entry):
    """Resolve `committed` (relative to testdata/, leading `testdata/` accepted)."""
    rel = Path(entry['committed'])
    if rel.parts and rel.parts[0] == 'testdata':
        rel = Path(*rel.parts[1:])
    return ROOT / 'testdata' / rel


def golden_for(entry):
    path = committed_path(entry)
    raw = path.read_bytes()
    digest = hashlib.sha256(raw).hexdigest()
    if digest != entry['sha256'] or len(raw) != entry['size']:
        raise ValueError(f"{entry['id']}: committed file does not match manifest sha256/size")
    framing, msg = unwrap(raw)
    result = {'id': entry['id'], 'sha256': digest, 'size': len(raw), 'framing': framing,
              'message_header': None, 'halfwords': None, 'product_code': None,
              'compression': None, 'blocks': None, 'packet_codes': []}
    walk = None
    if not framing['text_only']:
        walk = walk_message(msg)
        for k in ('message_header', 'halfwords', 'product_code', 'compression', 'blocks',
                  'packet_codes'):
            result[k] = walk[k]
    status, detail, f = read_metpy(raw, entry['id'])
    detail['crosscheck'] = crosscheck(walk, detail) if walk else None
    result['data'] = build_data(walk, f) if walk else None
    result['metpy'] = status
    result['metpy_detail'] = detail
    return result


def dumps(obj):
    return json.dumps(obj, indent=1, sort_keys=False, ensure_ascii=True) + '\n'


def main():
    ap = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    ap.add_argument('--check', action='store_true', help='fail if golden files are stale')
    ap.add_argument('ids', nargs='*')
    args = ap.parse_args()
    install_metpy_shim()
    manifest = tomllib.loads(MANIFEST.read_text(encoding='utf-8'))
    entries = [e for e in manifest['file'] if not args.ids or e['id'] in args.ids]
    GOLDEN_DIR.mkdir(parents=True, exist_ok=True)
    stale = []
    counts = {}
    for entry in entries:
        text = dumps(golden_for(entry))
        target = GOLDEN_DIR / f"{entry['id']}.json"
        status = json.loads(text)['metpy']
        counts[status] = counts.get(status, 0) + 1
        if args.check:
            if not target.exists() or target.read_text(encoding='utf-8') != text:
                stale.append(entry['id'])
        else:
            target.write_bytes(text.encode('utf-8'))
    print(f'{len(entries)} files; metpy status counts: {counts}')
    if stale:
        print('stale golden files:', ', '.join(stale))
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
