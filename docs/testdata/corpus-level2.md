# Level II test corpus

This page covers the NEXRAD Level II part of the real test corpus (plan task TD.1). The entries are in
`testdata/level2/manifest.toml`: 29 archive objects (`format = "nexrad-level2"`) and one complete real-time
chunk volume of 70 chunks (`format = "nexrad-level2-chunk"`, `ephemeral = true`). Three chunks are
committed under `testdata/files/level2-chunks/` (502,179 bytes). Everything else is downloaded on first use
and verified by sha256. The downloads total 248,383,361 bytes.

Cached copies are named by manifest id in the shared cache
(`%LOCALAPPDATA%\recast-radar-tools\testdata` on this machine). The chunk URLs expire, so the cache holds
the only copies of the 67 chunks that are not committed. Every other URL is a permanent object in the public
`unidata-nexrad-level2` bucket.

## How the corpus was built and checked

- **Keys exist.** Every key was found with S3 ListObjectsV2
  (`https://<bucket>.s3.amazonaws.com/?list-type=2&prefix=...`). The listed `Size` equals the size of the
  downloaded file for all 99 entries. The chunk keys were listed in `unidata-nexrad-level2-chunks`
  (`KIWA/307/`) on 2026-09-17 around 00:56Z.
- **Hashes.** sha256 and size come from the downloaded bytes. The cache files were hashed again against the
  manifest (99/99 match). `cargo test -p recast-radar-testdata` passes with this manifest. That run includes
  `committed_paths_exist_and_hash_match`, which checks the three committed chunks.
- **Structure.** Every file was decoded by a separate Python inspector written for this task (not the Rust
  decoder). The inspector records:
  - the volume header string and ICAO field
  - whether the file is gzip, LDM bzip2 records, or raw records
  - message-type counts and Message 2 RDA-build raw values
  - the Message 5 VCP and supplemental bits (SAILS, MRLE, MPDA, base tilt)
  - Message 31 VOL/ELV/RAD block sizes and versions, moment names, gate counts and word sizes, and radial
    counts per elevation
  - lowest-sweep reflectivity statistics
- **Regime.** The weather in each regime volume was checked with plots of lowest-sweep REF, VEL and RHOHV
  from Py-ART. The hail and snow volumes were also checked with numbers:
  - KEWX: count of gates with Z >= 60 dBZ, RHOHV < 0.93 and |ZDR| < 1 dB
  - KMTX: median ZDR over gates with Z > 15 dBZ and RHOHV > 0.97
- **Readers.** Py-ART 2.2.5 `read_nexrad_archive` and MetPy 1.7.1 `Level2File` were run on every archive
  file and on sample chunks (table below).

## Era, format and build coverage

`trim` marks the 16 candidates for trimmed committed fixtures (TD.3). Builds are the Message 2 RDA-build
halfword read the way MetPy reads it (raw/100, or raw/10 when raw/100 <= 2).

