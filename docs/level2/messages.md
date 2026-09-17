# Level II messages

Status of each message type in `recast-radar-io-nexrad` (`src/messages/`). References are to the RDA/RPG
ICD, document 2620002AA (Build 24.0, 19 August 2025), the latest revision published by the NOAA Radar
Operations Center.

A decoder is **verified** only when tests check it against real files. A decoder written from the ICD with
no real sample in the corpus is marked **no real sample** and is not claimed as verified.

## Framing and reassembly

`messages::RawMessages` walks decompressed record bytes; `messages::MessageWalker` also decodes the bodies.
Helpers: `messages::metadata_record(raw)` (first LDM record, or the first 134 frames of raw-record files)
and `messages::record_bytes(raw)` (every record, decompressed and concatenated).

- Each frame is a 12-byte CTM header, the 16-byte message header (Table II), then the body.
- Messages 29 and 31 are variable length (`size_halfwords * 2` bytes). A size of 65535 means header bytes
  12-15 hold the message size in bytes (Table II notes 6 and 7). Every other message is in a fixed
  2432-byte frame. A size of 0 is an empty frame.
- Segmented messages (13, 15, 18) are joined from consecutive frames numbered 1..N.
- Verified: `tests/message_walker.rs` checks frame counts and message sequences against
  `tools/level2_message_scan.py`, a separate Python scanner. It covers 11 metadata records (1991 ARCHIVE2
  to Build 24.1, TDWR, a status-only stub) and 8 whole files, including the 65535-size Message 29 in the
  KLIX `_MDM` file. For volumes with radials, it also checks that the Message 1/31 count equals the radial
  count from `decode_volume_from_bytes`.

Quirks found in the real corpus:

- **KPAH 2008-04-15 (Build 10.0).** Message 15 segment 1 says 5 segments, and segments 2-77 say 77. All 77
  have the same generation time. The walker joins all 77, because reassembly does not require the segment
  counts to agree.
- **KVWX 2008-04-15.** The first frames of Messages 15 and 13 have segment count and number 0. Their
  continuation frames therefore have no first segment, and are reported as orphan runs. Stale frames with
  message type 0 continue the numbering.
- **KLIX 2005-08-29.** Message 13 frames 1-14 say 14 segments and make a complete message. Frames 15-48 say
  48 and hold zero date, time and data, so they are reported as an orphan run.
- **KDMX and KPAH 2008.** The fixed metadata record keeps stale frames with message type 0, left over from an
  earlier, longer message.
- **KTLX 2003-05-08.** The first record has message type 202, which the ICD does not define. It is yielded
  unparsed.

## Message 1: Digital Radar Data (Table III)

Status: decoded by the volume decoder (`decode_volume_from_bytes`). The walker yields the body unparsed.
Real samples: ARCHIVE2 files 1991-2003 and KLIX 2005.

## Message 2: RDA Status Data (Table IV)

Module: `rda_status.rs`. Status: placeholder; the walker yields the body unparsed.
Real samples: every archive volume in the corpus (not the `_MDM` file or intermediate chunks).

## Message 3: Performance/Maintenance Data (Table V)

Module: `performance.rs`. Status: placeholder; the walker yields the body unparsed.
Real samples: metadata records from 2005 on, except KVWX 2008 and the TDWR files.

## Messages 4 and 10: Console Message (Table VI)

Module: `console.rs`. Status: decoded per ICD. **No real sample.**
Both are operator messages (4 from the RDA, 10 from the RPG). Neither appears in any corpus file.

## Messages 5 and 7: Volume Coverage Pattern (Table XI)

Module: `vcp.rs`. Status: placeholder; the walker yields the body unparsed.
Real samples: Message 5 in metadata records from 2005 on (not in KVWX 2008). No Message 7 (it goes from the
RPG to the RDA).

## Message 6: RDA Control Commands (Table X)

Module: `control.rs`. Status: decoded per ICD. **No real sample.**
The RPG sends this to the RDA, so Archive II files do not record it. Only the code-to-enum mappings are
unit tested (ZDR bias encoding, restart cut number); no bytes are tested.

## Message 8: Clutter Censor Zones (Table XII)

Module: `clutter_censor.rs` (`ClutterCensorZones`). Status: decoded per ICD. **No real sample.**
The RPG sends this to the RDA, so Archive II files do not record it. Halfword 1 is the number of override
regions (0 to 25). Each region is 6 halfwords: start and stop range (km), start and stop azimuth (degrees),
elevation segment number, and operator select code. The operator select code is the `OperatorSelectCode` enum
shared with Message 15. The decoder rejects more than 25 regions and a body too short for the declared count;
field values are kept as sent. The legacy layout in ICD 2620002B (8 halfwords per region, scaled azimuths) is
not decoded. The only real-byte test relabels the committed start chunk's Message 15 as Message 8. The decoder
rejects it (the map date, 20713, is read as the region count) and the walk continues. That tests the range
check, not the layout.

## Message 9: Request for Data (Table XIII)

Module: `request.rs`. Status: decoded per ICD. **No real sample.**
The RPG sends this to the RDA. Only the code mapping is unit tested.

## Messages 11 and 12: Loop Back Test (Table VIII)

Module: `loopback.rs`. Status: decoded per ICD. **No real sample.**
These are exchanged on wideband connection and not recorded in Archive II files.

