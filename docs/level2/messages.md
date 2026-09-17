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

Module: `vcp.rs` (`VolumeCoveragePattern`, one `VcpCut` per elevation cut). Status: message 5 **verified**;
message 7 shares the decoder and has **no real sample** (it goes from the RPG to the RDA).
Real samples: Message 5 in metadata records from 2005 on (not in KVWX 2008), including TDWR.

- Verified: `tests/messages_vcp.rs` compares every field with MetPy 1.7.1 `Level2File.vcp_info` on 22 metadata
  records (Build 10.0 to 24.1, TDWR VCP 80, the KIWA real-time start chunk). The goldens are
  `testdata/level2/golden/vcp/<id>.json`, written by `tools/level2_golden.py vcp`. The script maps MetPy's decoded
  names back to codes with MetPy's own tables. A second test checks the halfword 10 SAILS, MRLE, MPDA and base tilt
  flags against the per-cut E15 flags and elevations, and against the manifest tags. The message 7 test relabels
  a real message 5 frame, so it checks the dispatch only, not a real message 7.
- Layout: halfword 1 counts the body only (11 + 23 per cut in every sample; the message header size is 8 more).
  Cuts are read with a stride of (halfword 1 - 11) / cuts, which must be a whole number of at least 23.
- Angles: elevation (E1) and EBC (E19) codes above 90 degrees are negative, per the Table III-A note. Angle and
  azimuth rate codes are decoded from all 16 bits, like MetPy, Py-ART and xradar; the ICD marks bits 0-2 not
  applicable, and they are clear in every sample.
- Super resolution (E3) bits follow Build 24.0: bit 0 0.5 degree azimuth, bit 1 1/4 km reflectivity, bit 2 Doppler
  to 300 km, bit 3 dual polarization to 300 km. MetPy 1.7.1 names bits 1 and 2 differently. In dual-polarization
  volumes, split-cut surveillance cuts carry 11 and their Doppler partners 7, which fits the Build 24.0 names.
- Halfword 10 bit 10 is "MPDA cuts added" in the Table XI body, while note 16 calls bits 8-10 spare. It is exposed
  as `mpda_cuts_added()` and is clear in every sample.

Quirks found in the real corpus:

- **KLIX 2005-08-29.** The Message 5 frame declares 1208 halfwords, but every body byte is zero. The decoder returns
  an error (`VCP message size is 0`). MetPy skips the message.
- **Builds 10.0 to 16.1** (2008-2016 files). The VCP version is 0, and halfword 10 is 0 even in SAILS volumes
  (KOAX 2014, KEWX 2016). The per-cut E15 word is 0 as well.
- **KDGX 2023-03-25.** Halfword 10 is 0x5005: SAILS x2, base tilt VCP, and 2 base tilts in bits 13-15, although
  note 16 says only one base tilt is supported. The base tilt flag (E15 bit 10) is set on 3 split-cut pairs at
  0.31 degrees, 2 of them SAILS. EBC angles are -0.088 and -0.132 degrees (codes 65520 and 65512).
- **KDVN 2020-08-10.** Halfword 9 (sequencing) is 0x47: 7 elevations and up to 2 SAILS cuts, with the sequence
  not active.
- **KMTX 2024-03-01.** The base tilt split cut is commanded at 0.0 degrees.

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

Module: `prf.rs` (`RdaPrfData`; `surveillance_prf_hz` and `doppler_prf_hz` resolve a `VcpCut`'s PRF numbers).
Status: **verified**.
Real samples: PAHG 2025 (Build 23.1), KILX 2026, KIWA 2026 (Build 24.1, including the committed start
chunk).

- Layout: each waveform section is variable length. It holds the waveform type, a count N, then N 32-bit PRFs in
  mHz. Every sample has 3 sections of 8 PRFs, for waveforms 1, 2 and 5 (56 body halfwords). The PAHG and KILX
  waveform 5 tables are identical; KIWA's differs.
- Verified: MetPy, Py-ART and xradar do not decode this message. `tests/messages_vcp.rs` checks exact values against
  the message bytes, which are quoted in the tests. It also takes, for every sweep of the three volumes, the PRF
  that messages 5 and 32 select (waveform 1 table for surveillance; waveform 2 table for waveforms 2, 3 and 4, per
  note 1). It checks that PRF against the unambiguous range MetPy reads from the Message 31 radial blocks: c / (2
  PRF) must be within 0.5%, and every other PRF in the table must be more than 3% off. The radial blocks hold whole
  km. For Doppler sweeps they equal c / (2 PRF) rounded up; for surveillance sweeps they are 1.30-1.33 km above it.
- The waveform 5 (staggered pulse pair) table is verified against the bytes only, because no corpus VCP has an SPP
  cut.

## Message 33: RDA Log Data (Table XVIV)

Module: `rda_log.rs`. Status: decoded per ICD, and compressed log data is inflated with pure-Rust decoders
(gzip, bzip2, the first member of a ZIP). Inflated data is capped at 64 MiB. **No real sample.**
The message is not recorded in Archive II files. Table XVIV numbers halfwords from 0, so the data starts
at body byte 68. A mutation test relabels a real Message 32 frame as 33 and checks that the decoder's error
does not stop the walk. That test checks the walker's error handling, not the ICD layout.