| id | header | build | message layout | VCP | trim |
|---|---|---|---|---|---|
| l2-ktlx-19910605-162126 | ARCHIVE2.001, ICAO = 4 spaces | - (raw 0) | gzip of raw 2432-byte records; Message 1 + 2 only | 21 | yes |
| l2-ktlx-19990503-230052 | ARCHIVE2.022, ICAO NUL | - | truncated: 3 Message 2 + 68 Message 1 radials | 11 | |
| l2-ktlx-19990504-002218 | ARCHIVE2.036, ICAO NUL | - | Message 1 + 2 | 11 | yes |
| l2-ktlx-20030508-221041 | ARCHIVE2.224, ICAO NUL | - | first record is message type 202 (18 halfwords), then Message 1 + 2 | 11 | yes |
| l2-klix-20050829-130035 | AR2V0001.029 | - (raw 0) | metadata record (15 x62 segments, 13 x48, 18, 3, 5, 2) + Message 1 | 121 | yes |
| l2-kvwx-20080415-235337 | AR2V0001.737 | raw 1996 | Message 31 despite AR2V0001; no Message 5; 105 type-0 records | 32 | |
| l2-kpah-20080415-235014 | AR2V0004.171 | 10.0 | Message 31, 1 deg, REF 1 km gates; VOL 44 v1.0, RAD 20 | 32 | |
| l2-kdmx-20080525-205148 | AR2V0003 | 10.0 | Message 31 super-res; VOL 44 v1.0, RAD 20; REF/VEL/SW | 212 | yes |
| l2-kvnx-20110315-000203 | AR2V0006.734 | 12.0 | dual-pol; ZDR/RHO 8-bit, PHI 16-bit | 32 | |
| l2-ktlx-20130520-201643 | AR2V0006.939 | 13.2 | dual-pol; VOL 44 v1.0, RAD 20 | 12 | yes |
| l2-kgwx-20130601-235640 | AR2V0007.783 | 13.1 | dual-pol at 1 deg; first gate 125 m | 212 | |
| l2-koax-20140616-205305 | AR2V0006.689 | 14.0 | VOL 44 v2.0, RAD 28; SAILS cut with Message 5 supplemental = 0 | 212 | yes |
| l2-kewx-20160413-022531 | AR2V0006.344 | 16.1 | gzip archive object; SAILS | 212 | yes |
| l2-kdvn-20200810-180401 | AR2V0006 | 18.2 | uncompressed file of LDM bzip2 records; Message 13; ZDR 8-bit; MESO-SAILS x2 | 212 | yes |
| l2-klix-20210829-180425 | AR2V0006 | 19.1 | CFP moment; ZDR 16-bit; no Message 13; MPDA + SAILS | 112 | yes |
| l2-klix-20210829-175748-mdm | none | - | `_MDM` file: one LDM record, one Message 29 (0xFFFF size form) | - | |
| l2-kbox-20220129-150537 | AR2V0006 | 20.1 | VOL 52 v3.0 | 215 | yes |
| l2-tjua-20220918-190621 | AR2V0006.340 | 20.1 | VOL 52; SAILS | 215 | |
| l2-kdgx-20230325-010651 | AR2V0006.559 | 21.1 | base-tilt flag (0.31 deg split cut), SAILS x2, AVSET | 212 | |
| l2-kmaf-20230331-230843 | AR2V0006.639 | 21.0 | long-pulse clear-air VCP | 31 | |
| l2-tstl-20230331-230314 | AR2V0008.300 | raw 20 | TDWR: 1 deg radials, REF-only 300 m long-range cut, 150 m Doppler cuts; only Message 2 and 5 metadata | 80 | yes |
| l2-pgua-20230524-030945 | AR2V0006.679 | 21.1 | MESO-SAILS x3 | 212 | yes |
| l2-tbwi-20230601-175101-stub | AR2V0008.718 | raw 20 | 311 bytes: header + 3 LDM records of Message 2, no radials | - | |
| l2-kmtx-20240301-212827 | AR2V0006.228 | 22.0 | base-tilt split cut commanded at 0.0 deg | 215 | yes |
| l2-ktlx-20240315-000217 | AR2V0006.626 | 22.0 | MESO-SAILS x3, AVSET (20 of 23 cuts) | 212 | yes |
| l2-ktlx-20240515-000014 | AR2V0006.126 | 22.1 | clear air | 35 | |
| l2-pahg-20250909-212549 | AR2V0006.304 | 23.1 | Message 32 | 215 | |
| l2-kilx-20260418-013553 | AR2V0006.862 | 23.1 | Message 32; MRLE x3 | 212 | yes |
| l2-kiwa-20260917-003629 | AR2V0006.307 | 24.1 | Message 32; SAILS; AVSET; same bytes as the 70 chunks | 215 | |

Benchmark files (`bench` tag): the three plan volumes (KTLX 2013-05-20, KTLX 2024-03-15, KILX 2026-04-18),
plus the legacy Message 1 volume from spec section 7 (KTLX 1999-05-04 00:22Z).

## Weather regimes

| regime | id | what the volume shows |
|---|---|---|
| hurricane (legacy) | l2-klix-20050829-130035 | Katrina eye ~60 km S; eyewall and aliased velocity close to the radar |
| hurricane | l2-klix-20210829-180425 | Ida eye ~140 km SW; full eyewall ring in range |
| hurricane, non-CONUS | l2-tjua-20220918-190621 | Fiona eye ~100 km W at landfall |
| typhoon, non-CONUS | l2-pgua-20230524-030945 | Mawar eyewall over northern Guam (eye ~45 km NE); beam-blockage wedge |
| winter storm (snow) | l2-kbox-20220129-150537 | blizzard snow bands; median ZDR 0.1 dB over Z > 15 dBZ, RHOHV > 0.97 gates |
| derecho | l2-kdvn-20200810-180401 | bow echo 100-150 km W; 99 gates >= 60 dBZ within 120 km on the lowest sweep |
| hail | l2-kewx-20160413-022531 | San Antonio hailstorm: 259 gates >= 60 dBZ, 62 >= 65 dBZ, 39 with RHOHV < 0.93 and abs(ZDR) < 1 dB (lowest sweep, 120 km) |
| tornado | l2-ktlx-19990504-002218, l2-ktlx-20030508-221041, l2-ktlx-20130520-201643, l2-koax-20140616-205305, l2-kdgx-20230325-010651 | Moore 1999 / 2003 / 2013, Pilger 2014, Rolling Fork 2023 |
| clear air | l2-ktlx-20240515-000014 (VCP 35), l2-kmaf-20230331-230843 (VCP 31), l2-kvnx-20110315-000203, l2-kpah-20080415-235014, l2-kvwx-20080415-235337 (VCP 32) | no precipitation; ground clutter or biological returns |
| complex terrain (snow) | l2-kmtx-20240301-212827 | mountain-top site, 0.0 deg base tilt, snow bands (median ZDR -0.1 dB) |
| non-CONUS stratiform | l2-pahg-20250909-212549 | Kenai, Alaska: widespread stratiform precipitation |
| convective (other) | l2-ktlx-19910605-162126, l2-kdmx-20080525-205148, l2-kgwx-20130601-235640, l2-tstl-20230331-230314, l2-ktlx-20240315-000217, l2-kilx-20260418-013553, l2-kiwa-20260917-003629 | thunderstorms at various ranges |

## Trim candidates (16)

