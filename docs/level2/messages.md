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

Module: `clutter_censor.rs`. Status: placeholder; the walker yields the body unparsed.
No real sample (the RPG sends it to the RDA).

## Message 9: Request for Data (Table XIII)

Module: `request.rs`. Status: decoded per ICD. **No real sample.**
The RPG sends this to the RDA. Only the code mapping is unit tested.

## Messages 11 and 12: Loop Back Test (Table VIII)

Module: `loopback.rs`. Status: decoded per ICD. **No real sample.**
These are exchanged on wideband connection and not recorded in Archive II files.

## Message 13: Clutter Filter Bypass Map (Table IX)

Module: `bypass_map.rs`. Status: placeholder; the walker yields the body unparsed.
Real samples: 49 segments in files from 2008 through Build 18.2 (KDVN 2020); 14 segments plus an orphan run
in KLIX 2005. The ICD says it has not been sent since Build 19.

## Message 15: Clutter Filter Map (Table XIV)

Module: `clutter_filter_map.rs`. Status: placeholder; the walker yields the body unparsed.
Real samples: every WSR-88D metadata record from 2005 on (not TDWR). Segment counts: 5 (most), 6 (KMAF 2023),
7 (KTLX 2013), 62 (KLIX 2005), 77 (KPAH 2008, see quirks); KVWX 2008 has only broken segments.

## Message 18: RDA Adaptation Data (Table XV)

Module: `adaptation.rs`. Status: placeholder; the walker yields the body unparsed.
Real samples: 4 segments in every metadata record from 2005 on, except KVWX 2008 and the TDWR files.

## Message 29: Model Data

Status: yielded unparsed. Table I lists type 29 as reserved.
Real sample: the KLIX 2021-08-29 `_MDM` file, where it is one 809,229-byte message with a size of 65535.

## Message 31: Digital Radar Data Generic Format (Table XVII)

Module: `msg31_blocks.rs` (`DigitalRadarDataGeneric`). Status: **verified**. The walker decodes every block:
the Data Header Block (Table XVII-A), VOL (XVII-E), ELV (XVII-F), RAD (XVII-H), and every data moment block
(XVII-B) including CFP. Moment blocks with names the ICD does not define are kept as moments with their name.
Blocks of any other type or name are kept as bytes. `decode_volume_from_bytes` still builds the moment grids
on its own fast path; the two decoders agree radial by radial (`volume_decoder_agrees_with_typed_radials`).
Real samples: every volume from 2008 on (not the status-only stub or the `_MDM` file).

Layouts are chosen from the sizes in the message, not from the build:

| Structure | Layout | Selected by | Builds in the corpus |
|---|---|---|---|
| Data Header Block | 68 bytes, 9 pointer slots | first block pointer | 10.0 to 18.2, TDWR |
| Data Header Block | 72 bytes, 10 slots (CFP) | first block pointer | 19.1 on |
| VOL | 44 bytes | LRTUP 44 to 51 | 10.0 to 19.1, TDWR |
| VOL | 52 bytes, adds the ZDR bias estimate | LRTUP 52 or more | 20.1 on |
| RAD | 20 bytes | LRTUP 20 to 27 | 10.0 to 13.2, TDWR |
| RAD | 28 bytes, adds H and V calibration constants | LRTUP 28 or more | 14.0 on |
| ELV | 12 bytes | LRTUP | all |

VOL major version: 1 through Build 13 and on TDWR, 2 from Build 14, 3 from Build 20. Processing status: 0
before Build 14, 1 (RxR noise) from Build 14, 3 (RxR noise and CBT) from Build 19. A block larger than the
newest layout decodes with that layout, because ICD note 32 allows fields to be appended. A block smaller than
the oldest layout is an error.

Verification (`tests/messages_msg31.rs`):

- **MetPy goldens.** `tools/level2_golden.py msg31` writes `testdata/level2/golden/msg31/*.json` from MetPy
  1.7.1. The files are 13 volumes (Builds 10.0, 12.0, 13.1, 14.0, 18.2, 19.1, 20.1, 21.0, 22.0 and 24.1,
  TDWR, and KVWX 2008) plus the committed KIWA chunks. For every sweep, the test compares the first radial
  field by field. It compares every radial through per-field distinct values, or through count, min, max and
  sum. MetPy's RDA build is compared as well.
- **SNR threshold.** Table XVII-B scales the SNR threshold by 0.125 dB; MetPy uses 0.1 dB. On 13 golden
  sources, each moment's threshold at 0.125 dB equals the message 5 threshold for its elevation cut. The only
  exception is TDWR TSTL's last cut, which records 0 dB for REF, VEL and SW where message 5 says 1.0 dB.
- **Fields MetPy does not read.** These are the VOL ZDR bias estimate, RAD radial flags, and spare bytes. The
  expected values come from the file bytes, read with a separate Python script: ZDR bias raw 407 to 430 (-0.34
  to +0.375 dB), or 0 (not available) for KMAF 2023 and both KTLX 2024 files. Radial flags and spares are 0.
- **Every field of one radial.** The first radial of the committed KIWA chunk 002 is checked field by field
  against its hex. The same radial's gate code counts (REF below threshold, CFP filter states 0-2, CFP values
  0-73 dB) are checked against the separate Python reader.
- **Mutations of that radial.** Renamed blocks and a changed block type exercise unknown-block handling. Changed
  LRTUP sizes exercise layout selection. Re-encoding the real blocks with zlib and BZIP2 exercises compression.
  Pointer, count, word-size and truncation errors are also covered.

Quirks found in the real corpus:

- The data block count is the number of pointers in use, not the number of slots. Radials write their
  nonzero pointers first, then zero slots up to the fixed 68- or 72-byte header. For example, a Build 10
  reflectivity-only radial has 4 pointers and 5 zero slots.
- Build 10 KDMX 2008: 2520 radials declare a radial length one byte shorter than the halfword-padded body.
- KVWX 2008 has a blank (four-space) ICAO in every radial, no azimuth indexing, and no message 5.

**No real sample:** compressed radials (compression indicator 1 BZIP2, 2 zlib). The decoder inflates from the
first block pointer, where the Data Header Block ends in every uncompressed radial. The inflated size is
limited by the radial length. The mutation test checks only that the decoder reads its own re-encoding; no real
file confirms this layout.

RDA build (`msg31_blocks::RdaBuild`, message 2 halfword 10, note 6): the value divided by 100 when that is
greater than 2, otherwise the value divided by 10. `rda_build_from_metadata_records` checks the raw value and
build of the 26 corpus files whose metadata record has a message 2, and compares the build with the manifest
`build:` tags. KVWX 2008 records 1996 ("19.96", a build that did not exist in 2008). TDWR records 20
("2.0"). Files from 1991 and 2005 record 0. The golden tests compare the build with MetPy's `rda_build`.

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