## Message 13: Clutter Filter Bypass Map (Table IX)

Module: `bypass_map.rs` (`ClutterFilterBypassMap`). Status: **verified** (`tests/messages_clutter.rs`).
Real samples: 49 segments in KPAH and KDMX 2008 through Build 18.2 (KDVN 2020); 14 segments plus an orphan
run in KLIX 2005. The ICD says it has not been sent since Build 19.

- Layouts: `Current` (2620002AA: generation date and time, 1 to 5 elevation segments of 360 one-degree
  radials) and `Legacy` (2620002B, 2001: no generation time, 256 radials of 1.40625 degrees, radial 0 centred
  on north). KLIX 2005 uses the legacy layout with 2 segments. When halfword 1 is between 1 and 5, the decoder
  reads the legacy layout, as MetPy does. Each radial is 32 halfwords of 512 range bins, 1 km each. A 1 bit
  means bypass the clutter filters, and `BypassMapSegment::bypass(radial, bin)` reads it.
- MetPy 1.7.1 goldens (`tools/level2_golden.py`, `testdata/level2/golden/clutter/`): generation time, segment
  and radial counts, and radial 0 of every segment in 9 files. MetPy has two differences from the ICD, so no
  other values are compared: it reads every radial of a segment from radial 0's halfwords, and it orders each
  halfword's bits least significant first, while note 4 puts bin 0 in the MSB.
- Checked beyond MetPy: halfwords at documented record offsets for 4 radials each in KTLX 2013, KDVN 2020
  and KLIX 2005, and per-segment bypass-bin counts computed from the file bytes. The note 4 bit order is
  checked on all 9 maps by range continuity. Among neighbouring bins with at least one filtered, both are
  filtered across halfword boundaries at 71% or more of the rate inside a halfword when bits are read MSB
  first. Read LSB first, the boundary rate is lower by a factor of 1.97 or more.
- KVWX 2008 has only zero-filled frames whose first segment is numbered 0. MetPy joins them and the walker
  does not.

## Message 15: Clutter Filter Map (Table XIV)

Module: `clutter_filter_map.rs` (`ClutterFilterMap`). Status: **verified** (`tests/messages_clutter.rs`).
Real samples: every WSR-88D metadata record from 2005 on (not TDWR). Segment counts: 5 (most), 6 (KMAF 2023),
7 (KTLX 2013), 62 (KLIX 2005), 77 (KPAH 2008, see quirks); KVWX 2008 has only broken segments.

- The map has 1 to 5 elevation segments. Each segment has 360 azimuth segments, and each azimuth segment
  has 1 to 20 range zones of (op code, end range in km). The decoder rejects segment and zone counts
  outside those ranges. Op codes and end ranges are kept as sent (unknown op codes as `Unknown`). Bytes
  after the map are counted in `trailing_bytes`.
- MetPy 1.7.1 goldens: generation time and every range zone of every azimuth, in 20 files from 2008 to
  2026. Every decoded map has 360 azimuths per segment, ends strictly increasing, a last end range of 511
  and known op codes. The generation time is before the volume time. KTLX 2013 and KMAF 2023 have
  3-zone azimuths. All other maps are one zone ending at 511 with "bypass map in control".
- KPAH 2008: the 77 joined segments hold a 5 403-halfword map followed by 172 800 stale bytes, which go in
  `trailing_bytes`. MetPy reports the same split ("Used: 5400 Avail: 91800").
- KLIX 2005 and KVWX 2008 are zero-filled: date, time and elevation segment count are 0. MetPy skips them,
  and the decoder rejects them. Legacy RDAs used a different Message 15, the "Clutter Filter Notchwidth
  Map" (2620002B Table XIV: 256 azimuths, 16 range zones, byte fields). It is not decoded because the corpus
  has no populated sample. The 32 772-byte KVWX message has that layout's length, but all its bytes are
  zero.

## Message 18: RDA Adaptation Data (Table XV)

Module: `adaptation.rs`. Status: placeholder; the walker yields the body unparsed.
Real samples: 4 segments in every metadata record from 2005 on, except KVWX 2008 and the TDWR files.

## Message 29: Model Data

Status: yielded unparsed. Table I lists type 29 as reserved.
Real sample: the KLIX 2021-08-29 `_MDM` file, where it is one 809,229-byte message with a size of 65535.

## Message 31: Digital Radar Data Generic Format (Table XVII)

Status: decoded by the volume decoder. The walker yields the body unparsed.
Real samples: every volume from 2008 on (not the status-only stub or the `_MDM` file).

## Message 32: RDA PRF Data (Table XVIII)

Module: `prf.rs`. Status: placeholder; the walker yields the body unparsed.
Real samples: PAHG 2025 (Build 23.1), KILX 2026, KIWA 2026 (Build 24.1, including the committed start
chunk).

## Message 33: RDA Log Data (Table XVIV)

Module: `rda_log.rs`. Status: decoded per ICD, and compressed log data is inflated with pure-Rust decoders
(gzip, bzip2, the first member of a ZIP). Inflated data is capped at 64 MiB. **No real sample.**
The message is not recorded in Archive II files. Table XVIV numbers halfwords from 0, so the data starts
at body byte 68. A mutation test relabels a real Message 32 frame as 33 and checks that the decoder's error
does not stop the walk. That test checks the walker's error handling, not the ICD layout.