These 16 volumes cover every archive layout the trim tool must preserve:

| id | layout it covers |
|---|---|
| l2-ktlx-19910605-162126 | ARCHIVE2 with a blank ICAO, no metadata record |
| l2-ktlx-19990504-002218 | Message 1, VCP 11 |
| l2-ktlx-20030508-221041 | the type-202 record |
| l2-klix-20050829-130035 | AR2V0001 with segmented metadata, VCP 121 multi-cut |
| l2-kdmx-20080525-205148 | Build 10 super-res Message 31 |
| l2-ktlx-20130520-201643 | Build 13 dual-pol, RAD 20 |
| l2-koax-20140616-205305 | RAD 28 |
| l2-kewx-20160413-022531 | hail, gzip Build 16 |
| l2-kdvn-20200810-180401 | LDM records with Message 13 |
| l2-klix-20210829-180425 | CFP and MPDA |
| l2-kbox-20220129-150537 | VOL 52, snow |
| l2-tstl-20230331-230314 | TDWR |
| l2-pgua-20230524-030945 | non-CONUS, MESO-SAILS x3 |
| l2-kmtx-20240301-212827 | base tilt, terrain |
| l2-ktlx-20240315-000217 | bench |
| l2-kilx-20260418-013553 | Message 32, MRLE |

Notes for TD.3:

- **Two readers read every candidate.** Py-ART and MetPy both read all 16 in full-file form.
- **MetPy needs a `.gz` path for gzip files.** MetPy detects gzip only from a `.gz` file extension. The
  cache files have no extension, so a gzip archive read straight from the cache fails in MetPy with errors
  such as `data type '>u19' not understood`. Open those through `gzip.open`, or through a copy whose name
  ends in `.gz`.
- **Split cuts come first.** Where the first cut is a split cut, the first two sweeps form the surveillance
  and Doppler pair.
- **The lowest cuts differ.** The KTLX 1991 and 1999 legacy volumes start with a REF-only cut, then a
  VEL/SW cut at the same angle. TSTL (TDWR) starts with a REF-only long-range 0.26 deg sweep (300 m gates),
  then a 0.26 deg REF/VEL/SW sweep at 150 m.

## Real-time chunk capture (KIWA volume 307)

- **Capture.** A polling script listed `unidata-nexrad-level2-chunks` under the prefix `KIWA/307/` every
  ~4 s from 2026-09-17T00:35:04Z. It downloaded each new key as soon as it appeared. KIWA was picked
  because it had active thunderstorms at the time.
- **Late End chunk.** Chunks 001-069 were received 4.3-16.8 s after their S3 LastModified time (1 s
  resolution). LastModified trails the chunk's last radial time by 1.2-15.5 s. The End chunk (070-E,
  LastModified 00:41:50Z) was fetched at 00:46:33Z. The script's listing parser dropped the final
  key of each listing because the XML had no trailing newline, so the End chunk was only picked up by a
  manual fetch. The bytes are unaffected, since sha256 is taken from the object.
- **Completeness.** Keys are `KIWA/307/20260917-003629-NNN-{S,I,E}`, NNN = 001..070, with no gaps.
- **Identity with the archive.** Concatenating the 70 chunks in sequence order gives exactly the archive
  object `2026/09/17/KIWA/KIWA20260917_003629_V06` (entry `l2-kiwa-20260917-003629`): 12,653,945 bytes,
  sha256 `947a4372554d3b43f59c5fe5f49ce46cc032384485aec057499735b447bd2f45`.
- **Chunk contents.**
  - Start chunk: the 24-byte `AR2V0006.307` header and one LDM bzip2 record with the metadata messages
    (15 x5, 18 x4, 3, 5, 2, 32).
  - Every other chunk: exactly one LDM record of 120 Message 31 radials. Chunk 059 also carries 3 Message 2.
  - Sweeps of 720 radials (the 0.53/0.92/1.32 deg split-cut sweeps and the SAILS 0.53 deg pair) take 6
    chunks. Sweeps of 360 radials take 3.
- **Radial status markers.** Radial status 3 (beginning of volume) is in chunk 002, 5 (start of the last
  elevation) in chunk 068, and 4 (end of volume) in chunk 070.
- **VCP and AVSET.** VCP 215 lists 20 cuts. AVSET ended the volume after elevation 15 (8.0 deg).
- **Committed fixtures.** These are `files/level2-chunks/KIWA-307-20260917-003629-001-S`, `-002-I` and
  `-003-I`. On their own:
  - Py-ART cannot read any single chunk. The Start chunk has "No MSG31 records"; the intermediate chunks give
    "unknown compression record" (they start with an LDM record, not a volume header).
  - MetPy reads each intermediate chunk as 120 radials.
  - The three concatenated (S + I + I) read as 1 sweep / 240 radials in both Py-ART and MetPy.

Per-chunk timing, for the chunk timing model. Radial times and azimuths come from the Message 31 headers.
"received" is when the capture script finished downloading.

