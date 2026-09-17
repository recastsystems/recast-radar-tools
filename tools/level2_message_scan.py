"""Independent Level II message scan used for the expected values in
crates/recast-radar-io-nexrad/tests/message_walker.rs.

Written separately from the Rust walker (standard library only). For each
manifest id or file path it prints, for the metadata record and for the whole
file:

- frames: fixed and variable frames read, empty (size 0) frames
- tokens: messages and reassembly errors in file order, run-length encoded,
  as "type:segments/frames" (segment count from the first segment's header,
  frames joined) or "error: reason"
- per-type message counts

Framing follows ICD 2620002AA Table II and MetPy's Level2File: a 12-byte CTM
header, the 16-byte message header, messages 29 and 31 sized by their
halfword count, a size of 65535 meaning bytes 12-15 hold the size in bytes,
everything else in fixed 2432-byte frames.

Usage: python tools/level2_message_scan.py <manifest-id-or-path> [...]
"""

import bz2
import collections
import gzip
import json
import os
import struct
import sys

FRAME = 2432
CTM = 12
METADATA_FRAMES = 134


def cache_path(name):
    if os.path.exists(name):
        return name
    root = os.environ.get('RECAST_RADAR_TESTDATA') or os.path.join(
        os.environ['LOCALAPPDATA'], 'recast-radar-tools', 'testdata')
    return os.path.join(root, name)


def volume_header_len(data):
    return 24 if data[:4] == b'AR2V' or data[:8] == b'ARCHIVE2' else 0


def ldm_records(data):
    """Return decompressed LDM records, or None when data is not LDM-framed."""
    if data[4:7] != b'BZh':
        return None
    out, off = [], 0
    while off + 4 <= len(data):
        (n,) = struct.unpack('>i', data[off:off + 4])
        if n == 0 or (n == -1 and off + 4 == len(data)):
            break
        off += 4
        out.append(bz2.decompress(data[off:off + abs(n)]))
        off += abs(n)
        if n < 0:
            break
    return out


def records(raw):
    if raw[:2] == b'\x1f\x8b':
        raw = gzip.decompress(raw)
    elif raw[:3] == b'BZh':
        raw = bz2.decompress(raw)
    body = raw[volume_header_len(raw):]
    recs = ldm_records(body)
    if recs is None:
        return body, body[:METADATA_FRAMES * FRAME]
    return b''.join(recs), (recs[0] if recs else b'')


def scan(buf):
    frames = empty = 0
    raw_frames = []
    off = 0
    while off + CTM + 16 <= len(buf):
        size, _chan, typ, _seq, _date, _ms, nseg, segn = struct.unpack(
            '>HBBHHIHH', buf[off + CTM:off + CTM + 16])
        frames += 1
        if size == 0:
            empty += 1
            off += FRAME
            continue
        if size == 0xFFFF:
            length = CTM + ((nseg << 16) | segn)
            nseg, segn = 1, 1
        elif typ in (29, 31):
            length = CTM + 2 * size
        else:
            length = FRAME
        raw_frames.append((typ, nseg, segn))
        off += length
    # Reassembly: a frame numbered 1 of N > 1 starts a message; frames of the
    # same type numbered one higher each time continue it (segment counts may
    # disagree between segments in Build 10 files); the message completes at
    # the frame whose number reaches that frame's own count. Runs that never
    # start at 1 are orphans; runs that start at 1 but stop early are
    # incomplete. Events are listed in run order.
    events = []
    i = 0
    while i < len(raw_frames):
        typ, nseg, segn = raw_frames[i]
        if nseg <= 1:
            events.append(('message', typ, nseg, 1))
            i += 1
            continue
        j = i
        while not (segn == 1 and raw_frames[j][2] >= raw_frames[j][1]):
            if not (j + 1 < len(raw_frames) and raw_frames[j + 1][0] == typ
                    and raw_frames[j + 1][1] > 1
                    and raw_frames[j + 1][2] == raw_frames[j][2] + 1):
                break
            j += 1
        last, last_count, joined = raw_frames[j][2], raw_frames[j][1], j - i + 1
        if segn == 1 and last >= last_count:
            events.append(('message', typ, nseg, joined))
        elif segn == 1:
            events.append(('error', f'segmented message type {typ} ended after '
                                    f'{joined} of {last_count} segments'))
        else:
            events.append(('error', f'message type {typ} segments {segn}..={last} '
                                    f'of {last_count} have no first segment'))
        i = j + 1
    # Tokens as the Rust test writes them: "type:segments/frames" per message
    # (segments from the first segment's header, 1 for extended size) or
    # "error: reason", run-length encoded.
    tokens = []
    for event in events:
        token = (f'{event[1]}:{event[2]}/{event[3]}' if event[0] == 'message'
                 else f'error: {event[1]}')
        if tokens and tokens[-1][0] == token:
            tokens[-1][1] += 1
        else:
            tokens.append([token, 1])
    counts = collections.Counter(e[1] for e in events if e[0] == 'message')
    return {
        'frames': frames, 'empty_frames': empty, 'end': off, 'len': len(buf),
        'tokens': tokens, 'counts': dict(sorted(counts.items())),
    }


def main(names):
    for name in names:
        with open(cache_path(name), 'rb') as f:
            raw = f.read()
        whole, metadata = records(raw)
        print(json.dumps({'id': name, 'metadata_record': scan(metadata),
                          'whole_file': scan(whole)}))


if __name__ == '__main__':
    main(sys.argv[1:])
