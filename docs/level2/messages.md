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