| # | type | bytes | S3 LastModified | received (local UTC) | elev # (deg) | radials | first radial | last radial | status |
|---|---|---:|---|---|---|---:|---|---|---|
| 1 | S | 2455 | 00:36:31 | 00:36:41.583 | metadata | 0 | - | - | - |
| 2 | I | 162173 | 00:36:37 | 00:36:46.535 | 1 (0.53) | 120 | 00:36:29.397 az 155.2 | 00:36:34.532 az 214.8 | 1,3 |
| 3 | I | 337551 | 00:36:42 | 00:36:51.568 | 1 (0.53) | 120 | 00:36:34.576 az 215.2 | 00:36:39.702 az 274.8 | 1 |
| 4 | I | 366778 | 00:36:47 | 00:36:56.596 | 1 (0.53) | 120 | 00:36:39.742 az 275.2 | 00:36:44.878 az 334.8 | 1 |
| 5 | I | 279908 | 00:36:52 | 00:37:01.639 | 1 (0.53) | 120 | 00:36:44.918 az 335.2 | 00:36:50.050 az 34.7 | 1 |
| 6 | I | 177861 | 00:36:57 | 00:37:06.631 | 1 (0.53) | 120 | 00:36:50.094 az 35.2 | 00:36:55.220 az 94.8 | 1 |
| 7 | I | 380987 | 00:37:02 | 00:37:11.820 | 1 (0.53) | 120 | 00:36:55.263 az 95.2 | 00:37:00.395 az 154.8 | 1,2 |
| 8 | I | 66306 | 00:37:07 | 00:37:12.303 | 2 (0.53) | 120 | 00:37:01.664 az 172.2 | 00:37:04.547 az 231.7 | 0,1 |
| 9 | I | 125773 | 00:37:09 | 00:37:17.482 | 2 (0.53) | 120 | 00:37:04.571 az 232.2 | 00:37:07.469 az 291.7 | 1 |
| 10 | I | 123597 | 00:37:12 | 00:37:18.055 | 2 (0.53) | 120 | 00:37:07.493 az 292.2 | 00:37:10.388 az 351.7 | 1 |
| 11 | I | 63918 | 00:37:15 | 00:37:23.078 | 2 (0.53) | 120 | 00:37:10.412 az 352.2 | 00:37:13.305 az 51.7 | 1 |
| 12 | I | 81280 | 00:37:18 | 00:37:23.579 | 2 (0.53) | 120 | 00:37:13.329 az 52.2 | 00:37:16.222 az 111.7 | 1 |
| 13 | I | 100323 | 00:37:21 | 00:37:28.729 | 2 (0.53) | 120 | 00:37:16.246 az 112.2 | 00:37:19.140 az 171.7 | 1,2 |
| 14 | I | 253598 | 00:37:26 | 00:37:34.026 | 3 (0.92) | 120 | 00:37:20.279 az 192.2 | 00:37:24.662 az 251.8 | 0,1 |
| 15 | I | 341611 | 00:37:31 | 00:37:39.345 | 3 (0.92) | 120 | 00:37:24.700 az 252.2 | 00:37:29.096 az 311.8 | 1 |
| 16 | I | 377442 | 00:37:35 | 00:37:44.696 | 3 (0.92) | 120 | 00:37:29.133 az 312.2 | 00:37:33.532 az 11.8 | 1 |
| 17 | I | 164123 | 00:37:40 | 00:37:49.940 | 3 (0.92) | 120 | 00:37:33.566 az 12.2 | 00:37:37.962 az 71.8 | 1 |
| 18 | I | 370055 | 00:37:44 | 00:37:55.262 | 3 (0.92) | 120 | 00:37:37.999 az 72.2 | 00:37:42.392 az 131.7 | 1 |
| 19 | I | 269376 | 00:37:49 | 00:37:55.868 | 3 (0.92) | 120 | 00:37:42.432 az 132.2 | 00:37:46.828 az 191.8 | 1,2 |
| 20 | I | 119115 | 00:37:53 | 00:38:01.294 | 4 (0.92) | 120 | 00:37:47.982 az 209.2 | 00:37:50.863 az 268.7 | 0,1 |
| 21 | I | 134224 | 00:37:55 | 00:38:01.925 | 4 (0.92) | 120 | 00:37:50.888 az 269.2 | 00:37:53.782 az 328.7 | 1 |
| 22 | I | 117914 | 00:37:58 | 00:38:07.327 | 4 (0.92) | 120 | 00:37:53.806 az 329.2 | 00:37:56.704 az 28.7 | 1 |
| 23 | I | 65284 | 00:38:02 | 00:38:07.751 | 4 (0.92) | 120 | 00:37:56.729 az 29.2 | 00:37:59.623 az 88.7 | 1 |
| 24 | I | 135034 | 00:38:05 | 00:38:13.001 | 4 (0.92) | 120 | 00:37:59.647 az 89.2 | 00:38:02.541 az 148.7 | 1 |
| 25 | I | 77602 | 00:38:07 | 00:38:18.177 | 4 (0.92) | 120 | 00:38:02.566 az 149.2 | 00:38:05.459 az 208.7 | 1,2 |
| 26 | I | 323424 | 00:38:12 | 00:38:18.725 | 5 (1.32) | 120 | 00:38:06.329 az 225.2 | 00:38:09.999 az 284.8 | 0,1 |
| 27 | I | 334679 | 00:38:15 | 00:38:24.107 | 5 (1.32) | 120 | 00:38:10.030 az 285.3 | 00:38:13.705 az 344.8 | 1 |
| 28 | I | 297346 | 00:38:20 | 00:38:29.741 | 5 (1.32) | 120 | 00:38:13.734 az 345.2 | 00:38:17.412 az 44.8 | 1 |
| 29 | I | 255521 | 00:38:24 | 00:38:30.386 | 5 (1.32) | 120 | 00:38:17.443 az 45.3 | 00:38:21.124 az 104.8 | 1 |
| 30 | I | 343146 | 00:38:27 | 00:38:36.600 | 5 (1.32) | 120 | 00:38:21.153 az 105.2 | 00:38:24.836 az 164.7 | 1 |
| 31 | I | 212012 | 00:38:32 | 00:38:42.123 | 5 (1.32) | 120 | 00:38:24.868 az 165.2 | 00:38:28.554 az 224.8 | 1,2 |
| 32 | I | 135914 | 00:38:35 | 00:38:42.735 | 6 (1.32) | 120 | 00:38:29.532 az 241.2 | 00:38:32.400 az 300.7 | 0,1 |
| 33 | I | 138239 | 00:38:39 | 00:38:43.257 | 6 (1.32) | 120 | 00:38:32.426 az 301.2 | 00:38:35.325 az 0.7 | 1 |
| 34 | I | 77381 | 00:38:41 | 00:38:48.549 | 6 (1.32) | 120 | 00:38:35.350 az 1.2 | 00:38:38.243 az 60.7 | 1 |
| 35 | I | 132871 | 00:38:43 | 00:38:49.039 | 6 (1.32) | 120 | 00:38:38.267 az 61.2 | 00:38:41.160 az 120.7 | 1 |
| 36 | I | 118310 | 00:38:46 | 00:38:54.774 | 6 (1.32) | 120 | 00:38:41.184 az 121.2 | 00:38:44.079 az 180.7 | 1 |
| 37 | I | 84753 | 00:38:49 | 00:39:00.290 | 6 (1.32) | 120 | 00:38:44.104 az 181.2 | 00:38:46.999 az 240.7 | 1,2 |
| 38 | I | 249168 | 00:38:57 | 00:39:11.209 | 7 (1.85) | 120 | 00:38:48.310 az 265.5 | 00:38:55.266 az 24.5 | 0,1 |
| 39 | I | 216307 | 00:39:08 | 00:39:22.297 | 7 (1.85) | 120 | 00:38:55.324 az 25.5 | 00:39:02.278 az 144.5 | 1 |
| 40 | I | 194402 | 00:39:20 | 00:39:33.374 | 7 (1.85) | 120 | 00:39:02.338 az 145.5 | 00:39:09.295 az 264.6 | 1,2 |
| 41 | I | 371754 | 00:39:31 | 00:39:39.938 | 8 (0.53) | 120 | 00:39:10.407 az 281.2 | 00:39:15.533 az 340.8 | 0,1 |
| 42 | I | 254609 | 00:39:32 | 00:39:40.815 | 8 (0.53) | 120 | 00:39:15.574 az 341.2 | 00:39:20.709 az 40.8 | 1 |
| 43 | I | 197436 | 00:39:34 | 00:39:41.432 | 8 (0.53) | 120 | 00:39:20.753 az 41.2 | 00:39:25.882 az 100.8 | 1 |
| 44 | I | 370586 | 00:39:35 | 00:39:48.182 | 8 (0.53) | 120 | 00:39:25.922 az 101.3 | 00:39:31.051 az 160.8 | 1 |
| 45 | I | 162309 | 00:39:38 | 00:39:48.873 | 8 (0.53) | 120 | 00:39:31.095 az 161.2 | 00:39:36.224 az 220.7 | 1 |
| 46 | I | 336508 | 00:39:43 | 00:39:55.339 | 8 (0.53) | 120 | 00:39:36.270 az 221.3 | 00:39:41.396 az 280.8 | 1,2 |
| 47 | I | 123258 | 00:39:47 | 00:39:56.142 | 9 (0.53) | 120 | 00:39:42.752 az 299.2 | 00:39:45.638 az 358.7 | 0,1 |
| 48 | I | 51518 | 00:39:50 | 00:39:56.666 | 9 (0.53) | 120 | 00:39:45.663 az 359.2 | 00:39:48.563 az 58.7 | 1 |
| 49 | I | 91116 | 00:39:53 | 00:40:03.144 | 9 (0.53) | 120 | 00:39:48.587 az 59.2 | 00:39:51.481 az 118.7 | 1 |
| 50 | I | 92113 | 00:39:56 | 00:40:09.391 | 9 (0.53) | 120 | 00:39:51.505 az 119.2 | 00:39:54.401 az 178.7 | 1 |
| 51 | I | 72425 | 00:40:02 | 00:40:10.059 | 9 (0.53) | 120 | 00:39:54.425 az 179.2 | 00:39:57.324 az 238.7 | 1 |
| 52 | I | 127664 | 00:40:05 | 00:40:21.835 | 9 (0.53) | 120 | 00:39:57.347 az 239.2 | 00:40:00.238 az 298.7 | 1,2 |
| 53 | I | 183525 | 00:40:18 | 00:40:27.819 | 10 (2.46) | 120 | 00:40:01.659 az 328.6 | 00:40:07.263 az 87.5 | 0,1 |
| 54 | I | 190813 | 00:40:22 | 00:40:34.166 | 10 (2.46) | 120 | 00:40:07.311 az 88.5 | 00:40:12.910 az 207.6 | 1 |
| 55 | I | 233064 | 00:40:29 | 00:40:40.013 | 10 (2.46) | 120 | 00:40:12.958 az 208.5 | 00:40:18.558 az 327.6 | 1,2 |
| 56 | I | 163034 | 00:40:33 | 00:40:40.520 | 11 (3.12) | 120 | 00:40:19.468 az 346.5 | 00:40:25.396 az 105.5 | 0,1 |
| 57 | I | 142902 | 00:40:36 | 00:40:46.409 | 11 (3.12) | 120 | 00:40:25.446 az 106.5 | 00:40:31.377 az 225.6 | 1 |
| 58 | I | 219137 | 00:40:40 | 00:40:52.881 | 11 (3.12) | 120 | 00:40:31.425 az 226.6 | 00:40:37.354 az 345.6 | 1,2 |
| 59 | I | 137838 | 00:40:46 | 00:40:59.078 | 12 (4.04) +3 msg 2 | 120 | 00:40:38.339 az 5.5 | 00:40:44.056 az 124.5 | 0,1 |
| 60 | I | 120720 | 00:40:51 | 00:41:05.269 | 12 (4.04) | 120 | 00:40:44.104 az 125.5 | 00:40:49.825 az 244.6 | 1 |
| 61 | I | 187777 | 00:40:58 | 00:41:11.325 | 12 (4.04) | 120 | 00:40:49.873 az 245.5 | 00:40:55.591 az 4.5 | 1,2 |
| 62 | I | 116933 | 00:41:06 | 00:41:17.325 | 13 (5.14) | 120 | 00:40:56.648 az 26.5 | 00:41:02.365 az 145.5 | 0,1 |
| 63 | I | 107349 | 00:41:11 | 00:41:23.313 | 13 (5.14) | 120 | 00:41:02.413 az 146.5 | 00:41:08.130 az 265.6 | 1 |
| 64 | I | 120317 | 00:41:16 | 00:41:29.322 | 13 (5.14) | 120 | 00:41:08.178 az 266.5 | 00:41:13.896 az 25.6 | 1,2 |
| 65 | I | 101669 | 00:41:22 | 00:41:35.280 | 14 (6.46) | 120 | 00:41:15.001 az 48.5 | 00:41:20.719 az 167.5 | 0,1 |
| 66 | I | 103265 | 00:41:29 | 00:41:41.315 | 14 (6.46) | 120 | 00:41:20.767 az 168.5 | 00:41:26.484 az 287.5 | 1 |
| 67 | I | 80991 | 00:41:34 | 00:41:47.581 | 14 (6.46) | 120 | 00:41:26.531 az 288.5 | 00:41:32.251 az 47.5 | 1,2 |
| 68 | I | 120857 | 00:41:40 | 00:41:48.130 | 15 (8.0) | 120 | 00:41:33.267 az 70.5 | 00:41:37.884 az 189.5 | 1,5 |
| 69 | I | 152619 | 00:41:45 | 00:41:54.283 | 15 (8.0) | 120 | 00:41:37.922 az 190.5 | 00:41:42.551 az 309.5 | 1 |
| 70 | E | 112078 | 00:41:50 | 00:46:33.420 | 15 (8.0) | 120 | 00:41:42.589 az 310.5 | 00:41:47.217 az 69.5 | 1,4 |

