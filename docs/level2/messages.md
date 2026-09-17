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

Module: `rda_status.rs`. Status: decoded, **verified** against MetPy on 27 real files.
Real samples: every archive volume in the corpus (not the `_MDM` file or intermediate chunks).

- The RDA channel byte of the message header picks the layout: bit 3 set is an Open RDA (`OrdaRdaStatus`, ICD
  2620002AA Table IV), clear is a legacy RDA (`LegacyRdaStatus`, ICD 2620002B Table IV, the last revision that
  documents it). The walker passes the header to the decoder for messages 2, 3 and 18.
- ORDA bodies are 40 halfwords up to Build 17 and 60 from Build 18 (first seen in KDVN 2020, Build 18.2);
  halfwords 41, 59 and 60 decode to `None` for 40-halfword bodies. TDWR files set bit 3 and use the ORDA layout
  (build 2.0, local VCP 80 or 90).
- Legacy halfwords 10 to 14 are interference detection rate, operational mode, interference suppression unit,
  Archive II status and remaining capacity; 21-22 is the notch width map time. The legacy calibration
  correction (halfword 6) is kept raw, because ICD 2620002B does not state its scale.
- Codes are enums with `Unknown(raw)` or bit-field newtypes with named accessors. Alarm codes (27-40) are kept as
  numbers; the Table IV-A alarm text is not included.
- Verified: `tests/messages_status.rs` compares every halfword MetPy 1.7.1 reads (golden files from
  `tools/level2_golden.py status`, including raw codes before MetPy's name converters) for legacy files from
  1991, 1999, 2005 and 2008, ORDA Builds 10.0 to 24.1, and TDWR. Halfwords 26 and 59, which MetPy skips, are
  0000 in every corpus file; the KLIX 2005 legacy fields and several Build 22 codes are checked against values
  read from the file bytes.

## Message 3: Performance/Maintenance Data (Table V)

Module: `performance.rs`. Status: decoded per ICD 2620002AA (Build 24.0), **verified** against MetPy and file
bytes. Real samples: metadata records from 2005 on, except KVWX 2008 and the TDWR files.

- `PerformanceMaintenance` has one struct per Table V section (communications, AME, RCP/SPIP, power,
  transmitter, tower/utilities, equipment shelter, antenna/pedestal, RF generator/receiver, calibration, file
  status, device status) plus the version halfword. Every field documents its halfword, units, range and codes.
- Table V has reassigned locations over the builds: the DAU-to-SPIP change in Build 17.0, NTP/GPS counters
  removed in 18.0, IFDR and RSP status added in 19.0, CSU alarm counts removed in 20.0, T1/Ethernet port status
  added in 23.0. The module documentation lists the locations. Files from earlier builds decode with the
  Build 24.0 names, so those fields carry the older content. There is no per-build layout.
- The legacy RDA layout (ICD 2620002B, 520 halfwords, bit-packed, non-IEEE floating point) is not decoded. The
  walker yields the KLIX 2005 body unparsed.
- Verified: MetPy's message 3 layout predates Build 17, so the test compares by halfword and type. Each of the
  237 MetPy fields whose location and type match a Build 24.0 field matches exactly in all 20 ORDA volumes
  (Builds 10.0 to 24.1). The 32 MetPy locations with no Build 24.0 counterpart and the 21 Build 24.0 locations
  MetPy does not read are fixed lists in the test. Those 21 are checked against hex values from the KIWA 2026
  volume (Build 24.1), and against ICD ranges in the 12 volumes from Build 19.0 on.

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

Module: `adaptation.rs`. Status: decoded per ICD 2620002AA (Build 24.0), **verified** against MetPy and file
bytes. Real samples: 4 segments in every metadata record from 2005 on, except KVWX 2008 and the TDWR files.

- `RdaAdaptationData` has one field per Table XV entry, named after the ICD mnemonic, with its byte location,
  units and range. Arrays: `a_fuel_conv`, `atten_table`, `h_rnscale`, `atmos`, `el_index`, `v_rnscale` (whose
  last two elements follow VEL/WIDTH_DATA_TOVER). "T"/"F" strings decode to `Option<bool>`. Helpers give site
  latitude and longitude in decimal degrees and the manual setup binary angles in degrees.
- The ICD assigns bytes 8828-8843 to one Real*4 (BASELINE_ZDR_OFFSET); only the first four bytes are read.
- Reassigned locations since Build 10: the default VCP tables (bytes 1328-8359) are spare from Build 18, and
  DIG_RCVR_CLOCK_FREQ and COHO_FREQ (2500-2515) appear in files from Build 23.1. K1/K3 became the pre-limit
  angles, and the pedestal/DAU regulation limits became dead limits and SPIP limits, in Build 17. The noise
  temperature maintenance limits (Real*4) became H/V_MIN_NOISETEMP (Integer*4). The module documentation lists
  them; the VCP tables are not decoded.
- The legacy RDA's 9600-byte message 18 is documented by no available ICD revision (2620002B lists type 18 as
  reserved), so the walker yields it unparsed.
- Verified: each of the 320 MetPy locations (without the VCP tables) whose location and type match a Build 24.0
  field matches exactly in all 20 ORDA volumes. The 31 MetPy-only and 49 Build-24.0-only offsets are fixed
  lists. The Build-24.0-only fields are checked against KIWA 2026 hex values and ICD ranges from Build 19.0 on.
  REFINED_PARK is zero before Build 21.0. Two blocked sites exceed the ICD's 1.800 maximum for H_RNSCALE and
  V_RNSCALE (KMAF 2023: 1.981 and 2.059; KMTX 2024: 2.539 and 2.233). The site position matches the message 31
  volume data block of the same volume.

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