## Reader compatibility

| id | Py-ART 2.2.5 read_nexrad_archive | MetPy 1.7.1 Level2File |
|---|---|---|
| l2-kbox-20220129-150537 | 18 sweeps / 8640 rays / 7 fields | 18 sweeps / 8640 radials |
| l2-kdgx-20230325-010651 | 21 sweeps / 11880 rays / 7 fields | 21 sweeps / 11880 radials |
| l2-kdmx-20080525-205148 | 17 sweeps / 8280 rays / 3 fields | 17 sweeps / 8280 radials (path must end in .gz) |
| l2-kdvn-20200810-180401 | 21 sweeps / 11160 rays / 6 fields | 21 sweeps / 11160 radials |
| l2-kewx-20160413-022531 | 19 sweeps / 9720 rays / 6 fields | 19 sweeps / 9720 radials (path must end in .gz) |
| l2-kgwx-20130601-235640 | 16 sweeps / 5760 rays / 6 fields | 16 sweeps / 5760 radials (path must end in .gz) |
| l2-kilx-20260418-013553 | 23 sweeps / 12600 rays / 7 fields | 23 sweeps / 12600 radials |
| l2-kiwa-20260917-003629 | 15 sweeps / 8280 rays / 7 fields | 15 sweeps / 8280 radials |
| l2-klix-20050829-130035 | 20 sweeps / 7252 rays / 3 fields | 20 sweeps / 7252 radials (path must end in .gz) |
| l2-klix-20210829-175748-mdm | error: OSError: unknown compression record | 0 sweeps / 0 radials |
| l2-klix-20210829-180425 | 23 sweeps / 12600 rays / 7 fields | 23 sweeps / 12600 radials |
| l2-kmaf-20230331-230843 | 8 sweeps / 5040 rays / 7 fields | 8 sweeps / 5040 radials |
| l2-kmtx-20240301-212827 | 20 sweeps / 10080 rays / 7 fields | 20 sweeps / 10080 radials |
| l2-koax-20140616-205305 | 19 sweeps / 9720 rays / 6 fields | 19 sweeps / 9720 radials (path must end in .gz) |
| l2-kpah-20080415-235014 | 7 sweeps / 2520 rays / 3 fields | 7 sweeps / 2520 radials (path must end in .gz) |
| l2-ktlx-19910605-162126 | 11 sweeps / 4017 rays / 3 fields | 11 sweeps / 4017 radials (path must end in .gz) |
| l2-ktlx-19990503-230052 | 1 sweeps / 68 rays / 1 fields | 1 sweeps / 68 radials (path must end in .gz) |
| l2-ktlx-19990504-002218 | 16 sweeps / 5855 rays / 3 fields | 16 sweeps / 5855 radials (path must end in .gz) |
| l2-ktlx-20030508-221041 | 16 sweeps / 5856 rays / 3 fields | 16 sweeps / 5856 radials (path must end in .gz) |
| l2-ktlx-20130520-201643 | 17 sweeps / 8280 rays / 6 fields | 17 sweeps / 8280 radials (path must end in .gz) |
| l2-ktlx-20240315-000217 | 20 sweeps / 11520 rays / 7 fields | 20 sweeps / 11520 radials |
| l2-ktlx-20240515-000014 | 12 sweeps / 6480 rays / 7 fields | 12 sweeps / 6480 radials |
| l2-kvnx-20110315-000203 | 7 sweeps / 3960 rays / 6 fields | 7 sweeps / 3960 radials (path must end in .gz) |
| l2-kvwx-20080415-235337 | 7 sweeps / 2500 rays / 3 fields | 7 sweeps / 2500 radials (path must end in .gz) |
| l2-pahg-20250909-212549 | 18 sweeps / 8640 rays / 7 fields | 18 sweeps / 8640 radials |
| l2-pgua-20230524-030945 | 23 sweeps / 12600 rays / 7 fields | 23 sweeps / 12600 radials |
| l2-tbwi-20230601-175101-stub | error: ValueError: No MSG31 records found, cannot read file | 0 sweeps / 0 radials |
| l2-tjua-20220918-190621 | 20 sweeps / 10080 rays / 7 fields | 20 sweeps / 10080 radials |
| l2-tstl-20230331-230314 | 23 sweeps / 8280 rays / 3 fields | 23 sweeps / 8280 radials |
| l2chunk-kiwa-307-20260917-003629-001-s | error: ValueError: No MSG31 records found, cannot read file | 0 sweeps / 0 radials |
| l2chunk-kiwa-307-20260917-003629-002-i | error: OSError: unknown compression record | 1 sweeps / 120 radials |
| l2chunk-kiwa-307-20260917-003629-003-i | error: OSError: unknown compression record | 1 sweeps / 120 radials |
| l2chunk-kiwa-307-20260917-003629-070-e | error: OSError: unknown compression record | 15 sweeps / 120 radials |
| chunks 001-s + 002-i + 003-i concatenated | 1 sweep / 240 rays / 5 fields | 1 sweep / 240 radials |

## Format timeline

These facts come from the survey used to pick the volumes (the first volume of 15 May each year at KTLX, and
all sites on sample days). They are observations of the archive, not ICD statements.

- **1991-2000.** `ARCHIVE2.nnn` headers with a blank or NUL ICAO, gzip of raw Message 1 records.
- **2001-2004.** `ARCHIVE2.000`/`ARCHIVE2.nnn` with a NUL ICAO, where a type-202 record opens the file
  (seen in the first KTLX volumes of 2002-05-15 and 2004-05-15, and in 2003-05-08 22:10Z).
- **2005-2008.** `AR2V0001.nnn` with the ICAO, and a metadata record (15, 13, 18, 3, 5, 2) before the
  Message 1 radials.
- **2008.** Message 31 appears:
  - NOP3 was already `_V03` on 2008-01-15.
  - Operational sites switched between April and June (KMPX `_V03` on 2008-04-15, KTLX on 2008-06-12).
  - `AR2V0003` is 0.5 deg super-res. `AR2V0004` is 1 deg Message 31.
  - `_V01` Message 31 files exist (KVWX).
- **2011-2016.** `AR2V0006` dual-pol (KVNX `_V06` on 2011-03-15). A separate group of sites, mostly DoD- and
  FAA-operated, wrote non-super-res files:
  - `_V04` on 2011-06-01: KBBX, KDFX, KDOX, KEVX, KFDX, KGRK, KGWX, KHDX, KJGX, KTYX, PHKI, PHKM, PHMO,
    PHWA, TJUA, FOP1.
  - `_V07` dual-pol on 2013-06-01: KBBX, KDFX, KDOX, KEVX, KGWX, KHDX, KJGX, KTYX, PHKI, PHMO, PHWA.
  - `_V07` on 2016-06-01: FOP1, PHKI, PHKM, PHMO, PHWA.
  - KGWX 2013 is at 1 deg.
- **Block layouts.**
  - RAD 20 -> 28 and VOL v1.0 -> v2.0 by Build 14.0 (KTLX 2014-05-15, KOAX 2014-06-16).
  - VOL 44 -> 52 (v3.0) between Build 19.1 (KLIX 2021-08-29) and Build 20.1 (KTLX 2022-05-15, KBOX
    2022-01-29).
  - CFP moment and no Message 13 from Build 19.0 (KTLX 2021-05-15).
  - ZDR 8-bit in the Build 12.0-18.2 volumes here, 16-bit from Build 19.1.
  - Message 32 from Build 23.0 (KTLX 2025-05-15).
- **Archive compression.** Archive objects changed from gzip (`_V06.gz`, raw records) to uncompressed files
  of LDM bzip2 records during 2016. On 2016-06-01 the last object of the day was `_V06` at 153 sites and
  `_V06.gz` at 1.
- **Message 5 supplemental flags.** Before Build 18, SAILS cuts are present with the supplemental halfword
  = 0 (KOAX 2014 Build 14.0, KEWX 2016 Build 16.1). From Build 18.2 on, the SAILS/MRLE/MPDA/base-tilt bits
  are set.
- **TDWR.** TDWR Level II (`_V08`, `AR2V0008`) is in the same bucket from 2020. Counting TDWR sites on
  1 June: 0 in 2019, 3 in 2020 (TDAL, TDFW, TOKC), 43 in 2021, 44 in 2022. VCPs seen are 80 and 90.
- **Other object types.** `_MDM` model-data objects sit beside volumes about hourly. Odd objects exist, such
  as `TBWI20230601_175101_V08.008` (311 bytes).

## Differences from the plan text

- **Legacy header name.** The plan says "AR2V0001 legacy (1991-1995)". The 1991-2004 files in the bucket
  actually use `ARCHIVE2.nnn` headers, and `AR2V0001` first appears in 2005. The earliest volume
  (1991-06-05 KTLX) is included, and `AR2V0001` is covered by 2005 Katrina and the 2008 KVWX oddity.
- **Clear-air VCP 215.** No pure clear-air VCP 215 volume was added. VCP 215 appears with precipitation in
  KBOX, TJUA, KMTX, PAHG and KIWA. The clear-air entries are VCP 35 (KTLX 2024-05-15), VCP 31 (KMAF) and
  VCP 32 (KVNX, KPAH, KVWX).
- **Hail volume.** Hail uses KEWX 2016-04-13 02:25Z, chosen by the hail-signature metric. Three KFTG
  2017-05-08 volumes were rejected: at most 12 gates >= 60 dBZ and no hail signature.
- **PAHG volume.** PAHG uses 2025-09-09 21:25Z. A 2024-01-15 volume was rejected because its echoes were
  non-meteorological.
- **Extra entries.** These go beyond the plan list: the truncated 1999 file, the 2003 type-202 file, KVWX,
  KPAH `_V04`, KGWX `_V07`, KVNX early dual-pol, the `_MDM` file, TDWR TSTL, the TBWI stub, and the KIWA
  archive twin of the chunk volume.
- **One chunk volume only.** The chunk capture covers one volume, as the plan asks. There is no second site.

## Tag vocabulary

| tag | meaning |
|---|---|
| `era:YYYY` | year of the volume |
| `site:xxxx` | radar site |
| `radar:wsr-88d` / `radar:tdwr` | radar type |
| `header:<tape>` | volume header string, without the extension number |
| `icao:blank` / `icao:nul` | ICAO field is spaces or NUL bytes |
| `compression:gzip` / `compression:ldm-bzip2`, `records:raw` | outer container and record style |
| `build:NN.N` | RDA build from Message 2 |
| `msg:N` | message types present; `segmented:N` means the metadata message spans several segments |
| `vcp:N` | VCP number |
| `sails` / `meso-sails:N` / `mrle:N` / `mpda` / `base-tilt:N` / `avset` | scan strategy features |
| `vol-block:44\|52`, `rad-block:20\|28` | Message 31 VOL and RAD block sizes |
| `zdr:8bit\|16bit`, `moment:CFP`, `dualpol` | moment encoding |
| `res:super` / `res:legacy` | 0.5 deg or 1 deg lowest-sweep azimuth spacing |
| `first-gate:125m` | unusual range to the first gate |
| `regime:*` | weather regime |
| `edge:*` | edge case: truncated, header-version-mismatch, no-volume-header, size-0xffff, status-only |
| `file:mdm`, `no-msg5`, `no-metadata-record` | file-type oddities |
| `bench` | benchmark corpus |
| `trim` | trimmed-fixture candidate |
| `provider:aws`, `bucket:*` | source |
| `chunk-volume:kiwa-307`, `chunk:start\|intermediate\|end`, `elev:N`, `radial-status:N` | real-time chunk facts |
