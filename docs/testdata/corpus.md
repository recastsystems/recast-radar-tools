# Real test corpus

Tests in recast-radar-tools read real radar files. This page describes that corpus: what each file covers,
where it came from, how derived files were made, how the files were checked, and what is still missing.
The last section is generated from the manifests. It lists every entry and indexes every entry by tag.

The corpus is defined by `testdata/manifest.toml` plus every `testdata/*/manifest.toml`, and the
`recast-radar-testdata` crate serves it. Small files are committed under `testdata/files/`. Full volumes
and archives are downloaded on first use, checked against their sha256 and cached.

## Using the corpus

- `recast_radar_testdata::require_file!("<id>")` returns the path of a verified copy. When the file cannot
  be fetched (no network, `RECAST_RADAR_TESTDATA_OFFLINE` set, or an expired ephemeral URL), it prints the
  reason and returns from the test, so the test is skipped. Other failures panic: an unknown id, an HTTP
  error on a permanent URL, or a sha256 mismatch.
- `path(id)` and `bytes(id)` return a `Result`. `local_path(id)` never touches the network.
- `ids_with_tag(tag)` returns ids in manifest order, for tests that run over a class of files (for example
  `trimmed` or `object:pvol`). Tags are case-sensitive.
- Committed files are hashed on first use and are never downloaded.
- The download cache is the first of these that is set: `$RECAST_RADAR_TESTDATA`,
  `%LOCALAPPDATA%\recast-radar-tools\testdata` (Windows), `$XDG_CACHE_HOME/recast-radar-tools/testdata`,
  `$HOME/.cache/recast-radar-tools/testdata`, otherwise `<workspace>/.testdata-cache`. Cached files are
  named by manifest id, and all worktrees share the cache.
- Level II trim tool:
  `cargo run -p recast-radar-testdata --bin trim-level2 -- (INPUT | --id ID) OUTPUT [--sweeps N] [--max-radials N | --max-bytes N]`.

### Adding or changing an entry

1. Add a `[[file]]` table to the manifest for the format. The schema is in the comment at the top of
   `testdata/manifest.toml`, and unknown keys are rejected.
2. Take `sha256` and `size` from the bytes you downloaded. Put committed files under `testdata/files/` and
   keep them small. Trimmed Level II files are capped at 2 MB each, and committed testdata at 60 MB in
   total (`tests/trim.rs` checks both).
3. For a derived file, name the source in `derived_from` when it is a manifest entry, describe the steps
   in `derivation`, and add a recipe to this page.
4. Regenerate the index at the end of this page with
   `RECAST_RADAR_TESTDATA_BLESS=1 cargo test -p recast-radar-testdata --test corpus_doc`. Without the
   variable, that test fails whenever the index is out of date.
5. Run `cargo test -p recast-radar-testdata`.

## Layout

| path | contents |
|---|---|
| `testdata/manifest.toml` | schema comment only, no entries |
| `testdata/level2/manifest.toml` | Level II archive volumes, the KIWA real-time chunk volume, trimmed fixtures |
| `testdata/other/manifest.toml` | ODIM_H5, CfRadial 1 and 2, DORADE, JMA GRIB2 tars, one Level III VWP, and the archives some of them came from |
| `testdata/scattering/manifest.toml` | scattering inputs: PyTMatrix 0.3.3 lookup tables with their generator configs and held-out report, WRF P3 v5.4 lookup tables (see Scattering inputs below) |
| `testdata/fuzz/manifest.toml` | fuzz regression inputs: minimized libFuzzer mutations of real seeds, each with `derived_from` its seed (see `fuzz/README.md`) |
| `testdata/files/level2/` | 16 trimmed Level II fixtures, `<SITE><YYYYMMDD>_<HHMMSS>.trim.V06` |
| `testdata/files/level2-chunks/` | the first three chunks of KIWA volume 307 |
| `testdata/files/other/` | committed files for the other formats (layout under Other formats below) |
| `testdata/files/scattering/` | committed scattering inputs |
| `testdata/files/fuzz/<target>/` | committed fuzz regression inputs, named `<kind>-<description>` (kind: crash, oom or timeout) |
| `testdata/golden/` | JSON goldens the real-data tests compare against, written by the `tools/*_golden.py` scripts from independent readers |
| `crates/recast-radar-testdata/` | manifest loader, download and cache, `trim-level2` tool, tests |
| `tools/validate_trimmed.py` | checks the trimmed Level II fixtures against their sources with Py-ART and MetPy |
| `tools/core_golden.py`, `tools/scattering_golden.py` | goldens for `recast-radar-core` (model and merge tests) and `recast-radar-scattering` |

## Verification

The integration checks (plan task TD.4) were run on branch `testdata` on 2026-09-16:

- `cargo test -p recast-radar-testdata` passes 30 tests with 0 failures: 12 unit tests, 12 in
  `tests/manifest.rs`, 3 in `tests/trim.rs`, 2 in `tests/corpus_doc.rs` and 1 doctest. The trim tests
  reproduce all 16 fixtures from their cached source volumes.
- `cargo clippy -p recast-radar-testdata --all-targets -- -D warnings` reports no warnings, and
  `cargo fmt --check` is clean.
- A hash check in Python `hashlib`, separate from the crate, covered the committed files:
  - All 41 match their manifest sha256 and size, both in the working tree and as git blobs at `HEAD`.
  - Every file under `testdata/files/` is tracked by git and named by exactly one manifest entry.
- All 104 download entries are in the shared cache and match their sha256 and size
  (371,057,090 bytes).
- `tools/validate_trimmed.py --no-download` (Py-ART 2.2.5, MetPy 1.7.1) passes all 16 trimmed fixtures
  with 1,285 checks.
- Committed testdata is 25,575,337 bytes in 41 files, against the 60 MB budget. With the three manifests
  it is 25,706,598 bytes.

Changes made during integration:

- `docs/testdata/corpus-level2.md` and `docs/testdata/corpus-other.md` were merged into this page. The
  trimmed fixtures (TD.3) were added, and the manifest index is now generated.
- The site tags `site:KBMX` and `site:TAKA` became `site:kbmx` and `site:taka`, matching every other
  site tag. The manifest comments and derivations that named the old pages now name this page.

## Level II

These entries are in `testdata/level2/manifest.toml`:

- 29 archive objects (`format = "nexrad-level2"`) from the public `unidata-nexrad-level2` bucket. They are
  downloaded on first use.
- One complete real-time chunk volume of 70 chunks (`format = "nexrad-level2-chunk"`, `ephemeral = true`)
  from `unidata-nexrad-level2-chunks`. The first three chunks are committed under
  `testdata/files/level2-chunks/` (502,179 bytes).
- 16 trimmed fixtures (`<source id>-trim`), made from 16 of the archive objects and committed under
  `testdata/files/level2/` (9,891,922 bytes).

The 96 Level II files that are not committed total 247,881,182 bytes. The chunk URLs expire, so the
shared cache holds the only copies of the 67 chunks that are not committed. Every other URL is a
permanent object.

### How the corpus was built and checked

- **Keys exist.** Every key was found with S3 ListObjectsV2
  (`https://<bucket>.s3.amazonaws.com/?list-type=2&prefix=...`). The listed `Size` equals the size of the
  downloaded file for all 99 entries. The chunk keys were listed in `unidata-nexrad-level2-chunks`
  (`KIWA/307/`) on 2026-09-17 around 00:56Z.
- **Hashes.** sha256 and size come from the downloaded bytes. The cache files were hashed again against the
  manifest (99/99 match).
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

### Era, format and build coverage

The trimmed column marks the 16 sources of the trimmed fixtures (tag `trim`). Builds are the Message 2 RDA-build
halfword read the way MetPy reads it (raw/100, or raw/10 when raw/100 <= 2).

| id | header | build | message layout | VCP | trimmed |
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
| l2-klix-20210829-173117 | AR2V0006.048 | 19.1 | same layout as 18:04Z; 33 min earlier (stale temporal prior for recast-radar-correct) | 112 | |
| l2-klix-20210829-175748 | AR2V0006.052 | 19.1 | same layout as 18:04Z; the previous volume (temporal prior for recast-radar-correct) | 112 | |
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

### Weather regimes

| regime | id | what the volume shows |
|---|---|---|
| hurricane (legacy) | l2-klix-20050829-130035 | Katrina eye ~60 km S; eyewall and aliased velocity close to the radar |
| hurricane | l2-klix-20210829-180425 | Ida eye ~140 km SW; full eyewall ring in range |
| hurricane, non-CONUS | l2-tjua-20220918-190621 | Fiona eye ~100 km W at landfall |
| typhoon, non-CONUS | l2-pgua-20230524-030945 | Mawar eyewall over northern Guam (eye ~45 km NE); beam-blockage wedge |
| winter storm (snow) | l2-kbox-20220129-150537 | blizzard snow bands; median ZDR 0.1 dB over Z > 15 dBZ, RHOHV > 0.97 gates |
| derecho | l2-kdvn-20200810-180401 | bow echo 100-150 km W; 99 gates >= 60 dBZ within 120 km on the lowest sweep |
| derecho, consecutive volumes | l2-kdvn-20200810-175718, l2-kdvn-20200810-180401, l2-kdvn-20200810-181043, l2-kdvn-20200810-181724 | the four consecutive KDVN volumes 17:57-18:17Z (tag `sequence:kdvn-20200810`, about 6.7 min apart); with their Level III STI products (`l3-kdvn-20200810-*-nst`) they are the storm-tracking test sequence |
| hail | l2-kewx-20160413-022531 | San Antonio hailstorm: 259 gates >= 60 dBZ, 62 >= 65 dBZ, 39 with RHOHV < 0.93 and abs(ZDR) < 1 dB (lowest sweep, 120 km) |
| tornado | l2-ktlx-19990504-002218, l2-ktlx-20030508-221041, l2-ktlx-20130520-201643, l2-koax-20140616-205305, l2-kdgx-20230325-010651 | Moore 1999 / 2003 / 2013, Pilger 2014, Rolling Fork 2023 |
| clear air | l2-ktlx-20240515-000014 (VCP 35), l2-kmaf-20230331-230843 (VCP 31), l2-kvnx-20110315-000203, l2-kpah-20080415-235014, l2-kvwx-20080415-235337 (VCP 32) | no precipitation; ground clutter or biological returns |
| complex terrain (snow) | l2-kmtx-20240301-212827 | mountain-top site, 0.0 deg base tilt, snow bands (median ZDR -0.1 dB) |
| non-CONUS stratiform | l2-pahg-20250909-212549 | Kenai, Alaska: widespread stratiform precipitation |
| convective (other) | l2-ktlx-19910605-162126, l2-kdmx-20080525-205148, l2-kgwx-20130601-235640, l2-tstl-20230331-230314, l2-ktlx-20240315-000217, l2-kilx-20260418-013553, l2-kiwa-20260917-003629 | thunderstorms at various ranges |

### Trimmed fixtures

`trim-level2` (`crates/recast-radar-testdata/src/trim.rs`) cuts a full volume down to a committed
fixture without changing any message bytes:

- **What it keeps.**
  - The 24-byte volume header and every message before the first radial (the metadata record),
    unchanged.
  - The radials of the first split cut, found from the data: sweep 1 has no VEL, sweep 2 has VEL, and
    their mean elevations are within 0.25 deg. With no split cut, it keeps the first sweep.
  - With `--max-radials N`, only the first N radials of each kept sweep, cut at a record boundary.
    `--max-bytes B` picks the largest such N whose output fits in B bytes.
- **How it writes records.** Records are LDM records compressed with pure-Rust bzip2 at level 9, and the
  last control word is negative (end of volume).
  - Sources already made of LDM records keep their record boundaries. In all 8 such outputs, every record
    payload is byte-identical to a source record payload.
  - Gzip and uncompressed sources are framed as one record of the leading non-radial messages, then records
    of at most 120 radials of one elevation.
- **Reproducibility.** The output is deterministic, and each entry's `derivation` starts with the exact
  options. `tests/trim.rs` re-trims each committed fixture, which works offline, and trims each source
  volume, which is skipped offline. Both must reproduce the committed bytes.
- **Size.** The target is 1,000,000 bytes per file, with a hard cap of 2 MB. Whole split-cut sweeps fit
  the target for 6 sources. For the other 10 they take 1,128,608 to 3,182,975 bytes, and 6 of those 10
  are over 2 MB. Those 10 fixtures keep the first 120-480 radials of each sweep, a multiple of the
  120-radial source records.

| id | layout it covers | bytes | split cut kept (radials per sweep) | what the kept radials show |
|---|---|---:|---|---|
| l2-ktlx-19910605-162126-trim | ARCHIVE2 with a blank ICAO, no metadata record | 141,911 | 0.43 deg, whole (366 + 366) | thunderstorms to 59.4 dBZ at 22 km |
| l2-ktlx-19990504-002218-trim | Message 1, VCP 11 | 191,887 | 0.45 deg, whole (367 + 367) | Bridge Creek-Moore F5 supercell, 68.0 dBZ at 23 km |
| l2-ktlx-20030508-221041-trim | the type-202 record | 184,958 | 0.49 deg, whole (367 + 367) | Moore F4 hook echo ~15 km W |
| l2-klix-20050829-130035-trim | AR2V0001 with segmented metadata, VCP 121 | 241,543 | 0.38 deg, whole (367 + 362) | Katrina eyewall, Doppler speeds up to 32 m/s |
| l2-kdmx-20080525-205148-trim | Build 10 super-res Message 31 | 788,002 | 0.48 deg, whole (720 + 720) | storms to 68.0 dBZ at 123 km |
| l2-ktlx-20130520-201643-trim | Build 13.2 dual-pol, RAD 20 | 793,144 | 0.51 deg, 480 of 720 | Moore EF5 supercell, 69.5 dBZ at 23 km |
| l2-koax-20140616-205305-trim | RAD 28, SAILS | 826,983 | 0.48 deg, 360 of 720 | Pilger supercell at 91-112 km (62.0 dBZ) |
| l2-kewx-20160413-022531-trim | gzip Build 16.1 | 856,386 | 0.53 deg, 240 of 720 | storms 53-136 km W-NW (65.0 dBZ); **not** the hail core |
| l2-kdvn-20200810-180401-trim | LDM records with Message 13 | 629,083 | 0.44 deg, 120 of 720 | derecho bow echo at 49-126 km (66.0 dBZ) |
| l2-klix-20210829-180425-trim | CFP and MPDA | 440,556 | 0.48 deg, 120 of 720 | only the outer rain bands of Ida, 200-230 km N; **not** the eyewall |
| l2-kbox-20220129-150537-trim | VOL 52, snow | 998,852 | 0.48 deg, 240 of 720 | blizzard snow, median ZDR 0.12 dB |
| l2-tstl-20230331-230314-trim | TDWR | 389,446 | 0.26 deg, whole (360 + 360) | supercell line ~85 km S (60.5 dBZ) |
| l2-pgua-20230524-030945-trim | non-CONUS, MESO-SAILS x3 | 960,321 | 0.50 deg, 240 of 720 | Mawar rain bands 61-123 km SE; **not** the eyewall |
| l2-kmtx-20240301-212827-trim | base tilt, terrain | 884,380 | 0.04 deg (commanded 0.0), 360 of 720 | NW and NE snow bands |
| l2-ktlx-20240315-000217-trim | bench volume, VOL 52, MESO-SAILS x3, AVSET | 741,465 | 0.48 deg, 480 of 720 | supercells 225-243 km S (69.5 dBZ) |
| l2-kilx-20260418-013553-trim | Message 32, MRLE | 823,005 | 0.52 deg, 240 of 720 | storms to 67.5 dBZ at 31 km |

Tags on the trimmed entries describe the trimmed file, not the source:

- Every entry gets `compression:ldm-bzip2`, `trimmed` and `split-cut`, and partial trims also get
  `partial-sweeps`.
- `msg:N` tags list only the message types in the trimmed file, and the provider and bucket tags are
  dropped.
- A regime tag stays only when the feature was measured inside the kept radials. So KEWX is tagged
  `regime:convective` instead of `regime:hail`, KLIX 2021 has no regime tag, and PGUA keeps
  `regime:non-conus` but not `regime:typhoon`.

`tools/validate_trimmed.py` checks every trimmed fixture against its full source volume:

- Py-ART `read_nexrad_archive` and MetPy `Level2File` both read the trimmed file, and it has the sweeps
  and radial counts that the options select.
- With Py-ART, the raw moment arrays and radial metadata of every kept radial equal those of the source,
  and so do the `Radar` fields, times and fixed angles.
- With MetPy, every kept radial and all decoded metadata equal the source.
- Each trimmed record is a contiguous run of the source's message stream, in source order.
- As a negative control, changing one gate byte was caught by both readers.
- MetPy decodes raw 0 and raw 1 both as NaN, so its comparison cannot tell them apart. The Py-ART
  raw-integer comparison covers that case.
- Py-ART 2.2.5 converts station elevations from feet to meters in place on every
  `get_nexrad_location` call, so a second read of a TDWR file gets a different altitude. The validator
  restores the station table before each read.

Notes on the source volumes:

- **Two readers read every source.** Py-ART and MetPy both read all 16 full source volumes.
- **MetPy needs a `.gz` path for gzip files.** MetPy detects gzip only from a `.gz` file extension. The
  cache files have no extension, so a gzip archive read straight from the cache fails in MetPy with errors
  such as `data type '>u19' not understood`. Open those through `gzip.open`, or through a copy whose name
  ends in `.gz`.
- **Split cuts come first.** Where the first cut is a split cut, the first two sweeps form the surveillance
  and Doppler pair.
- **The lowest cuts differ.** The KTLX 1991 and 1999 legacy volumes start with a REF-only cut, then a
  VEL/SW cut at the same angle. TSTL (TDWR) starts with a REF-only long-range 0.26 deg sweep (300 m gates),
  then a 0.26 deg REF/VEL/SW sweep at 150 m.

### Real-time chunk capture (KIWA volume 307)

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

### Reader compatibility

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
| l2-klix-20210829-173117 | 23 sweeps / 12600 rays / 7 fields | 23 sweeps / 12600 radials |
| l2-klix-20210829-175748 | 23 sweeps / 12600 rays / 7 fields | 23 sweeps / 12600 radials |
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

The 16 trimmed fixtures are read by both readers and checked against their sources; see Trimmed
fixtures.

### Format timeline

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

### Differences from the plan text

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
- **Trim size.** The plan asks for at most 1 MB per trimmed file (hard cap 2 MB). Whole split cuts would
  exceed 1 MB for 10 of the 16 sources, so those fixtures use the spec's "or N radials" option: the first
  N radials of each kept sweep, cut at a source record boundary.
- **End-of-volume marker.** The plan does not give the sign of the last control word. The trimmed files
  make it negative, as in every complete archive object in the corpus.
- **Trimmed file names.** Every trimmed file is named `<SITE><YYYYMMDD>_<HHMMSS>.trim.V06`, including the
  ARCHIVE2, AR2V0003 and AR2V0008 sources. The real header version is in the file and in the tags.
- **Manifest path.** The Level II entries are in `testdata/level2/manifest.toml`, not in
  `testdata/manifest.toml`, because the crate loads every `testdata/*/manifest.toml`. Committed paths are
  relative to `testdata/`.

## Other formats: ODIM_H5, CfRadial, DORADE, JMA and Level III VWP

These entries are in `testdata/other/manifest.toml`, and their committed files are under
`testdata/files/other/`. The corpus holds only real radar files. A few files are derived from real files
by copying raw bytes: trimming, taking a subset, changing the container, or extracting an archive member.
Every derived file names its source and has a recipe on this page.

There are 35 entries. 27 files are committed (15,975,753 bytes, each under 2 MB). 8 files are downloaded
on first use (123,175,908 bytes). Five of the committed entries (the NOXP 2009-06-10 head trims and their
zip, and the NCI THREDDS zip response) were added by the io-formats test conversion (plan task C.2).
Later additions are not in these counts (the generated totals at the end of this page are current),
among them the four NOAA P-3 N42RF entries of the metadata-complete stream (2026-09-25): two airborne
sweepfiles downloaded on first use (6,020,260 bytes) and their committed head trims (655,332 bytes).

Checked when the files were curated (2026-09-16):

- Every URL in the manifest was downloaded and matched the sha256 in the
  manifest. Zenodo archives also matched the md5 that Zenodo publishes.
- Independent readers checked the files:
  - Py-ART 2.2.5 `read_odim_h5` and xradar 0.12.0 `open_odim_datatree` read
    all 6 ODIM polar volumes.
  - h5py 3.16.0 showed the HDF5 layout of every ODIM file, including the
    IMGW images.
  - Py-ART `read_cfradial` read all 4 committed CfRadial files and the
    downloaded DOW8 RHI and S-Pol volume. xradar read the S-Pol volume.
  - A standalone Python DORADE block walker and a JMA GRIB2 section and
    run-length walker checked the DORADE and JMA files.
  - Later (2026-09-25), LROSE RadxPrint (release 20250811, in the `nexbench`
    container) read seven of the committed DORADE sweepfiles and both full
    N42RF sweepfiles (`tools/dorade_radx_golden.py`), and `RadxPrint -native`
    printed the descriptor blocks of all ten committed sweepfiles
    (`tools/dorade_radx_native_golden.py`).

### Layout

| Directory | Contents |
|---|---|
| `files/other/odim/` | ODIM_H5 polar volumes (PVOL) |
| `files/other/odim/ord-parts/` | per-quantity ODIM_H5 parts of one scan (ORD archive objects, named as listed) |
| `files/other/odim/imgw_polrad/` | ODIM_H5 Cartesian IMAGE products (IMGW CMAX) |
| `files/other/odim/nci/` | NCI THREDDS responses for members of daily ODIM_H5 zips (zip local-file records), and a sweep subset of one member |
| `files/other/cfradial/` | CfRadial 1.x in classic netCDF and netCDF-4 containers |
| `files/other/dorade/` | DORADE sweep files |
| `files/other/jma/` | JMA polar GRIB2 tars with one station each |
| `files/other/nexrad-level3/` | NEXRAD Level III VWP (Product 48) carried over from BowEcho |

### Entries

"C" means the file is committed. "D" means it is downloaded on first use.
"derived" means the bytes come from a real file by a recipe in the
Derivation recipes section below.

#### ODIM_H5

| id | C/D | bytes | what it covers | source (license) |
|---|---|---|---|---|
| `odim-bejab-20190606-0000-pvol` | C | 640209 | H5rad 2.0, superblock v0, 11 DBZH sweeps, gate count changes between sweeps | wradlib-data (MIT) |
| `odim-bewid-20130429-0430-pvol-dbzh-scan1` | C | 348893 | H5rad 2.1, variable-length string attributes (global heap), root `how/NI` | wradlib-data (MIT) |
| `odim-norst-20170421-0908-pvol` | C | 422385 | H5rad 2.2, **superblock v1**, lowest sweep has 720 rays | open-radar-data (MIT) |
| `odim-espdg-20260707-1927-pvol-dbzh-vradh` | C | 162450 | H5rad 2.4 IRIS export, **v2 object headers** (OHDR/OCHK), float64 planes, `rstart` in metres | OPERA ORD 24h bucket (CC BY 4.0), URL expired |
| `odim-imgw-ram-20260711-0015-{kdp,phidp,rhohv,zdr}-max` | C | 33775, 32534, 61984, 59793 | ODIM IMAGE (Cartesian MAX with side projections), `what` on `dataset1`, version string `H5rd 2.3`, source has only a WMO number | IMGW-PIB datastore (attribution required), URL expired |
| `odim-iesha-20260305-0115-pvol` | C | 1667065 | **new** H5rad 2.3, 10 sweeps DBZH+TH+VRADH up to a 90 deg vertical sweep, widespread echo | OPERA ORD archive (CC BY 4.0) |
| `odim-dkrom-20260820-1130-pvol` | C | 1695131 | **new** H5rad 2.0 dual-pol, 10 sweeps x 8 quantities (VRAD/WRAD names, LDR all nodata), elevations not whole degrees | OPERA ORD archive (CC BY 4.0) |
| `odim-au24-20260610-000300-nci-zip-member` | C | 354749 | **new** unmodified NCI THREDDS response for a daily-zip member URL: a ZIP local-file record (deflate) holding an H5rad 2.4 PVOL (Bowen, 10 sweeps 0.8-32 deg) followed by the start of the next record, no central directory | NCI `rq0` Level 1 archive (Bureau of Meteorology, CC BY 4.0) |
| `odim-au02-20260921-0000-pvol-subset` | C, derived | 141384 | **new** H5rad 2.4 RAINBOW PVOL (Melbourne), sweeps 2 and 3 of 14: per-ray `how` arrays no typed slot holds (`TXpower`, `dataflag`, `noisepowerh`, `noisepowerv`, `numpulses`, `startT`), kept verbatim in `Sweep::other`, which must move with their rays when rays are reordered (`fm301::order_rays_for_view`) | NCI `rq0` Level 1 archive (Bureau of Meteorology, CC BY 4.0) |
| `odim-bejab-20260612-1450-{dbzh,vrad}` | C | 243054, 219191 | **new** one Doppler-task scan of RMI Jabbeke delivered as one PVOL per quantity: 9 sweeps 0.5-25 deg x 360 rays x 300 gates in both files, identical per-sweep times (`merge_volumes` inputs) | OPERA ORD archive (CC BY 4.0) |
| `odim-nohur-20260612-1445-{dbzh,th}`, `odim-nohur-20260612-1446-vradh` | C | 773343, 1478472, 197553 | **new** one scan of MET Norway Hurum as three per-quantity PVOLs: DBZH and TH over 10 sweeps 0.5-90 deg (720 rays on the lowest, tiered gate counts, a 30 m vertical sweep), VRADH over the 8 sweeps from 2.6 deg with a later `/what` time | OPERA ORD archive (CC BY 4.0) |

#### CfRadial

| id | C/D | bytes | what it covers | source (license) |
|---|---|---|---|---|
| `cfrad1-xsapr-sgp-20110520-ppi-netcdf4` | C | 75587 | Published netCDF-4 CfRadial 1.2 PPI (40x42). BowEcho used it to test routing and the guidance error for netCDF-4 input | Py-ART test data (BSD-3-Clause) |
| `cfrad1-xsapr-sgp-20110520-ppi-classic` | C, derived | 13624 | Classic-container copy of the file above | derived |
| `cfrad1-dow8-20211011-223602-rhi` | D | 1682730 | Native DOW8 RHI, CF-Radial-1.4 netCDF-4, 8 fields, mobile `latitude(time)` | open-radar-data (MIT) |
| `cfrad1-dow8-20211011-223602-rhi-trim3-classic` | C, derived | 888428 | Classic-container copy of the RHI above with 3 fields | derived |
| `cfrad1-irene-sr2-20110827-120420-sur-sweeps01` | C, derived | 1664412 | **new** Radx-written **classic** CfRadial 1.3 PPI (SMART-R2, Hurricane Irene), 2 sweeps, int8 packed DBZ/VEL with `_FillValue` | Zenodo 10.5281/zenodo.3494891 (CC BY 4.0) |
| `cfrad1-spol-20080604-002217-sur` | D | 15418562 | **new** full CfRadial 1.2 PPI volume (S-Pol, 9 sweeps), netCDF-4 | open-radar-data (MIT) |
| `cfrad2-spol-20080604-002217-sur` | D | 15487749 | **new** the same volume in CfRadial 2 group layout | open-radar-data (MIT) |

#### DORADE

| id | C/D | bytes | what it covers | source (license) |
|---|---|---|---|---|
| `dorade-cow2-20260521-225514-sur-head24` | C, derived | 37380 | Big-endian, HRD RLE, CSFD, antenna-transition rays, staggered PRT; first 24 of 719 rays | CSWR COW2 deployment; source URL and license unknown |
| `dorade-noxp-20090501-sweeps-tgz` | D | 324596 | Zenodo archive holding the next two entries | Zenodo 10.5281/zenodo.14194361 (CC BY 4.0) |
| `dorade-noxp-20090501-190244-ppi`, `dorade-noxp-20090501-190324-ppi` | C, derived | 939268 each | **new** consecutive single-tilt set: little-endian, uncompressed, CSFD, 51 rays over 360 deg, RADD lat/lon written as 0 | archive member, unmodified |
| `dorade-noxp-20090525-sweeps-tgz` | D | 5813887 | Zenodo archive holding the next entry | Zenodo 10.5281/zenodo.14194361 (CC BY 4.0) |
| `dorade-noxp-20090525-203211-sector` | C, derived | 1634536 | **new** little-endian sector PPI with echo, 8 dual-pol fields | archive member, unmodified |
| `dorade-dow6-20211230-222139-rhi-head41` | C, derived | 1471504 | **new** first real **DORADE RHI** (RADD scan mode 3): little-endian HRD RLE, CELV, 32 fields, rays 0-40 of 156 | Zenodo DOI 10.48514/JKJ0-TE44, FARM Marshall Fire (CC BY 4.0) |
| `dorade-noxp-20090610-{003210,003222,003226}-ppi-head6` | C, derived | 131596 each | **new** three sweeps (0.5, 1.0, 2.0 deg) of one **multi-elevation** NOXP volume (NOX090610003210.RAWAL8D): little-endian, uncompressed, CSFD 1174 x 75 m, real site coordinates; first 6 rays each | head trims of members of Zenodo 10.5281/zenodo.14194361 `2009.NOX.sweep.0609.tar.gz` (CC BY 4.0) |
| `dorade-noxp-20090610-003210-heads-zip` | C, derived | 44980 | **new** zip of that volume directory: the three head trims (stored 1.0, 0.5, 2.0 deg) and the directory's three text files, under their tar paths | container conversion (CC BY 4.0) |
| `dorade-n42rf-ts-20181010-122951-air`, `dorade-n42rf-tm-20181010-123925-air` | D | 2919700, 3100560 | **new** first **airborne DORADE**: NOAA P-3 N42RF tail radar (aft and fore antennas) in Hurricane Michael, RADD radar type 3 and scan mode 9 (AIR), little-endian HRD RLE, CELV 627 x 75 m, 17 fields, 360 rays; every ASIB motion and attitude value set; each ends with NULL, RKTB and a SEDS (Solo II edit history) block; the fore sweep has non-zero CFAC corrections | GitHub `Alex-DesRosiers/radarqc_scans` at 0dc45a2, unmodified; NOAA AOC P-3 data edited in Solo II (no license stated) |
| `dorade-n42rf-ts-20181010-122951-air-head24`, `dorade-n42rf-tm-20181010-123925-air-head48` | C, derived | 432440, 222892 | **new** head trims of those two: the first 24 and 48 rays | head trims (no license stated) |

#### JMA GRIB2 tar

| id | C/D | bytes | what it covers | source (license) |
|---|---|---|---|---|
| `jma-n5-20191012-090000` | D | 39106560 | **new** full N5 (reflectivity) tar at Typhoon Hagibis landfall, 20 stations | NICT mirror of JMA (terms not stated) |
| `jma-n6-20191012-090000` | D | 13209600 | **new** full N6 (radial velocity) tar at the same time, 20 stations | NICT mirror of JMA (terms not stated) |
| `jma-n5-20191012-090000-rs47773` | C, derived | 1761280 | Station TAKA (Osaka) N5 member: 26 sweeps, four descending elevation ladders | derived |
| `jma-n6-20191012-090000-rs47773` | C, derived | 624640 | Station TAKA N6 member: 13 sweeps, 25% of velocity gates non-missing | derived |

#### NEXRAD Level III (carried over)

| id | C/D | bytes | what it covers | source (license) |
|---|---|---|---|---|
| `l3-kbmx-19980416-archive-tarz` | D | 32132224 | NCEI day archive (`.tar.Z`) holding the VWP product | Google Cloud `gcp-public-data-nexrad-l3` (NOAA, public domain) |
| `l3-kbmx-19980416-0006-nvw` | C, derived | 7090 | Product 48 VWP whose HHMM timeline crosses midnight | archive member, unmodified |
| `l3-kdvn-20200810-1757-nst` | C | 14838 | Product 58 Storm Tracking Information for `l2-kdvn-20200810-175718` (50 SCIT cells) | AWS `unidata-nexrad-level3` object `DVN_NST_2020_08_10_17_57_18`, unmodified (NOAA, public domain) |
| `l3-kdvn-20200810-1804-nst` | C | 14928 | STI for `l2-kdvn-20200810-180401` (45 cells) | `DVN_NST_2020_08_10_18_04_01` |
| `l3-kdvn-20200810-1810-nst` | C | 13758 | STI for `l2-kdvn-20200810-181043` (35 cells) | `DVN_NST_2020_08_10_18_10_43` |
| `l3-kdvn-20200810-1817-nst` | C | 13158 | STI for `l2-kdvn-20200810-181724` (33 cells) | `DVN_NST_2020_08_10_18_17_24` |

The Level III stream (`testdata/level3/manifest.toml`, branch `level3`) owns
Level III coverage. The VWP file is here only because it was among the BowEcho
fixtures carried over; the four STI products were added by stream C (task C.2,
group `track`) as the independent reference for the storm-tracking tests: MetPy
`Level3File` reads their storm-id symbology packets (current, past and forecast
positions) and the STORM ID / AZ/RAN / FCST MVT / DBZM HGT tabular pages, which
`tools/track_golden.py` turns into `testdata/golden/track/tracking.json`. They
may move to the Level III manifest when the branches merge.

### BowEcho carryover (radar-bow@66ceb9c `crates/nexrad_io/tests/data`)

The files were extracted with `git -C radar-bow archive 66ceb9c`.

| BowEcho path | Here | Provenance check |
|---|---|---|
| `bejab.pvol.hdf` | `odim/bejab.pvol.hdf` | sha256 equals wradlib-data at commit 67337e5 |
| `20130429043000.rad.bewid.pvol.dbzh.scan1.hdf` | `odim/` (same name) | sha256 equals wradlib-data at commit 67337e5 |
| `T_PAGZ35_C_ENMI_20170421090837.hdf` | `odim/` (same name) | sha256 equals open-radar-data at commit ff39154 and its pooch registry |
| `espdg.pvol.20260707.dbzh_vradh.h5` | `odim/` (same name) | Came from the ORD 24h bucket on 2026-07-07 (`odim_real_files.rs` header). That object has expired and the ORD archive has no ES data for that day, so only the committed copy remains |
| `imgw_polrad/*.max.h5` | `odim/imgw_polrad/` | sha256 equals the values in BowEcho's `imgw_polrad/README.md`. The datastore is rolling and the URLs now return HTML |
| `cfrad.xsapr_sgp_ppi_20110520.netcdf4.nc` | `cfradial/` (same name) | sha256 equals Py-ART `example_cfradial_ppi.nc` at commit 1edc407 |
| `cfrad.xsapr_sgp_ppi_20110520.classic.nc` | `cfradial/` (same name) | Rebuilt from the Py-ART file with `convert_cfradial.py`. The output matches byte for byte |
| `cfrad.20211011_223602_DOW8_RHI.trim3.nc` | `cfradial/` (same name) | Rebuilt from the open-radar-data DOW8 RHI with `convert_cfradial.py ... DBZHC,VEL,WIDTH`. The output matches byte for byte |
| `swp.1260521225514.COW2.229.1.0_SUR_v215.head24` | `dorade/` (same name) | `dorade_real.rs` describes it as the first 37,380 bytes of a real COW2 sweep file from 2026-05-21. BowEcho recorded neither the full file nor a public source, and it was not found locally |
| `nexrad_vwp/KBMX_SDUS54_NVWBMX_199804160006` | `nexrad-level3/` (same name) | Extracted from the Google Cloud copy of the NCEI archive named in `nexrad_vwp/README.md`. sha256 matches |

These files were not carried over:

- `cfrad_synth.nc` and `gen_cfradial_fixture.py`: synthetic. The replacement
  is `cfrad1-irene-sr2-20110827-120420-sur-sweeps01`. The full volumes
  `cfrad1-spol-*` and `cfrad2-spol-*` are also new download entries.
- `odim_pvol_synth.h5` and `gen_odim_fixture.py`: synthetic. The replacements
  are `odim-iesha-20260305-0115-pvol` and `odim-dkrom-20260820-1130-pvol`.
- `ref_cfradial.py` and `ref_odim.py`: scripts that print BowEcho golden
  values in the `dump_radar.rs` text format. They are test tooling, not
  corpus files. They remain at radar-bow@66ceb9c for the wave 2 test
  conversion.
- `convert_cfradial.py`: its recipe is reproduced below.
- `README.md` files: their provenance is now in the manifest and on this page.

#### What the synthetic fixtures covered, and what the real replacements cover

| Synthetic feature | Real coverage now |
|---|---|
| ODIM PVOL with DBZH and VRADH | iesha (DBZH/TH/VRADH), dkrom (VRAD plus dual-pol), espdg (DBZH/VRADH float64) |
| ODIM gzip-chunked u8 planes | all PVOLs |
| ODIM **contiguous** (unchunked) data layout | **gap**: every real file checked uses chunked+gzip (bejab, bewid, norst, espdg, iesha, dkrom) |
| ODIM nodata/undetect sentinel gates | all real PVOLs contain both |
| CfRadial classic container | xsapr classic (converted), DOW8 trim3 (converted), **Irene, written natively by Radx** |
| CfRadial **UNLIMITED `time`** (record-variable interleave) | xsapr classic: `convert_cfradial.py` keeps the Py-ART file's unlimited `time` (40 records), so its fields and per-ray `prt`/`unambiguous_range`/`nyquist_velocity` are record variables. **Gap**: no classic file *written natively* with an unlimited record dimension; Irene and DOW8 trim3 have a fixed `time` |
| CfRadial packed short with scale/offset | Irene packs int8 with scale/offset. **Gap**: no real int16 packed field |
| CfRadial float field with `_FillValue` | **gap** in classic files. The xsapr and DOW8 netCDF-4 originals have float fields |
| Two PPI sweeps with per-ray PRT, unambiguous range and sample counts | Irene: 2 sweeps, per-ray `prt`, `prt_ratio`, `unambiguous_range`, `n_samples`, `nyquist_velocity` |

### Derivation recipes

All scripts were run with the Python venv named in the wave 1 plan:
netCDF4-python 1.7.4, h5py 3.16.0, numpy 2.5.3.

#### `convert_cfradial.py` (from BowEcho): netCDF-4 to classic container

Run as `convert_cfradial.py in.nc out.nc [FIELD,FIELD,...]`. It makes a raw
copy of each variable. It turns off mask/scale, keeps `_FillValue`, copies
all attributes and dimensions (keeping any unlimited dimension), and drops
`(time, range)` fields that are not listed.

```python
import sys, netCDF4
def main(src_path, dst_path, keep_fields=None):
    src = netCDF4.Dataset(src_path)
    dst = netCDF4.Dataset(dst_path, "w", format="NETCDF3_CLASSIC")
    field_vars = {k for k, v in src.variables.items() if v.dimensions == ("time", "range")}
    drop = field_vars - set(keep_fields) if keep_fields is not None else set()
    dst.setncatts({k: src.getncattr(k) for k in src.ncattrs()})
    for name, dim in src.dimensions.items():
        dst.createDimension(name, None if dim.isunlimited() else len(dim))
    for name, var in src.variables.items():
        if name in drop:
            continue
        var.set_auto_maskandscale(False)
        fill = var.getncattr("_FillValue") if "_FillValue" in var.ncattrs() else None
        out = dst.createVariable(name, var.dtype, var.dimensions, fill_value=fill)
        out.set_auto_maskandscale(False)
        out.setncatts({k: var.getncattr(k) for k in var.ncattrs() if k != "_FillValue"})
        if var.shape:
            out[:] = var[:]
        else:
            out[()] = var[()]
    src.close(); dst.close()
if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2], sys.argv[3].split(",") if len(sys.argv) > 3 else None)
```

#### `cfrad_subset.py`: first N sweeps and chosen fields of a classic CfRadial

This script produced `cfrad1-irene-sr2-20110827-120420-sur-sweeps01` with
`cfrad_subset.py <member> out.nc 2 DBZ,VEL`. Running it twice gives
identical bytes. Every kept variable and attribute was compared with the
source volume, and all were byte-identical.

```python
import sys, netCDF4
src_path, dst_path, nsweeps, keep = sys.argv[1], sys.argv[2], int(sys.argv[3]), set(sys.argv[4].split(","))
src = netCDF4.Dataset(src_path)
assert src.file_format == "NETCDF3_CLASSIC"
nrays = int(src.variables["sweep_end_ray_index"][nsweeps - 1]) + 1
assert int(src.variables["sweep_start_ray_index"][0]) == 0
dst = netCDF4.Dataset(dst_path, "w", format="NETCDF3_CLASSIC")
dst.setncatts({k: src.getncattr(k) for k in src.ncattrs()})
for name, dim in src.dimensions.items():
    dst.createDimension(name, None if dim.isunlimited() else {"time": nrays, "sweep": nsweeps}.get(name, len(dim)))
fields = {k for k, v in src.variables.items() if v.dimensions == ("time", "range")}
for name, var in src.variables.items():
    if name in fields and name not in keep:
        continue
    var.set_auto_maskandscale(False)
    fill = var.getncattr("_FillValue") if "_FillValue" in var.ncattrs() else None
    out = dst.createVariable(name, var.dtype, var.dimensions, fill_value=fill)
    out.set_auto_maskandscale(False)
    out.setncatts({k: var.getncattr(k) for k in var.ncattrs() if k != "_FillValue"})
    index = tuple(slice(0, nrays) if d == "time" else slice(0, nsweeps) if d == "sweep" else slice(None) for d in var.dimensions)
    if var.shape:
        out[:] = var[index]
    else:
        out[()] = var[()]
src.close(); dst.close()
```

The source archive is 1.96 GB, so it is not a manifest entry. Use streaming
extraction: the member is the first file in `sr2_winds.tar.gz`, and reading
can stop once it is out.

```python
import tarfile, urllib.request
name = "sr2_winds/cfrad.20110827_120420.760_to_20110827_120802.081_CPOLRVP_IRENE_WINDS_SUR.nc"
r = urllib.request.urlopen("https://zenodo.org/records/3494891/files/sr2_winds.tar.gz")
t = tarfile.open(fileobj=r, mode="r|gz")
for m in t:
    if m.name == name:
        open("member.nc", "wb").write(t.extractfile(m).read())
        break
r.close()
```

The member's sha256 is in the manifest `derivation`.

#### `tarslice.py`: one JMA station per tar

The member's original 512-byte ustar header block and its data blocks are
copied without change. Zero blocks are then added up to the next
10240-byte record, with at least two zero blocks.

```python
import sys
src, pattern, dst = sys.argv[1], sys.argv[2], sys.argv[3]
data = open(src, "rb").read(); pos = 0; out = None
while pos + 512 <= len(data):
    hdr = data[pos:pos + 512]
    if hdr == b"\0" * 512: break
    size = int(hdr[124:136].rstrip(b"\0 ").decode() or "0", 8)
    nblocks = (size + 511) // 512
    if pattern in hdr[0:100].rstrip(b"\0").decode():
        assert out is None; out = data[pos:pos + 512 + nblocks * 512]
    pos += 512 + nblocks * 512
total = ((len(out) + 1024 + 10239) // 10240) * 10240
open(dst, "wb").write(out + b"\0" * (total - len(out)))
```

It was run as `tarslice.py <jma-n5-20191012-090000> RS47773 out.tar`, and
the same way for N6; and as `tarslice.py <jma-n5-20260924-210000> RS47937
out.tar` (JMA Okinawa, ITOK), and the same way for N6 of that time.

#### Head trims (DORADE)

- `dorade-dow6-20211230-222139-rhi-head41`: `head -c 1471504 <member>`.
  Offset 1,471,504 is where the RYIB block of ray 41 starts. The block walk
  from the start of the file gives these RYIB offsets: ray 0 at 10,372,
  ray 40 at 1,432,764, ray 41 at 1,471,504. The member comes from a 9.2 GB
  zip. Read the zip central directory with HTTP range requests (Zenodo
  supports them), then read the member's local header and deflate stream
  (compressed size 4,488,121 bytes). About 4.5 MB is transferred. A Python
  `zipfile.ZipFile` over a seekable file-like object that issues `Range`
  requests is enough.
- `dorade-cow2-20260521-225514-sur-head24`: carried over as is (see above).
- `dorade-noxp-20090610-{003210,003222,003226}-ppi-head6`: `head -c 131596 <member>`
  for members `swp.1090610003210.NOXPRVP.0.0.5_PPI_v1`,
  `swp.1090610003222.NOXPRVP.0.1.0_PPI_v1` and `swp.1090610003226.NOXPRVP.0.2.0_PPI_v1`
  of `2009/NOX/sweep/0609/NOX090610003210.RAWAL8D/` in `2009.NOX.sweep.0609.tar.gz`
  (157,788,051 bytes, md5 `af414f78b0bc77625747a9799a54200a` as Zenodo publishes; the directory is the
  first one in the archive, so a streaming read can stop after it). In all three the descriptor blocks
  (COMM SSWB VOLD RADD 9xPARM CSFD CFAC SWIB) end at 3,196, rays are 21,400 bytes, and the RYIB block
  of ray 6 starts at 131,596. None of the kept rays is flagged in transition.
- `dorade-n42rf-ts-20181010-122951-air-head24`: `head -c 432440 <dorade-n42rf-ts-20181010-122951-air>`,
  and `dorade-n42rf-tm-20181010-123925-air-head48`: `head -c 222892 <dorade-n42rf-tm-20181010-123925-air>`.
  In both files the descriptor blocks (SSWB VOLD RADD 17xPARM CELV CFAC SWIB COMM) end at 35,580, where
  the RYIB block of ray 0 starts; the RYIB block of ray 24 (aft) starts at 432,440 and that of ray 48
  (fore) at 222,892. The trims leave out the rest of the rays and the NULL, RKTB and SEDS blocks at the
  end, which the tests read from the full files.

#### `zip_noxp_heads.py`: the NOXP volume directory as a zip

Run as `zip_noxp_heads.py <extracted RAWAL8D dir>/ <dir with the .head6 files>/ out.zip` with Python
3.13 (zlib 1.3.1). Two runs give identical bytes (sha256 in the manifest).

```python
import sys, zipfile, time
src_dir, heads_dir, out = sys.argv[1], sys.argv[2], sys.argv[3]
prefix = "2009/NOX/sweep/0609/NOX090610003210.RAWAL8D/"
members = [  # tar member order, tar mtimes
    ("corrections", src_dir + "corrections", 1328204976),
    ("NOX090610003210.RAWAL8D.log", src_dir + "NOX090610003210.RAWAL8D.log", 1328205361),
    ("swp.1090610003222.NOXPRVP.0.1.0_PPI_v1", heads_dir + "swp.1090610003222.NOXPRVP.0.1.0_PPI_v1.head6", 1328205360),
    ("swp.1090610003210.NOXPRVP.0.0.5_PPI_v1", heads_dir + "swp.1090610003210.NOXPRVP.0.0.5_PPI_v1.head6", 1328205360),
    ("swp.1090610003226.NOXPRVP.0.2.0_PPI_v1", heads_dir + "swp.1090610003226.NOXPRVP.0.2.0_PPI_v1.head6", 1328205360),
    ("sigmet_dorade.out", src_dir + "sigmet_dorade.out", 1328205361),
]
with zipfile.ZipFile(out, "w") as z:
    for name, path, mtime in members:
        info = zipfile.ZipInfo(prefix + name, date_time=time.gmtime(mtime)[:6])
        info.compress_type = zipfile.ZIP_DEFLATED
        info.create_system = 3
        info.external_attr = 0o100644 << 16
        z.writestr(info, open(path, "rb").read(), compresslevel=9)
```

#### `derive_odim_subset.py`: chosen sweeps and planes of an ODIM_H5 file

`tools/derive_odim_subset.py <source.h5> <output.h5> datasetN:dataM,... [datasetK:...]` rebuilds the
root `what`/`where`/`how` groups and the chosen `datasetN` groups (their `what`/`where`/`how`, every
dataset-level `qualityK` group and the chosen `dataM` planes) in a new file, with every attribute in
storage order with its stored datatype and value, and every dataset with its datatype, shape,
chunking, filters and values. Names are not renumbered. The output has no object times, so it is
byte-reproducible for one h5py/HDF5 build (h5py 3.16.0, HDF5 2.0.0).

`odim-au02-20260921-0000-pvol-subset`: unwrap the NCI response's first ZIP local-file record (see
the NCI note below; `recast_radar_io::decompress_zip_local_member_bytes` or Python `zlib` with
`wbits=-15`, CRC-32 checked), then

```
python tools/derive_odim_subset.py 2_20260921_000000.pvol.h5 2_20260921_000000.pvol.subset.h5     dataset2:data1,data2 dataset3:data1
```

#### Archive members

`tar -xzf 2009.NOX.sweep.0501.tar.gz <member>` and the same for 0525, using
the member paths in the manifest. The `.../all/` paths in these archives are
hard links to the same files. For the Level III file, run
`gzip -dc NWS_NEXRAD_NXL3_KBMX_19980416000000_19980416235959.tar.Z | tar -x KBMX_SDUS54_NVWBMX_199804160006`.

### Notes on the new sources

- **OPERA ORD archive**: `https://s3.waw3-1.cloudferro.com/openradar-archive/{yyyy}/{mm}/{dd}/{CC}/{site}/PVOL/{site}@{stamp}@{elevs}@{moments}.h5`.
  Listing is anonymous S3 `ListObjectsV2`. Data is EUMETNET OPERA, CC BY 4.0.
  The manifest URLs encode `@` as `%40`, and both spellings return the same
  bytes. The two volumes were picked from surveys of daily file sizes as
  sub-2 MB volumes with echo.
- **Irene SMART-R2**: from Alford, Biggerstaff and Bodine (2019), "Data for
  'Transition of the hurricane boundary layer during the landfall of
  Hurricane Irene (2011)'", doi:10.5281/zenodo.3494891, CC BY 4.0. The files
  were converted from Sigmet RAW by RadxConvert in 2019 and kept the classic
  container. The same record's `sr2_rhi.tar.gz` (3.2 MB) holds two native
  classic CfRadial **RHI** files of about 2.6 MB each. They are over the size
  cap and are not used, but they would be a real classic RHI if one is needed.
- **VORTEX-2 NOXP**: Mansell and Burgess (2024), doi:10.5281/zenodo.14194361,
  CC BY 4.0. Sweep files were written by `sigmet_dorade` as little-endian,
  uncompressed DORADE with 1001 gates. The May 2009 archives inspected (0501,
  0507, 0509, 0510, 0513, 0522, 0525, 0527B, 0530) hold only single-tilt
  volumes. The June 2009 archives inspected (0601A, 0601B, 0604-0607, 0609,
  0610, 0612, 0614) hold 12- and 18-sweep volumes with sweep files of
  3.7-5.4 MB, which is over the cap.
- **FARM Marshall Fire DOW6**: Wurman and Kosiba (2023),
  doi:10.48514/JKJ0-TE44, CC BY 4.0. The zip holds 399 files: 396 `_SUR_`
  sweeps (2.5-68 MB) and 3 `_RHI_` sweeps (6.3, 6.3 and 14.5 MB).
- **JMA via NICT**: `https://pawr.nict.go.jp/jmadata/JMA-PolarCoordsRadar/{yyyy}/{mm}/{dd}/`
  keeps N5/N6 tars back to 2017. Some days are missing, for example
  2026/01/15. The mirror states no redistribution terms.
- **NEXRAD Level III archive**: the Google Cloud public bucket
  `gcp-public-data-nexrad-l3` keeps NCEI day tars back to the 1990s.
- **NCI THREDDS `rq0`** (Australian Unified Radar Archive Level 1, Bureau of Meteorology; license file
  `rq0_level1_license-cc4.pdf` in the catalog): a member URL
  `.../{site}/{yyyy}/vol/{site}_{yyyymmdd}.pvol.zip/{member}.pvol.h5` answers `200` with
  `Content-Length` equal to the requested member's uncompressed size, but the body is the first
  `Content-Length` bytes **of the zip file**, whatever member was named (checked 2026-09-17: the
  response for `24_20260610_142000.pvol.h5`, 157,280 bytes, is a prefix of the response for the first
  member `24_20260610_000300.pvol.h5`, and both start with the zip's own first bytes; 2024 zips start
  with a `_file_list` member). Only the first member of a daily zip is therefore readable this way, and
  only when its compressed record fits in its own uncompressed size. The committed response is the
  first member of the site 24 zip, fetched twice with identical bytes.

### Licensing and attribution

- The IMGW-PIB datastore terms require attribution to "Instytut Meteorologii
  i Gospodarki Wodnej – Państwowy Instytut Badawczy" and a note when the data
  has been processed. The fixtures are unmodified.
- OPERA ORD (espdg, iesha, dkrom) is CC BY 4.0. Attribute EUMETNET OPERA and
  the national services: AEMET, Met Éireann, DMI.
- The Zenodo datasets (Irene, NOXP, Marshall Fire) are CC BY 4.0. Cite the
  DOIs above. Derived fixtures are marked as derived in the manifest.
- wradlib-data and open-radar-data are MIT. The Py-ART test data is
  BSD-3-Clause.
- **Unresolved**: the terms for JMA data from the NICT mirror
  (`license:unknown`), the source and license of the COW2 head24 fixture
  (`license:unknown`), and the terms of the N42RF sweepfiles
  (`license:unknown`): the GitHub repository `Alex-DesRosiers/radarqc_scans`
  states no license. The data are NOAA AOC P-3 tail radar data, edited in
  Solo II by the repository's author. Review all three before publishing
  the repository.

## Gaps

### Level II

1. **Most chunks exist only in the shared cache.** The chunk URLs expire, so the 67 chunks that are not
   committed cannot be downloaded again. On a machine without that cache, tests
   that need them are skipped (`Offline`).
2. **Late End chunk.** The End chunk (070-E) was fetched 4.7 minutes after its S3 LastModified time,
   because of a bug in the polling script. The bytes are unaffected, but its "received" time in the
   timing table does not show real-time delivery.
3. **No clear-air VCP 215 volume.** See Differences from the plan text.
4. **Unreadable edge cases.** Py-ART cannot read the `_MDM` file, the 311-byte TBWI file or any single
   chunk. MetPy reads them but finds no sweeps (MDM, TBWI, Start chunk) or 120 radials (intermediate
   chunks). These entries exist to test edge cases, not to compare decoded values.
5. **Headline features missing from three partial trims.** The first N radials of each sweep miss them:
   - The KEWX 2016 fixture has no hail-signature gates.
   - The KLIX 2021 fixture has only the outer rain bands of Ida.
   - The PGUA fixture has Mawar rain bands but not the eyewall.

   The KDVN and KLIX 2021 fixtures also keep only 120 radials (60 deg) per sweep. The full volumes are
   download entries.
6. **One record container in the trimmed files.** Every trimmed file uses LDM bzip2 records. The
   gzip-of-uncompressed-records container of 1991-2016 archive objects is covered only by download
   entries. The four ARCHIVE2 and AR2V0001 trimmed files hold real Message 1 bytes inside bzip2 LDM
   records, a layout the archive never used for those headers.
7. **Trimmed files are not byte prefixes of their sources.** The last control word is negated, so 4
   bytes differ. The last kept radial also keeps its original radial status (1 or 2), not
   end-of-volume (4), because no message bytes are edited.
8. **First online test run downloads about 150 MB.** The test that reproduces each fixture from its
   source volume calls `path()` on all 16 sources, and it takes about 30 s in the debug profile.

### Other formats

1. **No natively written classic CfRadial file with an UNLIMITED `time`
   dimension.** The record-interleaved read path is covered only by the xsapr
   classic conversion, which keeps the Py-ART file's unlimited `time`. BowEcho
   searched and found every public CfRadial sample to be netCDF-4. This search
   found only the fixed-dimension Irene files.
2. **No real ODIM file with a contiguous (unchunked) data layout.** Every
   public PVOL checked uses chunked+gzip.
3. **Multi-elevation DORADE only as head trims.** NOXP 2009 June volumes and
   FARM DOW6 volumes are real multi-sweep sets but their sweeps are 3.7-68 MB
   inside large tar.gz or zip archives, which the testdata crate cannot fetch
   member by member. The committed multi-elevation set is three 6-ray head
   trims of one NOXP volume (0.5, 1.0, 2.0 deg) and a zip of them; the other
   committed DORADE sweeps are single-tilt.
4. **No full DORADE RHI sweep.** The DOW6 RHI is trimmed to rays 0-40 (30.0
   to 13.0 deg). The full 6.3 MB member can be read from the Zenodo zip with
   range requests.
5. **No public source for the COW2 fixture.** The original sweep file and its
   URL are unknown, and so is its license.
6. **Expired URLs**: espdg (ORD 24h bucket) and the IMGW datastore. The
   committed copies are the only copies.
7. **License review needed** for the JMA/NICT data, the COW2 file and the
   N42RF sweepfiles (see Licensing and attribution under Other formats).
8. **Level III VWP placement.** `l3-kbmx-19980416-0006-nvw` and its archive
   entry use the `l3-` prefix. Branch `level3` (checked at d66e163, 216
   entries) has no id or sha256 in common with them. They may move to the
   Level III manifest when `level3` merges.
9. **No real mobile-radar deployment zip.** The CSWR/FARM deployment zips
   BowEcho was written against (`DORADE/<radar>/...` and `GR2 MSG31/...`
   directories, several radars) are not public at a usable size. The zip in
   the corpus is a container conversion of one NOXP archive directory, and
   the several-radars case is tested with loose sweep files.

### Integration

1. **Level III is not here yet.** The Level III corpus (`testdata/level3/manifest.toml`) is on branch
   `level3`. When it merges, regenerate the index below. Its tags use different words for the same
   things (for example `radar:nexrad` where Level II uses `radar:wsr-88d`), and it does not use `site:`
   tags.
2. **No synthetic-data enforcement test.** Spec section 5 calls for a test that fails when a test module
   builds radar bytes synthetically. It is not in this crate yet; the plan assigns it to wave 2
   stream C.
3. **`Cargo.lock` is not committed on this branch.** `main` tracks its own `Cargo.lock`, and a second
   copy added here would conflict when the branches merge. After merging, cargo adds the testdata
   dependencies (bzip2, flate2 with zlib-rs, sha2, toml, ureq with rustls) to the lock file.
4. **ureq's rustls feature pulls in `ring`,** which has a C build step. The plan's rustls exception
   covers this, but a scan for C dependencies will list it.

## Scattering inputs

These entries are in `testdata/scattering/manifest.toml`, committed under `testdata/files/scattering/`.
They are not radar observations: they are the generator and model artifacts
`crates/recast-radar-scattering` consumes, added in plan task C.2 (group core-data-scattering) so that
its lookup-table, PSD-integration and P3-table tests read real inputs instead of analytic fixtures.

| id | C/D | bytes | what it covers | source |
|---|---|---|---|---|
| `tmatrix-lut-rain-sband-pytmatrix-0.3.3` (+ `-config`, `-manifest`) | C | 9108 (+ 3151, 4595) | schema-1 LUT `conventional-liquid-rain-sband-pytmatrix-0.3.3-unvalidated-v1`: 16 diameters 0.3-7 mm x 3 axis ratios x singleton 2.7008 GHz x singleton 0 deg, 48 nodes; the exact generator config its header hashes; the generator manifest with every SHA-256 | `crates/recast-radar-scattering/tools/pytmatrix-0.3.3` run (`run_all.ps1`, locked Docker image) recorded in radar-bow `research_only_assets/tmatrix/pytmatrix-0.3.3` |
| `tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3` (+ `-config`, `-manifest`) | C | 12947 (+ 3882, 5000) | LUT `conventional-dry-ice-spheroids-sband-pytmatrix-0.3.3-unvalidated-v1`: 29 diameters 0.1-50 mm x 3 axis ratios, 87 nodes, Gaussian 20-degree canting, Schiller-Naumann fall speeds | same run |
| `tmatrix-lut-property-dry-oblate-sband-trim`, `-wet-oblate-`, `-rain-` (+ `-config`, `-trim`) | C | 147472, 165506, 25651 | subsets of three property-bundle tables (P3/ISHMAEL dry oblate, wet oblate, standalone and residual rain; 2.8 GHz), every kept node's outputs copied byte for byte and every axis end point kept, for the research-runtime tests; the rewritten config each header hashes; the trim record naming the source table's SHA-256 | `tools/trim_tmatrix_lut.py` on the tables in radar-bow `research_only_assets/tmatrix/pytmatrix-0.3.3` |
| `tmatrix-held-out-interpolation-report-v10`, `tmatrix-held-out-nodes-v10` | C | 186036, 17067 | the post-freeze held-out check of all 8 generated tables: nodes absent from the grids (public seed), direct PyTMatrix recomputation, the validator's multilinear interpolation and per-component errors; the node request it answers | radar-bow `validation/tmatrix/refined_grid_v10_post_freeze_held_out_*.json` |
| `wrf-p3-lookup-table-1-v5.4-2momI`, `-3momI` | D | 1606038, 17886038 | the official WRF P3 v5.4 lookup tables at commit f52c197 (the bytes the crate pins by length and SHA-256) | github.com/wrf-model/WRF (public domain) |
| `wrf-p3-lookup-table-1-v5.4-{2,3}momI-first-block` | C, derived | 80338, 81338 | the byte prefix through the 1552nd line feed of each table: header, separator, the first (density 1, rime 1[, shape 1]) block of 50 main records and 1500 collision records | `head -n 1552` of the download |

The tables above are `research_only_unvalidated` by their own headers; committing them makes them test
inputs, not validated science. The golden script `tools/scattering_golden.py` reads the LUT bytes with
`struct`, cross-checks the report's interpolation with numpy on the payload, and reads the P3 records
from the text.

## Tag vocabulary

Tags are free-form strings, usually `namespace:value`. They are case-sensitive, and site tags are
lowercase.

### Level II

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
| `trim` | source volume of a trimmed fixture |
| `trimmed` | trimmed fixture made by `trim-level2` |
| `split-cut` | trimmed fixture keeps the first split cut (surveillance + Doppler sweeps) |
| `partial-sweeps` | trimmed fixture keeps only the first N radials of each kept sweep |
| `provider:aws`, `bucket:*` | source |
| `chunk-volume:kiwa-307`, `chunk:start\|intermediate\|end`, `elev:N`, `radial-status:N` | real-time chunk facts |
| `sequence:kdvn-20200810` | one of the four consecutive KDVN derecho volumes 17:57-18:17Z (and, in the other manifest, their Level III STI products) |

### Other formats

Tags used in this manifest:

- `provider:*` and `license:*` give the source and license.
- `carryover:bowecho` marks files carried over from BowEcho.
- `era:YYYY`, `site:*`, `country:*`, `network:*` and `project:*` identify
  time and place.
- `object:pvol|image`, `odim:h5rad-*`, `hdf5:*` and `dtype:*` describe
  ODIM_H5 and HDF5 structure.
- `container:netcdf3-classic|netcdf4`, `cfradial:*` and `writer:*` describe
  netCDF files.
- `dorade:rle|uncompressed|csfd|celv` and `endian:*` describe DORADE files.
- `scan:ppi|rhi|sector|sur|air` gives the scan type (`air`: the DORADE
  airborne scan of a tail radar).
- `platform:mobile|aircraft` marks radars that are not fixed.
- `moments:*` lists the fields.
- `regime:*` and `echo:*` describe the weather.
- `quirk:*` marks writer oddities that decoders must handle.
- `replaces:cfrad_synth|odim_pvol_synth` marks the real replacements for the
  synthetic fixtures.
- `derived` and `derivation:container-conversion|subset|head-trim|archive-member|prefix`
  mark derived files.
- `archive` and `contains:*` mark source archives.
- `sweepset:*` groups consecutive DORADE sweeps, or the sweeps of one volume.
- Format `zip` is a zip archive; format `zip-local-member` is a single ZIP
  local-file record without a central directory (`quirk:zip-local-member-stream`).
- `part-of-scan` and `split-scan:*` group the per-quantity ODIM files of one scan.

### Scattering inputs

- `generator:pytmatrix-0.3.3`, `lut:schema-1|generator-config|generator-manifest`, `table:*`,
  `band:s` and `status:research-only-unvalidated` describe the PyTMatrix tables.
- `validation:held-out-interpolation|held-out-nodes` and `golden-source` mark the held-out check.
- `provider:wrf-model`, `p3:v5.4`, `p3:two-moment|three-moment` and `text` describe the P3 tables.

## Manifest index

Everything below the marker is generated from the manifests by
`crates/recast-radar-testdata/tests/corpus_doc.rs`. Do not edit it by hand. Regenerate it with
`RECAST_RADAR_TESTDATA_BLESS=1 cargo test -p recast-radar-testdata --test corpus_doc`.

<!-- BEGIN GENERATED: crates/recast-radar-testdata/tests/corpus_doc.rs -->

### Totals

| manifest | entries | committed files | committed bytes | download files | download bytes |
|---|---:|---:|---:|---:|---:|
| `testdata/manifest.toml` | 0 | 0 | 0 | 0 | 0 |
| `testdata/feeds/manifest.toml` | 5 | 4 | 224,523 | 1 | 20,616,906 |
| `testdata/fuzz/manifest.toml` | 14 | 14 | 1,988,264 | 0 | 0 |
| `testdata/level2/manifest.toml` | 125 | 24 | 10,426,272 | 101 | 332,296,252 |
| `testdata/level3/manifest.toml` | 269 | 269 | 8,200,268 | 0 | 0 |
| `testdata/other/manifest.toml` | 75 | 62 | 27,023,383 | 13 | 187,735,375 |
| `testdata/scattering/manifest.toml` | 21 | 19 | 763,095 | 2 | 19,492,076 |
| **all** | **509** | **392** | **48,625,805** | **117** | **560,140,609** |

| format | committed | download |
|---|---:|---:|
| `brslut-v1` | 5 | 0 |
| `cfradial1` | 10 | 2 |
| `cfradial2` | 4 | 1 |
| `dorade` | 12 | 2 |
| `gr2-polling-dir-list` | 1 | 0 |
| `gr2-polling-site-config` | 2 | 0 |
| `http-request-head` | 3 | 0 |
| `jma-grib2-tar` | 4 | 4 |
| `json` | 12 | 0 |
| `nexrad-level2` | 22 | 34 |
| `nexrad-level2-chunk` | 8 | 67 |
| `nexrad-level2-feed` | 1 | 1 |
| `nexrad-level3` | 277 | 0 |
| `odim-h5` | 26 | 0 |
| `tar-gz` | 0 | 2 |
| `tar-z` | 0 | 2 |
| `wmo-text` | 1 | 0 |
| `wrf-p3-lookup-table` | 2 | 2 |
| `zip` | 1 | 0 |
| `zip-local-member` | 1 | 0 |

### Entries

#### `testdata/manifest.toml`

No entries.

#### `testdata/feeds/manifest.toml`

| id | format | where | bytes | derived from |
|---|---|---|---:|---|
| `ndswc-kxwa-20260924-214316` | `nexrad-level2-feed` | download (ephemeral URL) | 20,616,906 |  |
| `ndswc-kxwa-20260924-214316-head41` | `nexrad-level2-feed` | committed `files/feeds/ndswc/KXWA20260924_214316_V06.head41.ar2v` | 181,437 | `ndswc-kxwa-20260924-214316` |
| `polling-ndswc-kxwa-dir-list-20260925` | `gr2-polling-dir-list` | committed `files/other/polling/ndswc-KXWA-dir.list-20260925T0318Z` | 40,635 |  |
| `polling-iem-config-cfg-20260926` | `gr2-polling-site-config` | committed `files/other/polling/iem-config.cfg-20260926T0213Z` | 2,440 |  |
| `polling-ewr-laredo-grlevel2-cfg-20260925` | `gr2-polling-site-config` | committed `files/other/polling/ewr-Laredo-grlevel2.cfg-20260925T0301Z` | 11 |  |

#### `testdata/fuzz/manifest.toml`

| id | format | where | bytes | derived from |
|---|---|---|---:|---|
| `fuzz-odim-hdf5-local-heap-name-offset-overflow` | `odim-h5` | committed `files/fuzz/odim/crash-hdf5-local-heap-name-offset-overflow` | 1,640 | `odim-imgw-ram-20260711-0015-kdp-max` |
| `fuzz-hdf5-chunk-offset-overflow` | `odim-h5` | committed `files/fuzz/hdf5/crash-chunk-offset-overflow` | 532,077 | `odim-dkrom-20260820-1130-pvol-h5latest-trim` |
| `fuzz-cfradial-overlapping-sweep-ray-ranges` | `cfradial1` | committed `files/fuzz/cfradial/oom-overlapping-sweep-ray-ranges` | 868,481 | `cfrad1-irene-sr2-20110827-120420-sur-sweeps01` |
| `fuzz-level2-writer-nexrad-moment-nan-scale` | `nexrad-level2` | committed `files/fuzz/level2_writer/crash-nexrad-moment-nan-scale` | 22,157 | `l2-kiwa-20260917-003629` |
| `fuzz-writers-l2-sweep-without-gates` | `nexrad-level2` | committed `files/fuzz/writers/crash-l2-sweep-without-gates` | 130 | `l2-kvnx-20110315-000203` |
| `fuzz-writers-l2-one-gate-sweep` | `nexrad-level2` | committed `files/fuzz/writers/crash-l2-one-gate-sweep` | 60,461 | `l2-kvnx-20110315-000203` |
| `fuzz-writers-l2-odim-rstart-beyond-20-km` | `nexrad-level2` | committed `files/fuzz/writers/crash-l2-odim-rstart-beyond-20-km` | 312 | `l2-kvnx-20110315-000203` |
| `fuzz-writers-dorade-ray-without-time` | `dorade` | committed `files/fuzz/writers/crash-dorade-ray-without-time` | 37,380 | `dorade-cow2-20260521-225514-sur-head24` |
| `fuzz-writers-l2-empty-field-name` | `nexrad-level2` | committed `files/fuzz/writers/crash-l2-empty-field-name` | 98,116 | `l2-kvnx-20110315-000203` |
| `fuzz-writers-odim-gate-spacing-below-float` | `odim-h5` | committed `files/fuzz/writers/crash-odim-gate-spacing-below-float` | 322,411 | `odim-itdes-20260924-2135-pvol-class` |
| `fuzz-writers-dorade-absent-rows-without-fill` | `dorade` | committed `files/fuzz/writers/crash-dorade-absent-rows-without-fill` | 31,428 | `dorade-noxp-20090501-190244-ppi` |
| `fuzz-writers-cfradial1-ray-time-near-float-max` | `cfradial1` | committed `files/fuzz/writers/crash-cfradial1-ray-time-near-float-max` | 13,624 | `cfrad1-xsapr-sgp-20110520-ppi-classic` |
| `fuzz-level3-rcm-centroid-non-ascii` | `nexrad-level3` | committed `files/fuzz/level3/crash-rcm-centroid-non-ascii` | 23 | `l3-fws-rcm-19950517-2310` |
| `fuzz-level2-records-volume-header-date-overflow` | `nexrad-level2` | committed `files/fuzz/level2_records/crash-volume-header-date-overflow` | 24 | `l2-tbwi-20230601-175101-stub` |

#### `testdata/level2/manifest.toml`

| id | format | where | bytes | derived from |
|---|---|---|---:|---|
| `l2-ktlx-19910605-162126` | `nexrad-level2` | download | 1,439,817 |  |
| `l2-ktlx-19990503-230052` | `nexrad-level2` | download | 9,424 |  |
| `l2-ktlx-19990504-002218` | `nexrad-level2` | download | 2,473,255 |  |
| `l2-ktlx-20030508-221041` | `nexrad-level2` | download | 1,652,210 |  |
| `l2-klix-20050829-130035` | `nexrad-level2` | download | 4,750,949 |  |
| `l2-kvwx-20080415-235337` | `nexrad-level2` | download | 175,233 |  |
| `l2-kpah-20080415-235014` | `nexrad-level2` | download | 294,612 |  |
| `l2-kdmx-20080525-205148` | `nexrad-level2` | download | 3,302,990 |  |
| `l2-kvnx-20110315-000203` | `nexrad-level2` | download | 1,260,464 |  |
| `l2-ktlx-20130520-201643` | `nexrad-level2` | download | 9,548,976 |  |
| `l2-kgwx-20130601-235640` | `nexrad-level2` | download | 4,502,847 |  |
| `l2-koax-20140616-205305` | `nexrad-level2` | download | 11,549,918 |  |
| `l2-kewx-20160413-022531` | `nexrad-level2` | download | 15,533,296 |  |
| `l2-kdvn-20200810-175718` | `nexrad-level2` | download | 15,760,259 |  |
| `l2-kdvn-20200810-180401` | `nexrad-level2` | download | 16,063,887 |  |
| `l2-kdvn-20200810-181043` | `nexrad-level2` | download | 16,341,122 |  |
| `l2-kdvn-20200810-181724` | `nexrad-level2` | download | 16,483,359 |  |
| `l2-klix-20210829-180425` | `nexrad-level2` | download | 17,635,589 |  |
| `l2-klix-20210829-173117` | `nexrad-level2` | download | 17,996,934 |  |
| `l2-klix-20210829-175748` | `nexrad-level2` | download | 17,833,396 |  |
| `l2-klix-20210829-175748-mdm` | `nexrad-level2` | download | 812,213 |  |
| `l2-kbox-20220129-150537` | `nexrad-level2` | download | 12,070,983 |  |
| `l2-tjua-20220918-190621` | `nexrad-level2` | download | 18,452,163 |  |
| `l2-kdgx-20230325-010651` | `nexrad-level2` | download | 14,919,420 |  |
| `l2-kmaf-20230331-230843` | `nexrad-level2` | download | 2,250,716 |  |
| `l2-tstl-20230331-230314` | `nexrad-level2` | download | 3,524,886 |  |
| `l2-pgua-20230524-030945` | `nexrad-level2` | download | 20,463,715 |  |
| `l2-tbwi-20230601-175101-stub` | `nexrad-level2` | download | 311 |  |
| `l2-kmtx-20240301-212827` | `nexrad-level2` | download | 9,543,462 |  |
| `l2-ktlx-20240315-000217` | `nexrad-level2` | download | 10,786,581 |  |
| `l2-ktlx-20240515-000014` | `nexrad-level2` | download | 4,358,239 |  |
| `l2-pahg-20250909-212549` | `nexrad-level2` | download | 12,382,118 |  |
| `l2-kilx-20260418-013553` | `nexrad-level2` | download | 23,317,197 |  |
| `l2-kiwa-20260917-003629` | `nexrad-level2` | download | 12,653,945 |  |
| `l2chunk-kiwa-307-20260917-003629-001-s` | `nexrad-level2-chunk` | committed `files/level2-chunks/KIWA-307-20260917-003629-001-S` | 2,455 |  |
| `l2chunk-kiwa-307-20260917-003629-002-i` | `nexrad-level2-chunk` | committed `files/level2-chunks/KIWA-307-20260917-003629-002-I` | 162,173 |  |
| `l2chunk-kiwa-307-20260917-003629-003-i` | `nexrad-level2-chunk` | committed `files/level2-chunks/KIWA-307-20260917-003629-003-I` | 337,551 |  |
| `l2chunk-kiwa-307-20260917-003629-004-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 366,778 |  |
| `l2chunk-kiwa-307-20260917-003629-005-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 279,908 |  |
| `l2chunk-kiwa-307-20260917-003629-006-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 177,861 |  |
| `l2chunk-kiwa-307-20260917-003629-007-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 380,987 |  |
| `l2chunk-kiwa-307-20260917-003629-008-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 66,306 |  |
| `l2chunk-kiwa-307-20260917-003629-009-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 125,773 |  |
| `l2chunk-kiwa-307-20260917-003629-010-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 123,597 |  |
| `l2chunk-kiwa-307-20260917-003629-011-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 63,918 |  |
| `l2chunk-kiwa-307-20260917-003629-012-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 81,280 |  |
| `l2chunk-kiwa-307-20260917-003629-013-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 100,323 |  |
| `l2chunk-kiwa-307-20260917-003629-014-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 253,598 |  |
| `l2chunk-kiwa-307-20260917-003629-015-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 341,611 |  |
| `l2chunk-kiwa-307-20260917-003629-016-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 377,442 |  |
| `l2chunk-kiwa-307-20260917-003629-017-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 164,123 |  |
| `l2chunk-kiwa-307-20260917-003629-018-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 370,055 |  |
| `l2chunk-kiwa-307-20260917-003629-019-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 269,376 |  |
| `l2chunk-kiwa-307-20260917-003629-020-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 119,115 |  |
| `l2chunk-kiwa-307-20260917-003629-021-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 134,224 |  |
| `l2chunk-kiwa-307-20260917-003629-022-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 117,914 |  |
| `l2chunk-kiwa-307-20260917-003629-023-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 65,284 |  |
| `l2chunk-kiwa-307-20260917-003629-024-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 135,034 |  |
| `l2chunk-kiwa-307-20260917-003629-025-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 77,602 |  |
| `l2chunk-kiwa-307-20260917-003629-026-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 323,424 |  |
| `l2chunk-kiwa-307-20260917-003629-027-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 334,679 |  |
| `l2chunk-kiwa-307-20260917-003629-028-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 297,346 |  |
| `l2chunk-kiwa-307-20260917-003629-029-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 255,521 |  |
| `l2chunk-kiwa-307-20260917-003629-030-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 343,146 |  |
| `l2chunk-kiwa-307-20260917-003629-031-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 212,012 |  |
| `l2chunk-kiwa-307-20260917-003629-032-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 135,914 |  |
| `l2chunk-kiwa-307-20260917-003629-033-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 138,239 |  |
| `l2chunk-kiwa-307-20260917-003629-034-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 77,381 |  |
| `l2chunk-kiwa-307-20260917-003629-035-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 132,871 |  |
| `l2chunk-kiwa-307-20260917-003629-036-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 118,310 |  |
| `l2chunk-kiwa-307-20260917-003629-037-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 84,753 |  |
| `l2chunk-kiwa-307-20260917-003629-038-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 249,168 |  |
| `l2chunk-kiwa-307-20260917-003629-039-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 216,307 |  |
| `l2chunk-kiwa-307-20260917-003629-040-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 194,402 |  |
| `l2chunk-kiwa-307-20260917-003629-041-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 371,754 |  |
| `l2chunk-kiwa-307-20260917-003629-042-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 254,609 |  |
| `l2chunk-kiwa-307-20260917-003629-043-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 197,436 |  |
| `l2chunk-kiwa-307-20260917-003629-044-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 370,586 |  |
| `l2chunk-kiwa-307-20260917-003629-045-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 162,309 |  |
| `l2chunk-kiwa-307-20260917-003629-046-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 336,508 |  |
| `l2chunk-kiwa-307-20260917-003629-047-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 123,258 |  |
| `l2chunk-kiwa-307-20260917-003629-048-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 51,518 |  |
| `l2chunk-kiwa-307-20260917-003629-049-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 91,116 |  |
| `l2chunk-kiwa-307-20260917-003629-050-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 92,113 |  |
| `l2chunk-kiwa-307-20260917-003629-051-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 72,425 |  |
| `l2chunk-kiwa-307-20260917-003629-052-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 127,664 |  |
| `l2chunk-kiwa-307-20260917-003629-053-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 183,525 |  |
| `l2chunk-kiwa-307-20260917-003629-054-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 190,813 |  |
| `l2chunk-kiwa-307-20260917-003629-055-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 233,064 |  |
| `l2chunk-kiwa-307-20260917-003629-056-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 163,034 |  |
| `l2chunk-kiwa-307-20260917-003629-057-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 142,902 |  |
| `l2chunk-kiwa-307-20260917-003629-058-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 219,137 |  |
| `l2chunk-kiwa-307-20260917-003629-059-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 137,838 |  |
| `l2chunk-kiwa-307-20260917-003629-060-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 120,720 |  |
| `l2chunk-kiwa-307-20260917-003629-061-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 187,777 |  |
| `l2chunk-kiwa-307-20260917-003629-062-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 116,933 |  |
| `l2chunk-kiwa-307-20260917-003629-063-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 107,349 |  |
| `l2chunk-kiwa-307-20260917-003629-064-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 120,317 |  |
| `l2chunk-kiwa-307-20260917-003629-065-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 101,669 |  |
| `l2chunk-kiwa-307-20260917-003629-066-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 103,265 |  |
| `l2chunk-kiwa-307-20260917-003629-067-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 80,991 |  |
| `l2chunk-kiwa-307-20260917-003629-068-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 120,857 |  |
| `l2chunk-kiwa-307-20260917-003629-069-i` | `nexrad-level2-chunk` | download (ephemeral URL) | 152,619 |  |
| `l2chunk-kiwa-307-20260917-003629-070-e` | `nexrad-level2-chunk` | download (ephemeral URL) | 112,078 |  |
| `l2chunk-tlas-998-20260917-012843-001-s` | `nexrad-level2-chunk` | committed `files/level2-chunks/TLAS-998-20260917-012843-001-S` | 263 |  |
| `l2chunk-tlas-999-20260917-013443-001-s` | `nexrad-level2-chunk` | committed `files/level2-chunks/TLAS-999-20260917-013443-001-S` | 265 |  |
| `l2chunk-tlas-3-20260917-015242-001-s` | `nexrad-level2-chunk` | committed `files/level2-chunks/TLAS-3-20260917-015242-001-S` | 262 |  |
| `l2chunk-tlas-3-20260917-015242-002-i` | `nexrad-level2-chunk` | committed `files/level2-chunks/TLAS-3-20260917-015242-002-I` | 7,363 |  |
| `l2chunk-tlas-3-20260917-015242-003-i` | `nexrad-level2-chunk` | committed `files/level2-chunks/TLAS-3-20260917-015242-003-I` | 24,018 |  |
| `l2-ktlx-19910605-162126-trim` | `nexrad-level2` | committed `files/level2/KTLX19910605_162126.trim.V06` | 141,911 | `l2-ktlx-19910605-162126` |
| `l2-ktlx-19990504-002218-trim` | `nexrad-level2` | committed `files/level2/KTLX19990504_002218.trim.V06` | 191,887 | `l2-ktlx-19990504-002218` |
| `l2-ktlx-20030508-221041-trim` | `nexrad-level2` | committed `files/level2/KTLX20030508_221041.trim.V06` | 184,958 | `l2-ktlx-20030508-221041` |
| `l2-klix-20050829-130035-trim` | `nexrad-level2` | committed `files/level2/KLIX20050829_130035.trim.V06` | 241,543 | `l2-klix-20050829-130035` |
| `l2-kdmx-20080525-205148-trim` | `nexrad-level2` | committed `files/level2/KDMX20080525_205148.trim.V06` | 788,002 | `l2-kdmx-20080525-205148` |
| `l2-ktlx-20130520-201643-trim` | `nexrad-level2` | committed `files/level2/KTLX20130520_201643.trim.V06` | 793,144 | `l2-ktlx-20130520-201643` |
| `l2-koax-20140616-205305-trim` | `nexrad-level2` | committed `files/level2/KOAX20140616_205305.trim.V06` | 826,983 | `l2-koax-20140616-205305` |
| `l2-kewx-20160413-022531-trim` | `nexrad-level2` | committed `files/level2/KEWX20160413_022531.trim.V06` | 856,386 | `l2-kewx-20160413-022531` |
| `l2-kdvn-20200810-180401-trim` | `nexrad-level2` | committed `files/level2/KDVN20200810_180401.trim.V06` | 629,083 | `l2-kdvn-20200810-180401` |
| `l2-klix-20210829-180425-trim` | `nexrad-level2` | committed `files/level2/KLIX20210829_180425.trim.V06` | 440,556 | `l2-klix-20210829-180425` |
| `l2-kbox-20220129-150537-trim` | `nexrad-level2` | committed `files/level2/KBOX20220129_150537.trim.V06` | 998,852 | `l2-kbox-20220129-150537` |
| `l2-tstl-20230331-230314-trim` | `nexrad-level2` | committed `files/level2/TSTL20230331_230314.trim.V06` | 389,446 | `l2-tstl-20230331-230314` |
| `l2-pgua-20230524-030945-trim` | `nexrad-level2` | committed `files/level2/PGUA20230524_030945.trim.V06` | 960,321 | `l2-pgua-20230524-030945` |
| `l2-kmtx-20240301-212827-trim` | `nexrad-level2` | committed `files/level2/KMTX20240301_212827.trim.V06` | 884,380 | `l2-kmtx-20240301-212827` |
| `l2-ktlx-20240315-000217-trim` | `nexrad-level2` | committed `files/level2/KTLX20240315_000217.trim.V06` | 741,465 | `l2-ktlx-20240315-000217` |
| `l2-kilx-20260418-013553-trim` | `nexrad-level2` | committed `files/level2/KILX20260418_013553.trim.V06` | 823,005 | `l2-kilx-20260418-013553` |

#### `testdata/level3/manifest.toml`

| id | format | where | bytes | derived from |
|---|---|---|---:|---|
| `l3-abr-ftm-20110428-1331` | `nexrad-level3` | committed `files/level3/l3-abr-ftm-20110428-1331` | 151 |  |
| `l3-akc-nc1-20210730-055033` | `nexrad-level3` | committed `files/level3/l3-akc-nc1-20210730-055033` | 14,860 |  |
| `l3-akq-018-19940810-0835` | `nexrad-level3` | committed `files/level3/l3-akq-018-19940810-0835` | 9,078 |  |
| `l3-bmx-029-19940914-1621` | `nexrad-level3` | committed `files/level3/l3-bmx-029-19940914-1621` | 30,112 |  |
| `l3-byx-n0q-20150124-2106` | `nexrad-level3` | committed `files/level3/l3-byx-n0q-20150124-2106` | 10,673 |  |
| `l3-cae-053-19940629-1906` | `nexrad-level3` | committed `files/level3/l3-cae-053-19940629-1906` | 8,968 |  |
| `l3-cae-n0r-19940629-1906` | `nexrad-level3` | committed `files/level3/l3-cae-n0r-19940629-1906` | 22,060 |  |
| `l3-cys-021-19941114-1107` | `nexrad-level3` | committed `files/level3/l3-cys-021-19941114-1107` | 7,628 |  |
| `l3-ddc-gsm-20200817-1000` | `nexrad-level3` | committed `files/level3/l3-ddc-gsm-20200817-1000` | 230 |  |
| `l3-ddc-n0q-20200817-0501` | `nexrad-level3` | committed `files/level3/l3-ddc-n0q-20200817-0501` | 43,024 |  |
| `l3-ddc-n0q-20200817-0503` | `nexrad-level3` | committed `files/level3/l3-ddc-n0q-20200817-0503` | 42,428 |  |
| `l3-den-tz0-20200804-2226` | `nexrad-level3` | committed `files/level3/l3-den-tz0-20200804-2226` | 119,104 |  |
| `l3-den-tz1-20200804-2226` | `nexrad-level3` | committed `files/level3/l3-den-tz1-20200804-2226` | 113,537 |  |
| `l3-den-tz2-20200804-2227` | `nexrad-level3` | committed `files/level3/l3-den-tz2-20200804-2227` | 97,690 |  |
| `l3-eax-gsm-20200817-0933` | `nexrad-level3` | committed `files/level3/l3-eax-gsm-20200817-0933` | 230 |  |
| `l3-eax-n0q-20200817-0401` | `nexrad-level3` | committed `files/level3/l3-eax-n0q-20200817-0401` | 60,202 |  |
| `l3-eax-n0q-20200817-0405` | `nexrad-level3` | committed `files/level3/l3-eax-n0q-20200817-0405` | 61,139 |  |
| `l3-ffc-n0q-20140407-1805` | `nexrad-level3` | committed `files/level3/l3-ffc-n0q-20140407-1805` | 29,142 |  |
| `l3-ftg-022-19940601-1917` | `nexrad-level3` | committed `files/level3/l3-ftg-022-19940601-1917` | 21,912 |  |
| `l3-ftg-026-19940930-0536` | `nexrad-level3` | committed `files/level3/l3-ftg-026-19940930-0536` | 39,144 |  |
| `l3-ftg-043-19940930-1849` | `nexrad-level3` | committed `files/level3/l3-ftg-043-19940930-1849` | 1,846 |  |
| `l3-ftg-044-19940930-1849` | `nexrad-level3` | committed `files/level3/l3-ftg-044-19940930-1849` | 4,838 |  |
| `l3-ftg-045-19940930-1849` | `nexrad-level3` | committed `files/level3/l3-ftg-045-19940930-1849` | 5,774 |  |
| `l3-ftg-046-19940930-1849` | `nexrad-level3` | committed `files/level3/l3-ftg-046-19940930-1849` | 4,016 |  |
| `l3-ftg-100-19940930-0102` | `nexrad-level3` | committed `files/level3/l3-ftg-100-19940930-0102` | 1,926 |  |
| `l3-ftg-102-19940930-1932` | `nexrad-level3` | committed `files/level3/l3-ftg-102-19940930-1932` | 2,946 |  |
| `l3-ftg-104-19940930-0850` | `nexrad-level3` | committed `files/level3/l3-ftg-104-19940930-0850` | 600 |  |
| `l3-ftg-107-19940930-0154` | `nexrad-level3` | committed `files/level3/l3-ftg-107-19940930-0154` | 4,182 |  |
| `l3-ftg-109-19940930-0102` | `nexrad-level3` | committed `files/level3/l3-ftg-109-19940930-0102` | 4,182 |  |
| `l3-ftg-n0b-20220304-1820` | `nexrad-level3` | committed `files/level3/l3-ftg-n0b-20220304-1820` | 149,921 |  |
| `l3-fws-dpa-19950517-2304` | `nexrad-level3` | committed `files/level3/l3-fws-dpa-19950517-2304` | 5,576 |  |
| `l3-fws-n0r-19950517-2304` | `nexrad-level3` | committed `files/level3/l3-fws-n0r-19950517-2304` | 19,090 |  |
| `l3-fws-n1p-19950517-2304` | `nexrad-level3` | committed `files/level3/l3-fws-n1p-19950517-2304` | 9,866 |  |
| `l3-fws-ncz-19950517-2304` | `nexrad-level3` | committed `files/level3/l3-fws-ncz-19950517-2304` | 7,512 |  |
| `l3-fws-nhi-19950517-1323` | `nexrad-level3` | committed `files/level3/l3-fws-nhi-19950517-1323` | 3,714 |  |
| `l3-fws-nme-19950517-2316` | `nexrad-level3` | committed `files/level3/l3-fws-nme-19950517-2316` | 3,420 |  |
| `l3-fws-now-19950517-2304` | `nexrad-level3` | committed `files/level3/l3-fws-now-19950517-2304` | 32,358 |  |
| `l3-fws-nss-19950517-2304` | `nexrad-level3` | committed `files/level3/l3-fws-nss-19950517-2304` | 2,290 |  |
| `l3-fws-nst-19950517-2304` | `nexrad-level3` | committed `files/level3/l3-fws-nst-19950517-2304` | 3,588 |  |
| `l3-fws-ntp-19950517-2304` | `nexrad-level3` | committed `files/level3/l3-fws-ntp-19950517-2304` | 14,674 |  |
| `l3-fws-nvw-19950517-2322` | `nexrad-level3` | committed `files/level3/l3-fws-nvw-19950517-2322` | 7,168 |  |
| `l3-fws-nwp-19950517-2304` | `nexrad-level3` | committed `files/level3/l3-fws-nwp-19950517-2304` | 306 |  |
| `l3-fws-rcm-19950517-2310` | `nexrad-level3` | committed `files/level3/l3-fws-rcm-19950517-2310` | 2,040 |  |
| `l3-fws-sup-19950517-2304` | `nexrad-level3` | committed `files/level3/l3-fws-sup-19950517-2304` | 1,470 |  |
| `l3-gjx-n0f-20200817-0551` | `nexrad-level3` | committed `files/level3/l3-gjx-n0f-20200817-0551` | 49,327 |  |
| `l3-gjx-naf-20200817-0551` | `nexrad-level3` | committed `files/level3/l3-gjx-naf-20200817-0551` | 45,661 |  |
| `l3-gjx-nbf-20200817-0551` | `nexrad-level3` | committed `files/level3/l3-gjx-nbf-20200817-0551` | 22,292 |  |
| `l3-gjx-nxf-20200817-0600` | `nexrad-level3` | committed `files/level3/l3-gjx-nxf-20200817-0600` | 54,126 |  |
| `l3-gjx-nyq-20220503-005356` | `nexrad-level3` | committed `files/level3/l3-gjx-nyq-20220503-005356` | 12,002 |  |
| `l3-grr-039-20011011-0631` | `nexrad-level3` | committed `files/level3/l3-grr-039-20011011-0631` | 7,744 |  |
| `l3-ilx-irm-19960419-2309` | `nexrad-level3` | committed `files/level3/l3-ilx-irm-19960419-2309` | 4,846 |  |
| `l3-ilx-ncz-19960419-2320` | `nexrad-level3` | committed `files/level3/l3-ilx-ncz-19960419-2320` | 11,206 |  |
| `l3-ilx-nhi-19960419-2303` | `nexrad-level3` | committed `files/level3/l3-ilx-nhi-19960419-2303` | 6,392 |  |
| `l3-ilx-nme-19960419-2303` | `nexrad-level3` | committed `files/level3/l3-ilx-nme-19960419-2303` | 5,842 |  |
| `l3-ilx-nst-19960419-2303` | `nexrad-level3` | committed `files/level3/l3-ilx-nst-19960419-2303` | 8,724 |  |
| `l3-ilx-ntv-19960419-2303` | `nexrad-level3` | committed `files/level3/l3-ilx-ntv-19960419-2303` | 1,974 |  |
| `l3-ilx-rcm-19960419-2309` | `nexrad-level3` | committed `files/level3/l3-ilx-rcm-19960419-2309` | 2,880 |  |
| `l3-ind-016-19940910-0647` | `nexrad-level3` | committed `files/level3/l3-ind-016-19940910-0647` | 19,492 |  |
| `l3-ind-017-19940910-1104` | `nexrad-level3` | committed `files/level3/l3-ind-017-19940910-1104` | 15,400 |  |
| `l3-ind-035-19941031-2138` | `nexrad-level3` | committed `files/level3/l3-ind-035-19941031-2138` | 27,950 |  |
| `l3-ind-042-19940910-1642` | `nexrad-level3` | committed `files/level3/l3-ind-042-19940910-1642` | 646 |  |
| `l3-ind-073-19940910-1555` | `nexrad-level3` | committed `files/level3/l3-ind-073-19940910-1555` | 1,304 |  |
| `l3-jan-024-19951003-1312` | `nexrad-level3` | committed `files/level3/l3-jan-024-19951003-1312` | 20,186 |  |
| `l3-jfk-tr0-20210120-154051` | `nexrad-level3` | committed `files/level3/l3-jfk-tr0-20210120-154051` | 19,702 |  |
| `l3-lot-050-19941031-1358` | `nexrad-level3` | committed `files/level3/l3-lot-050-19941031-1358` | 1,504 |  |
| `l3-lot-053-19941106-0246` | `nexrad-level3` | committed `files/level3/l3-lot-053-19941106-0246` | 8,784 |  |
| `l3-lot-055-19941031-1137` | `nexrad-level3` | committed `files/level3/l3-lot-055-19941031-1137` | 3,294 |  |
| `l3-lot-084-19931120-0721` | `nexrad-level3` | committed `files/level3/l3-lot-084-19931120-0721` | 6,070 |  |
| `l3-lot-101-19930824-0005` | `nexrad-level3` | committed `files/level3/l3-lot-101-19930824-0005` | 6,478 |  |
| `l3-lot-108-19930824-0005` | `nexrad-level3` | committed `files/level3/l3-lot-108-19930824-0005` | 1,058 |  |
| `l3-lot-irm-19941031-0503` | `nexrad-level3` | committed `files/level3/l3-lot-irm-19941031-0503` | 1,416 |  |
| `l3-lot-n0r-19941106-0246` | `nexrad-level3` | committed `files/level3/l3-lot-n0r-19941106-0246` | 28,222 |  |
| `l3-lot-nvw-19931120-0721` | `nexrad-level3` | committed `files/level3/l3-lot-nvw-19931120-0721` | 5,196 |  |
| `l3-lzk-h0c-20200814-0417` | `nexrad-level3` | committed `files/level3/l3-lzk-h0c-20200814-0417` | 514,319 |  |
| `l3-lzk-h0v-20200812-1309` | `nexrad-level3` | committed `files/level3/l3-lzk-h0v-20200812-1309` | 160,362 |  |
| `l3-lzk-h0w-20200812-1305` | `nexrad-level3` | committed `files/level3/l3-lzk-h0w-20200812-1305` | 142,943 |  |
| `l3-lzk-h0z-20200812-1318` | `nexrad-level3` | committed `files/level3/l3-lzk-h0z-20200812-1318` | 258,527 |  |
| `l3-lzk-ncz-19970301-1912` | `nexrad-level3` | committed `files/level3/l3-lzk-ncz-19970301-1912` | 14,100 |  |
| `l3-lzk-nme-19970301-2027` | `nexrad-level3` | committed `files/level3/l3-lzk-nme-19970301-2027` | 3,656 |  |
| `l3-lzk-ntv-19970301-2027` | `nexrad-level3` | committed `files/level3/l3-lzk-ntv-19970301-2027` | 1,974 |  |
| `l3-mci-dhr-20160526-2154` | `nexrad-level3` | committed `files/level3/l3-mci-dhr-20160526-2154` | 45,317 |  |
| `l3-mci-dpa-20160526-2154` | `nexrad-level3` | committed `files/level3/l3-mci-dpa-20160526-2154` | 5,597 |  |
| `l3-mci-dsp-20160526-2154` | `nexrad-level3` | committed `files/level3/l3-mci-dsp-20160526-2154` | 32,483 |  |
| `l3-mci-n1p-20160526-2154` | `nexrad-level3` | committed `files/level3/l3-mci-n1p-20160526-2154` | 6,919 |  |
| `l3-mci-ncr-20160526-2154` | `nexrad-level3` | committed `files/level3/l3-mci-ncr-20160526-2154` | 9,511 |  |
| `l3-mci-net-20160526-2154` | `nexrad-level3` | committed `files/level3/l3-mci-net-20160526-2154` | 942 |  |
| `l3-mci-nmd-20160526-2154` | `nexrad-level3` | committed `files/level3/l3-mci-nmd-20160526-2154` | 1,400 |  |
| `l3-mci-nst-20160526-2154` | `nexrad-level3` | committed `files/level3/l3-mci-nst-20160526-2154` | 4,836 |  |
| `l3-mci-ntp-20160526-2154` | `nexrad-level3` | committed `files/level3/l3-mci-ntp-20160526-2154` | 11,404 |  |
| `l3-mci-nvl-20160526-2154` | `nexrad-level3` | committed `files/level3/l3-mci-nvl-20160526-2154` | 979 |  |
| `l3-mci-nvw-20160526-2154` | `nexrad-level3` | committed `files/level3/l3-mci-nvw-20160526-2154` | 2,839 |  |
| `l3-mci-tr0-20160526-2154` | `nexrad-level3` | committed `files/level3/l3-mci-tr0-20160526-2154` | 44,583 |  |
| `l3-mci-tr1-20160526-2154` | `nexrad-level3` | committed `files/level3/l3-mci-tr1-20160526-2154` | 46,043 |  |
| `l3-mci-tr2-20160526-2154` | `nexrad-level3` | committed `files/level3/l3-mci-tr2-20160526-2154` | 48,616 |  |
| `l3-mci-tv0-20160526-2154` | `nexrad-level3` | committed `files/level3/l3-mci-tv0-20160526-2154` | 70,071 |  |
| `l3-mci-tv1-20160526-2154` | `nexrad-level3` | committed `files/level3/l3-mci-tv1-20160526-2154` | 70,832 |  |
| `l3-mci-tv2-20160526-2154` | `nexrad-level3` | committed `files/level3/l3-mci-tv2-20160526-2154` | 72,109 |  |
| `l3-mci-tzl-20160526-2154` | `nexrad-level3` | committed `files/level3/l3-mci-tzl-20160526-2154` | 158,498 |  |
| `l3-mlb-051-19941116-0335` | `nexrad-level3` | committed `files/level3/l3-mlb-051-19941116-0335` | 944 |  |
| `l3-okc-ncr-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-okc-ncr-20260622-080623` | 27,254 |  |
| `l3-okc-net-20220503-005210` | `nexrad-level3` | committed `files/level3/l3-okc-net-20220503-005210` | 1,774 |  |
| `l3-okc-nhi-20220503-005210` | `nexrad-level3` | committed `files/level3/l3-okc-nhi-20220503-005210` | 6,322 |  |
| `l3-okc-nmd-20260622-080640` | `nexrad-level3` | committed `files/level3/l3-okc-nmd-20260622-080640` | 1,686 |  |
| `l3-okc-nst-20260622-080640` | `nexrad-level3` | committed `files/level3/l3-okc-nst-20260622-080640` | 13,008 |  |
| `l3-okc-ntv-20220503-005210` | `nexrad-level3` | committed `files/level3/l3-okc-ntv-20220503-005210` | 3,458 |  |
| `l3-okc-nvl-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-okc-nvl-20260622-080623` | 1,740 |  |
| `l3-okc-nvw-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-okc-nvw-20260622-080623` | 12,348 |  |
| `l3-okc-rsl-20220517-085551` | `nexrad-level3` | committed `files/level3/l3-okc-rsl-20220517-085551` | 1,706 |  |
| `l3-okc-tv0-20260622-080547` | `nexrad-level3` | committed `files/level3/l3-okc-tv0-20260622-080547` | 93,715 |  |
| `l3-okc-tz0-20260622-080547` | `nexrad-level3` | committed `files/level3/l3-okc-tz0-20260622-080547` | 118,794 |  |
| `l3-okc-tzl-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-okc-tzl-20260622-080623` | 122,317 |  |
| `l3-rax-dta-20200818-0454` | `nexrad-level3` | committed `files/level3/l3-rax-dta-20200818-0454` | 104,033 |  |
| `l3-rax-nll-20220510-155126` | `nexrad-level3` | committed `files/level3/l3-rax-nll-20220510-155126` | 1,606 |  |
| `l3-rax-nyf-20200818-0001` | `nexrad-level3` | committed `files/level3/l3-rax-nyf-20200818-0001` | 74,611 |  |
| `l3-sgf-nme-20030504-2332` | `nexrad-level3` | committed `files/level3/l3-sgf-nme-20030504-2332` | 5,868 |  |
| `l3-sgf-ntv-20030504-2352` | `nexrad-level3` | committed `files/level3/l3-sgf-ntv-20030504-2352` | 3,358 |  |
| `l3-shv-nzq-20220503-005452` | `nexrad-level3` | committed `files/level3/l3-shv-nzq-20220503-005452` | 39,543 |  |
| `l3-slc-tv0-20160516-2359` | `nexrad-level3` | committed `files/level3/l3-slc-tv0-20160516-2359` | 39,764 |  |
| `l3-tlx-063-19940308-1930` | `nexrad-level3` | committed `files/level3/l3-tlx-063-19940308-1930` | 3,136 |  |
| `l3-tlx-064-19940308-1930` | `nexrad-level3` | committed `files/level3/l3-tlx-064-19940308-1930` | 2,892 |  |
| `l3-tlx-087-19940308-1939` | `nexrad-level3` | committed `files/level3/l3-tlx-087-19940308-1939` | 5,504 |  |
| `l3-tlx-101-20010503-0007` | `nexrad-level3` | committed `files/level3/l3-tlx-101-20010503-0007` | 1,962 |  |
| `l3-tlx-102-19990504-0052` | `nexrad-level3` | committed `files/level3/l3-tlx-102-19990504-0052` | 4,096 |  |
| `l3-tlx-103-20010503-0007` | `nexrad-level3` | committed `files/level3/l3-tlx-103-20010503-0007` | 1,550 |  |
| `l3-tlx-104-20010503-2355` | `nexrad-level3` | committed `files/level3/l3-tlx-104-20010503-2355` | 1,550 |  |
| `l3-tlx-daa-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-daa-20130520-2016` | 30,437 |  |
| `l3-tlx-daa-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-tlx-daa-20260622-080623` | 109,115 |  |
| `l3-tlx-dhr-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-dhr-20130520-2016` | 21,590 |  |
| `l3-tlx-dhr-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-tlx-dhr-20260622-080623` | 33,992 |  |
| `l3-tlx-dod-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-dod-20130520-2016` | 8,092 |  |
| `l3-tlx-dod-20220503-005231` | `nexrad-level3` | committed `files/level3/l3-tlx-dod-20220503-005231` | 16,888 |  |
| `l3-tlx-dpa-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-dpa-20130520-2016` | 8,406 |  |
| `l3-tlx-dpa-20260629-173638` | `nexrad-level3` | committed `files/level3/l3-tlx-dpa-20260629-173638` | 2,618 |  |
| `l3-tlx-dpr-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-dpr-20130520-2016` | 47,894 |  |
| `l3-tlx-dpr-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-tlx-dpr-20260622-080623` | 178,664 |  |
| `l3-tlx-dsd-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-dsd-20130520-2016` | 8,288 |  |
| `l3-tlx-dsd-20220503-005231` | `nexrad-level3` | committed `files/level3/l3-tlx-dsd-20220503-005231` | 25,857 |  |
| `l3-tlx-dsp-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-dsp-20130520-2016` | 6,556 |  |
| `l3-tlx-dsp-20260629-173638` | `nexrad-level3` | committed `files/level3/l3-tlx-dsp-20260629-173638` | 990 |  |
| `l3-tlx-dta-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-dta-20130520-2016` | 25,744 |  |
| `l3-tlx-dta-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-tlx-dta-20260622-080623` | 130,408 |  |
| `l3-tlx-du3-20130520-2008` | `nexrad-level3` | committed `files/level3/l3-tlx-du3-20130520-2008` | 26,906 |  |
| `l3-tlx-du3-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-tlx-du3-20260622-080623` | 124,521 |  |
| `l3-tlx-du6-20260622-120608` | `nexrad-level3` | committed `files/level3/l3-tlx-du6-20260622-120608` | 163,598 |  |
| `l3-tlx-dvl-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-dvl-20130520-2016` | 27,053 |  |
| `l3-tlx-dvl-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-tlx-dvl-20260622-080623` | 49,713 |  |
| `l3-tlx-eet-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-eet-20130520-2016` | 14,226 |  |
| `l3-tlx-eet-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-tlx-eet-20260622-080623` | 26,205 |  |
| `l3-tlx-gsm-20130520-2100` | `nexrad-level3` | committed `files/level3/l3-tlx-gsm-20130520-2100` | 134 |  |
| `l3-tlx-hhc-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-hhc-20130520-2016` | 9,290 |  |
| `l3-tlx-hhc-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-tlx-hhc-20260622-080623` | 12,659 |  |
| `l3-tlx-irm-19940308-1115` | `nexrad-level3` | committed `files/level3/l3-tlx-irm-19940308-1115` | 3,894 |  |
| `l3-tlx-n0b-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-tlx-n0b-20260622-080623` | 320,577 |  |
| `l3-tlx-n0c-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n0c-20130520-2016` | 71,233 |  |
| `l3-tlx-n0c-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-tlx-n0c-20260622-080623` | 92,276 |  |
| `l3-tlx-n0f-20220502-235926` | `nexrad-level3` | committed `files/level3/l3-tlx-n0f-20220502-235926` | 37,269 |  |
| `l3-tlx-n0g-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-tlx-n0g-20260622-080623` | 254,447 |  |
| `l3-tlx-n0h-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n0h-20130520-2016` | 20,319 |  |
| `l3-tlx-n0h-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-tlx-n0h-20260622-080623` | 22,943 |  |
| `l3-tlx-n0k-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n0k-20130520-2016` | 26,425 |  |
| `l3-tlx-n0k-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-tlx-n0k-20260622-080623` | 41,487 |  |
| `l3-tlx-n0m-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n0m-20130520-2016` | 5,990 |  |
| `l3-tlx-n0m-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-tlx-n0m-20260622-080623` | 5,990 |  |
| `l3-tlx-n0q-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n0q-20130520-2016` | 22,992 |  |
| `l3-tlx-n0q-20220503-005231` | `nexrad-level3` | committed `files/level3/l3-tlx-n0q-20220503-005231` | 28,120 |  |
| `l3-tlx-n0r-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n0r-20130520-2016` | 17,578 |  |
| `l3-tlx-n0r-20220908-131957` | `nexrad-level3` | committed `files/level3/l3-tlx-n0r-20220908-131957` | 33,432 |  |
| `l3-tlx-n0s-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n0s-20130520-2016` | 17,058 |  |
| `l3-tlx-n0s-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-tlx-n0s-20260622-080623` | 19,212 |  |
| `l3-tlx-n0u-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n0u-20130520-2016` | 55,129 |  |
| `l3-tlx-n0u-20220503-005231` | `nexrad-level3` | committed `files/level3/l3-tlx-n0u-20220503-005231` | 71,080 |  |
| `l3-tlx-n0v-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n0v-20130520-2016` | 17,474 |  |
| `l3-tlx-n0v-20220908-131957` | `nexrad-level3` | committed `files/level3/l3-tlx-n0v-20220908-131957` | 30,688 |  |
| `l3-tlx-n0x-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n0x-20130520-2016` | 78,242 |  |
| `l3-tlx-n0x-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-tlx-n0x-20260622-080623` | 122,885 |  |
| `l3-tlx-n0z-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n0z-20130520-2016` | 14,938 |  |
| `l3-tlx-n0z-20220908-131957` | `nexrad-level3` | committed `files/level3/l3-tlx-n0z-20220908-131957` | 21,484 |  |
| `l3-tlx-n1c-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n1c-20130520-2016` | 61,451 |  |
| `l3-tlx-n1h-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n1h-20130520-2016` | 17,163 |  |
| `l3-tlx-n1k-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n1k-20130520-2016` | 25,889 |  |
| `l3-tlx-n1m-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n1m-20130520-2016` | 5,990 |  |
| `l3-tlx-n1p-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n1p-20130520-2016` | 11,756 |  |
| `l3-tlx-n1p-20260629-173638` | `nexrad-level3` | committed `files/level3/l3-tlx-n1p-20260629-173638` | 8,560 |  |
| `l3-tlx-n1q-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n1q-20130520-2016` | 20,411 |  |
| `l3-tlx-n1s-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n1s-20130520-2016` | 14,808 |  |
| `l3-tlx-n1u-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n1u-20130520-2016` | 48,527 |  |
| `l3-tlx-n1x-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n1x-20130520-2016` | 69,671 |  |
| `l3-tlx-n2c-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n2c-20130520-2016` | 54,659 |  |
| `l3-tlx-n2h-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n2h-20130520-2016` | 19,408 |  |
| `l3-tlx-n2k-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n2k-20130520-2016` | 28,592 |  |
| `l3-tlx-n2m-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n2m-20130520-2016` | 5,990 |  |
| `l3-tlx-n2q-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n2q-20130520-2016` | 22,246 |  |
| `l3-tlx-n2s-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n2s-20130520-2016` | 15,686 |  |
| `l3-tlx-n2u-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n2u-20130520-2016` | 51,148 |  |
| `l3-tlx-n2x-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n2x-20130520-2016` | 62,547 |  |
| `l3-tlx-n3c-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n3c-20130520-2016` | 57,702 |  |
| `l3-tlx-n3h-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n3h-20130520-2016` | 20,120 |  |
| `l3-tlx-n3k-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n3k-20130520-2016` | 29,336 |  |
| `l3-tlx-n3m-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n3m-20130520-2016` | 5,990 |  |
| `l3-tlx-n3p-20130520-2012` | `nexrad-level3` | committed `files/level3/l3-tlx-n3p-20130520-2012` | 9,312 |  |
| `l3-tlx-n3p-20220503-011226` | `nexrad-level3` | committed `files/level3/l3-tlx-n3p-20220503-011226` | 15,516 |  |
| `l3-tlx-n3q-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n3q-20130520-2016` | 23,416 |  |
| `l3-tlx-n3s-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n3s-20130520-2016` | 16,562 |  |
| `l3-tlx-n3u-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n3u-20130520-2016` | 56,204 |  |
| `l3-tlx-n3x-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-n3x-20130520-2016` | 66,689 |  |
| `l3-tlx-nac-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nac-20130520-2016` | 65,840 |  |
| `l3-tlx-nah-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nah-20130520-2016` | 18,296 |  |
| `l3-tlx-nak-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nak-20130520-2016` | 26,851 |  |
| `l3-tlx-nam-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nam-20130520-2016` | 5,990 |  |
| `l3-tlx-naq-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-naq-20130520-2016` | 21,646 |  |
| `l3-tlx-nau-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nau-20130520-2016` | 51,904 |  |
| `l3-tlx-nax-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nax-20130520-2016` | 73,484 |  |
| `l3-tlx-nbc-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nbc-20130520-2016` | 53,131 |  |
| `l3-tlx-nbh-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nbh-20130520-2016` | 19,213 |  |
| `l3-tlx-nbk-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nbk-20130520-2016` | 28,426 |  |
| `l3-tlx-nbm-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nbm-20130520-2016` | 5,990 |  |
| `l3-tlx-nbq-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nbq-20130520-2016` | 21,482 |  |
| `l3-tlx-nbu-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nbu-20130520-2016` | 48,186 |  |
| `l3-tlx-nbx-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nbx-20130520-2016` | 60,672 |  |
| `l3-tlx-nc1-20130520-2354` | `nexrad-level3` | committed `files/level3/l3-tlx-nc1-20130520-2354` | 9,562 |  |
| `l3-tlx-nc2-20130520-2354` | `nexrad-level3` | committed `files/level3/l3-tlx-nc2-20130520-2354` | 9,062 |  |
| `l3-tlx-nc3-20130520-2354` | `nexrad-level3` | committed `files/level3/l3-tlx-nc3-20130520-2354` | 8,792 |  |
| `l3-tlx-nc4-20130520-2354` | `nexrad-level3` | committed `files/level3/l3-tlx-nc4-20130520-2354` | 8,812 |  |
| `l3-tlx-nc5-20130520-2354` | `nexrad-level3` | committed `files/level3/l3-tlx-nc5-20130520-2354` | 8,822 |  |
| `l3-tlx-nco-20130520-1816` | `nexrad-level3` | committed `files/level3/l3-tlx-nco-20130520-1816` | 5,476 |  |
| `l3-tlx-ncr-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-ncr-20130520-2016` | 32,400 |  |
| `l3-tlx-ncr-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-tlx-ncr-20260622-080623` | 45,300 |  |
| `l3-tlx-ncz-19990503-2316` | `nexrad-level3` | committed `files/level3/l3-tlx-ncz-19990503-2316` | 7,072 |  |
| `l3-tlx-ncz-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-ncz-20130520-2016` | 9,780 |  |
| `l3-tlx-ncz-20220503-005231` | `nexrad-level3` | committed `files/level3/l3-tlx-ncz-20220503-005231` | 10,114 |  |
| `l3-tlx-net-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-net-20130520-2016` | 2,340 |  |
| `l3-tlx-net-20220503-005231` | `nexrad-level3` | committed `files/level3/l3-tlx-net-20220503-005231` | 2,678 |  |
| `l3-tlx-nhi-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nhi-20130520-2016` | 8,294 |  |
| `l3-tlx-nhi-20220503-005231` | `nexrad-level3` | committed `files/level3/l3-tlx-nhi-20220503-005231` | 8,304 |  |
| `l3-tlx-nhl-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nhl-20130520-2016` | 2,296 |  |
| `l3-tlx-nhl-20220503-005231` | `nexrad-level3` | committed `files/level3/l3-tlx-nhl-20220503-005231` | 2,540 |  |
| `l3-tlx-nla-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nla-20130520-2016` | 2,378 |  |
| `l3-tlx-nla-20220503-005231` | `nexrad-level3` | committed `files/level3/l3-tlx-nla-20220503-005231` | 2,542 |  |
| `l3-tlx-nll-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nll-20130520-2016` | 2,398 |  |
| `l3-tlx-nmd-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nmd-20130520-2016` | 2,764 |  |
| `l3-tlx-nmd-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-tlx-nmd-20260622-080623` | 6,038 |  |
| `l3-tlx-nml-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nml-20130520-2016` | 2,254 |  |
| `l3-tlx-nml-20220503-005231` | `nexrad-level3` | committed `files/level3/l3-tlx-nml-20220503-005231` | 2,318 |  |
| `l3-tlx-nrr-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-tlx-nrr-20260622-080623` | 10,301 |  |
| `l3-tlx-nsp-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nsp-20130520-2016` | 31,998 |  |
| `l3-tlx-nss-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nss-20130520-2016` | 9,968 |  |
| `l3-tlx-nss-20220503-005231` | `nexrad-level3` | committed `files/level3/l3-tlx-nss-20220503-005231` | 9,712 |  |
| `l3-tlx-nst-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nst-20130520-2016` | 10,552 |  |
| `l3-tlx-nst-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-tlx-nst-20260622-080623` | 16,004 |  |
| `l3-tlx-nsw-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nsw-20130520-2016` | 18,902 |  |
| `l3-tlx-nsw-20220503-005231` | `nexrad-level3` | committed `files/level3/l3-tlx-nsw-20220503-005231` | 20,676 |  |
| `l3-tlx-ntp-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-ntp-20130520-2016` | 11,060 |  |
| `l3-tlx-ntp-20260629-173638` | `nexrad-level3` | committed `files/level3/l3-tlx-ntp-20260629-173638` | 8,560 |  |
| `l3-tlx-ntv-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-ntv-20130520-2016` | 3,258 |  |
| `l3-tlx-ntv-20220503-005231` | `nexrad-level3` | committed `files/level3/l3-tlx-ntv-20220503-005231` | 4,136 |  |
| `l3-tlx-nvl-20130520-2012` | `nexrad-level3` | committed `files/level3/l3-tlx-nvl-20130520-2012` | 1,816 |  |
| `l3-tlx-nvl-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-tlx-nvl-20260622-080623` | 2,714 |  |
| `l3-tlx-nvw-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-nvw-20130520-2016` | 11,932 |  |
| `l3-tlx-nvw-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-tlx-nvw-20260622-080623` | 11,910 |  |
| `l3-tlx-oha-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-oha-20130520-2016` | 8,108 |  |
| `l3-tlx-oha-20260622-080623` | `nexrad-level3` | committed `files/level3/l3-tlx-oha-20260622-080623` | 11,266 |  |
| `l3-tlx-pta-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-pta-20130520-2016` | 10,886 |  |
| `l3-tlx-pta-20200501-000023` | `nexrad-level3` | committed `files/level3/l3-tlx-pta-20200501-000023` | 1,474 |  |
| `l3-tlx-rcm-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-rcm-20130520-2016` | 2,180 |  |
| `l3-tlx-rcm-20220503-004553` | `nexrad-level3` | committed `files/level3/l3-tlx-rcm-20220503-004553` | 2,250 |  |
| `l3-tlx-rsl-20130520-2358` | `nexrad-level3` | committed `files/level3/l3-tlx-rsl-20130520-2358` | 11,319 |  |
| `l3-tlx-rsl-20220502-235926` | `nexrad-level3` | committed `files/level3/l3-tlx-rsl-20220502-235926` | 4,024 |  |
| `l3-tlx-spd-20130520-2016` | `nexrad-level3` | committed `files/level3/l3-tlx-spd-20130520-2016` | 2,864 |  |
| `l3-tlx-spd-20220503-005231` | `nexrad-level3` | committed `files/level3/l3-tlx-spd-20220503-005231` | 2,864 |  |

#### `testdata/other/manifest.toml`

| id | format | where | bytes | derived from |
|---|---|---|---:|---|
| `odim-bejab-20190606-0000-pvol` | `odim-h5` | committed `files/other/odim/bejab.pvol.hdf` | 640,209 |  |
| `odim-bewid-20130429-0430-pvol-dbzh-scan1` | `odim-h5` | committed `files/other/odim/20130429043000.rad.bewid.pvol.dbzh.scan1.hdf` | 348,893 |  |
| `odim-norst-20170421-0908-pvol` | `odim-h5` | committed `files/other/odim/T_PAGZ35_C_ENMI_20170421090837.hdf` | 422,385 |  |
| `odim-espdg-20260707-1927-pvol-dbzh-vradh` | `odim-h5` | committed `files/other/odim/espdg.pvol.20260707.dbzh_vradh.h5` | 162,450 |  |
| `odim-imgw-ram-20260711-0015-kdp-max` | `odim-h5` | committed `files/other/odim/imgw_polrad/2026071100150601KDP.max.h5` | 33,775 |  |
| `odim-imgw-ram-20260711-0015-phidp-max` | `odim-h5` | committed `files/other/odim/imgw_polrad/2026071100150601PhiDP.max.h5` | 32,534 |  |
| `odim-imgw-ram-20260711-0015-rhohv-max` | `odim-h5` | committed `files/other/odim/imgw_polrad/2026071100150601RhoHV.max.h5` | 61,984 |  |
| `odim-imgw-ram-20260711-0015-zdr-max` | `odim-h5` | committed `files/other/odim/imgw_polrad/2026071100150601ZDR.max.h5` | 59,793 |  |
| `odim-iesha-20260305-0115-pvol` | `odim-h5` | committed `files/other/odim/iesha.pvol.20260305T0115.dbzh_th_vradh.h5` | 1,667,065 |  |
| `odim-dkrom-20260820-1130-pvol` | `odim-h5` | committed `files/other/odim/dkrom.pvol.20260820T1130.dualpol.h5` | 1,695,131 |  |
| `odim-dkrom-20260820-1130-pvol-h5latest-trim` | `odim-h5` | committed `files/other/odim/h5latest/dkrom.pvol.20260820T1130.h5latest-trim.h5` | 532,077 | `odim-dkrom-20260820-1130-pvol` |
| `odim-dkrom-20260820-1130-pvol-h5edge-paged-ea` | `odim-h5` | committed `files/other/odim/h5edge/dkrom.pvol.20260820T1130.h5edge-paged-ea.h5` | 939,586 | `odim-dkrom-20260820-1130-pvol` |
| `odim-dkrom-20260820-1130-pvol-h5edge-len4` | `odim-h5` | committed `files/other/odim/h5edge/dkrom.pvol.20260820T1130.h5edge-len4.h5` | 178,575 | `odim-dkrom-20260820-1130-pvol` |
| `odim-au24-20260610-000300-nci-zip-member` | `zip-local-member` | committed `files/other/odim/nci/24_20260610_000300.pvol.h5.nci-response` | 354,749 |  |
| `odim-seang-20260924-2130-qcvol-dataset1-trim` | `odim-h5` | committed `files/other/odim/smhi/seang.qcvol.20260924T2130.dataset1-trim.h5` | 156,364 |  |
| `odim-fianj-20260924-2130-pvol-dataset1-trim` | `odim-h5` | committed `files/other/odim/fmi/fianj.pvol.20260924T2130.dataset1-trim.h5` | 454,704 |  |
| `odim-deboo-20260924-2130-sweep-th-00` | `odim-h5` | committed `files/other/odim/dwd/ras07-vol5minng01_sweeph5onem_th_00-2026092421305800-boo-10132-hd5` | 166,090 |  |
| `odim-itdes-20260924-2135-pvol-class` | `odim-h5` | committed `files/other/odim/lombardia/Desio.20260924T213500Z_CLASS.h5` | 322,411 |  |
| `cfrad1-xsapr-sgp-20110520-ppi-netcdf4` | `cfradial1` | committed `files/other/cfradial/cfrad.xsapr_sgp_ppi_20110520.netcdf4.nc` | 75,587 |  |
| `cfrad1-xsapr-sgp-20110520-ppi-netcdf4-user-types` | `cfradial1` | committed `files/other/cfradial/cfrad.xsapr_sgp_ppi_20110520.netcdf4.user-types.nc` | 85,569 | `cfrad1-xsapr-sgp-20110520-ppi-netcdf4` |
| `cfrad1-xsapr-sgp-20110520-ppi-netcdf4-szip-lzf` | `cfradial1` | committed `files/other/cfradial/filters/cfrad.xsapr_sgp_ppi_20110520.netcdf4-szip-lzf.nc` | 98,074 | `cfrad1-xsapr-sgp-20110520-ppi-netcdf4` |
| `cfrad1-xsapr-sgp-20110520-ppi-classic` | `cfradial1` | committed `files/other/cfradial/cfrad.xsapr_sgp_ppi_20110520.classic.nc` | 13,624 | `cfrad1-xsapr-sgp-20110520-ppi-netcdf4` |
| `cfrad1-dow8-20211011-223602-rhi` | `cfradial1` | download | 1,682,730 |  |
| `cfrad1-dow8-20211011-223602-rhi-trim3-classic` | `cfradial1` | committed `files/other/cfradial/cfrad.20211011_223602_DOW8_RHI.trim3.nc` | 888,428 | `cfrad1-dow8-20211011-223602-rhi` |
| `cfrad1-irene-sr2-20110827-120420-sur-sweeps01` | `cfradial1` | committed `files/other/cfradial/cfrad.20110827_120420.760_CPOLRVP_IRENE_WINDS_SUR.sweeps0-1_DBZ_VEL.nc` | 1,664,412 |  |
| `cfrad1-spol-20080604-002217-sur` | `cfradial1` | download | 15,418,562 |  |
| `cfrad2-spol-20080604-002217-sur` | `cfradial2` | download | 15,487,749 |  |
| `cfrad2-radx-irene-sr2-20110827-120420-sur-r30km` | `cfradial2` | committed `files/other/cfradial/cfrad2.radx.20110827_120420_IRENE_SUR.r30km.nc` | 565,495 | `cfrad1-irene-sr2-20110827-120420-sur-sweeps01` |
| `cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32` | `cfradial2` | committed `files/other/cfradial/cfrad2.radx.iesha.20260305_0115.sweeps7-10.int32.nc` | 612,876 | `odim-iesha-20260305-0115-pvol` |
| `cfrad2-xradar-xsapr-sgp-20110520-ppi` | `cfradial2` | committed `files/other/cfradial/cfrad2.xradar.xsapr_sgp_ppi_20110520.nc` | 34,396 | `cfrad1-xsapr-sgp-20110520-ppi-classic` |
| `cfrad2-xradar-dow8-20211011-223602-rhi-r300` | `cfradial2` | committed `files/other/cfradial/cfrad2.xradar.20211011_223602_DOW8_RHI.r300.nc` | 315,407 | `cfrad1-dow8-20211011-223602-rhi-trim3-classic` |
| `dorade-cow2-20260521-225514-sur-head24` | `dorade` | committed `files/other/dorade/swp.1260521225514.COW2.229.1.0_SUR_v215.head24` | 37,380 |  |
| `dorade-noxp-20090501-sweeps-tgz` | `tar-gz` | download | 324,596 |  |
| `dorade-noxp-20090525-sweeps-tgz` | `tar-gz` | download | 5,813,887 |  |
| `dorade-noxp-20090501-190244-ppi` | `dorade` | committed `files/other/dorade/swp.1090501190244.NOXPRVP.0.0.5_PPI_v1` | 939,268 | `dorade-noxp-20090501-sweeps-tgz` |
| `dorade-noxp-20090501-190324-ppi` | `dorade` | committed `files/other/dorade/swp.1090501190324.NOXPRVP.0.0.5_PPI_v1` | 939,268 | `dorade-noxp-20090501-sweeps-tgz` |
| `dorade-noxp-20090525-203211-sector` | `dorade` | committed `files/other/dorade/swp.1090525203211.NOXPRVP.0.0.5_PPI_v1` | 1,634,536 | `dorade-noxp-20090525-sweeps-tgz` |
| `dorade-dow6-20211230-222139-rhi-head41` | `dorade` | committed `files/other/dorade/swp.1211230222139.DOW6low.648.144.0_RHI_v169.head41` | 1,471,504 |  |
| `dorade-noxp-20090610-003210-ppi-head6` | `dorade` | committed `files/other/dorade/swp.1090610003210.NOXPRVP.0.0.5_PPI_v1.head6` | 131,596 |  |
| `dorade-noxp-20090610-003222-ppi-head6` | `dorade` | committed `files/other/dorade/swp.1090610003222.NOXPRVP.0.1.0_PPI_v1.head6` | 131,596 |  |
| `dorade-noxp-20090610-003226-ppi-head6` | `dorade` | committed `files/other/dorade/swp.1090610003226.NOXPRVP.0.2.0_PPI_v1.head6` | 131,596 |  |
| `dorade-noxp-20090610-003210-heads-zip` | `zip` | committed `files/other/dorade/NOX090610003210.RAWAL8D.head6.zip` | 44,980 |  |
| `dorade-n42rf-ts-20181010-122951-air` | `dorade` | download | 2,919,700 |  |
| `dorade-n42rf-ts-20181010-122951-air-head24` | `dorade` | committed `files/other/dorade/swp.1181010122951.N42RF-TS.196.-20.0_AIR_v3394.head24` | 432,440 | `dorade-n42rf-ts-20181010-122951-air` |
| `dorade-n42rf-tm-20181010-123925-air` | `dorade` | download | 3,100,560 |  |
| `dorade-n42rf-tm-20181010-123925-air-head48` | `dorade` | committed `files/other/dorade/swp.1181010123925.N42RF-TM.137.20.0_AIR_v3532.head48` | 222,892 | `dorade-n42rf-tm-20181010-123925-air` |
| `jma-n5-20191012-090000` | `jma-grib2-tar` | download | 39,106,560 |  |
| `jma-n6-20191012-090000` | `jma-grib2-tar` | download | 13,209,600 |  |
| `jma-n5-20191012-090000-rs47773` | `jma-grib2-tar` | committed `files/other/jma/Z__C_RJTD_20191012090000_RDR_JMAGPV_N5_grib2.RS47773.tar` | 1,761,280 | `jma-n5-20191012-090000` |
| `jma-n6-20191012-090000-rs47773` | `jma-grib2-tar` | committed `files/other/jma/Z__C_RJTD_20191012090000_RDR_JMAGPV_N6_grib2.RS47773.tar` | 624,640 | `jma-n6-20191012-090000` |
| `jma-n5-20260924-210000` | `jma-grib2-tar` | download | 4,014,080 |  |
| `jma-n6-20260924-210000` | `jma-grib2-tar` | download | 2,498,560 |  |
| `jma-n5-20260924-210000-rs47937` | `jma-grib2-tar` | committed `files/other/jma/Z__C_RJTD_20260924210000_RDR_JMAGPV_N5_grib2.RS47937.tar` | 153,600 | `jma-n5-20260924-210000` |
| `jma-n6-20260924-210000-rs47937` | `jma-grib2-tar` | committed `files/other/jma/Z__C_RJTD_20260924210000_RDR_JMAGPV_N6_grib2.RS47937.tar` | 81,920 | `jma-n6-20260924-210000` |
| `odim-bejab-20260612-1450-dbzh` | `odim-h5` | committed `files/other/odim/ord-parts/bejab@20260612T1450@0.5_1.2_2.1_3.4_4.8_6.5_9.0_13.0_25.0@DBZH.h5` | 243,054 |  |
| `odim-bejab-20260612-1450-vrad` | `odim-h5` | committed `files/other/odim/ord-parts/bejab@20260612T1450@0.5_1.2_2.1_3.4_4.8_6.5_9.0_13.0_25.0@VRAD.h5` | 219,191 |  |
| `odim-nohur-20260612-1445-dbzh` | `odim-h5` | committed `files/other/odim/ord-parts/nohur@20260612T1445@0.5_1.0_2.6_5.2_8.6_13.0_18.6_25.8_35.0_90.0@DBZH.h5` | 773,343 |  |
| `odim-nohur-20260612-1445-th` | `odim-h5` | committed `files/other/odim/ord-parts/nohur@20260612T1445@0.5_1.0_2.6_5.2_8.6_13.0_18.6_25.8_35.0_90.0@TH.h5` | 1,478,472 |  |
| `odim-nohur-20260612-1446-vradh` | `odim-h5` | committed `files/other/odim/ord-parts/nohur@20260612T1446@2.6_5.2_8.6_13.0_18.6_25.8_35.0_90.0@VRADH.h5` | 197,553 |  |
| `odim-au02-20260921-0000-pvol-subset` | `odim-h5` | committed `files/other/odim/nci/2_20260921_000000.pvol.subset.h5` | 141,384 |  |
| `l3-kbmx-19980416-archive-tarz` | `tar-z` | download | 32,132,224 |  |
| `l3-kbmx-19980416-0006-nvw` | `nexrad-level3` | committed `files/other/nexrad-level3/KBMX_SDUS54_NVWBMX_199804160006` | 7,090 | `l3-kbmx-19980416-archive-tarz` |
| `l3-kdvn-20200810-1757-nst` | `nexrad-level3` | committed `files/other/nexrad-level3/KDVN_SDUS33_NSTDVN_202008101757` | 14,838 |  |
| `l3-kdvn-20200810-1804-nst` | `nexrad-level3` | committed `files/other/nexrad-level3/KDVN_SDUS33_NSTDVN_202008101804` | 14,928 |  |
| `l3-kdvn-20200810-1810-nst` | `nexrad-level3` | committed `files/other/nexrad-level3/KDVN_SDUS33_NSTDVN_202008101810` | 13,758 |  |
| `l3-kdvn-20200810-1817-nst` | `nexrad-level3` | committed `files/other/nexrad-level3/KDVN_SDUS33_NSTDVN_202008101817` | 13,158 |  |
| `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-per-ray-geometry` | `cfradial1` | committed `files/other/cfradial/radx/cfrad1.radx.fianj.20260924_2130.sweeps1-2-7.per-ray-geometry.nc` | 1,255,576 |  |
| `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-finest-geometry-netcdf4` | `cfradial1` | committed `files/other/cfradial/radx/cfrad1.radx.fianj.20260924_2130.sweeps1-2-7.finest-geometry.netcdf4.nc` | 987,194 |  |
| `l3-ktlx-20260622-080806-n0b-sails` | `nexrad-level3` | committed `files/other/nexrad-level3/TLX_N0B_2026_06_22_08_08_06` | 322,812 |  |
| `l3-knqa-20080205-archive-tarz` | `tar-z` | download | 52,026,567 |  |
| `l3-knqa-20080205-0018-rob` | `nexrad-level3` | committed `files/other/nexrad-level3/KWBC_SDUS44_ROBNQA_200802050018` | 142 | `l3-knqa-20080205-archive-tarz` |
| `wmo-text-kslc-20251012-0424-hmlslc` | `wmo-text` | committed `files/other/wmo-text/SRUS55_KSLC_HMLSLC_202510120424` | 19,398 |  |
| `http-request-head-curl-8.21.0` | `http-request-head` | committed `files/other/http/request-head-curl-8.21.0` | 92 |  |
| `http-request-head-python-urllib-3.13` | `http-request-head` | committed `files/other/http/request-head-python-urllib-3.13` | 132 |  |
| `http-request-head-recast-radar-fetch` | `http-request-head` | committed `files/other/http/request-head-recast-radar-fetch` | 129 |  |

#### `testdata/scattering/manifest.toml`

| id | format | where | bytes | derived from |
|---|---|---|---:|---|
| `tmatrix-lut-rain-sband-pytmatrix-0.3.3` | `brslut-v1` | committed `files/scattering/pytmatrix-0.3.3/conventional_liquid_rain_sband_unvalidated/table.lut` | 9,108 |  |
| `tmatrix-lut-rain-sband-pytmatrix-0.3.3-config` | `json` | committed `files/scattering/pytmatrix-0.3.3/conventional_liquid_rain_sband_unvalidated/config.json` | 3,151 |  |
| `tmatrix-lut-rain-sband-pytmatrix-0.3.3-manifest` | `json` | committed `files/scattering/pytmatrix-0.3.3/conventional_liquid_rain_sband_unvalidated/manifest.json` | 4,595 |  |
| `tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3` | `brslut-v1` | committed `files/scattering/pytmatrix-0.3.3/conventional_dry_ice_spheroids_sband_unvalidated/table.lut` | 12,947 |  |
| `tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3-config` | `json` | committed `files/scattering/pytmatrix-0.3.3/conventional_dry_ice_spheroids_sband_unvalidated/config.json` | 3,882 |  |
| `tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3-manifest` | `json` | committed `files/scattering/pytmatrix-0.3.3/conventional_dry_ice_spheroids_sband_unvalidated/manifest.json` | 5,000 |  |
| `tmatrix-held-out-interpolation-report-v10` | `json` | committed `files/scattering/pytmatrix-0.3.3/refined_grid_v10_post_freeze_held_out_interpolation_report.json` | 186,036 |  |
| `tmatrix-held-out-nodes-v10` | `json` | committed `files/scattering/pytmatrix-0.3.3/refined_grid_v10_post_freeze_held_out_nodes.json` | 17,067 |  |
| `wrf-p3-lookup-table-1-v5.4-2momI` | `wrf-p3-lookup-table` | download | 1,606,038 |  |
| `wrf-p3-lookup-table-1-v5.4-3momI` | `wrf-p3-lookup-table` | download | 17,886,038 |  |
| `wrf-p3-lookup-table-1-v5.4-2momI-first-block` | `wrf-p3-lookup-table` | committed `files/scattering/wrf-p3/p3_lookupTable_1.dat-v5.4_2momI.first-block` | 80,338 | `wrf-p3-lookup-table-1-v5.4-2momI` |
| `wrf-p3-lookup-table-1-v5.4-3momI-first-block` | `wrf-p3-lookup-table` | committed `files/scattering/wrf-p3/p3_lookupTable_1.dat-v5.4_3momI.first-block` | 81,338 | `wrf-p3-lookup-table-1-v5.4-3momI` |
| `tmatrix-lut-property-dry-oblate-sband-trim` | `brslut-v1` | committed `files/scattering/pytmatrix-0.3.3/property_p3_ishmael_dry_oblate_sband_trim/table.lut` | 147,472 |  |
| `tmatrix-lut-property-dry-oblate-sband-trim-config` | `json` | committed `files/scattering/pytmatrix-0.3.3/property_p3_ishmael_dry_oblate_sband_trim/config.json` | 6,376 |  |
| `tmatrix-lut-property-dry-oblate-sband-trim-trim` | `json` | committed `files/scattering/pytmatrix-0.3.3/property_p3_ishmael_dry_oblate_sband_trim/trim.json` | 1,020 |  |
| `tmatrix-lut-property-wet-oblate-sband-trim` | `brslut-v1` | committed `files/scattering/pytmatrix-0.3.3/property_p3_ishmael_wet_oblate_sband_trim/table.lut` | 165,506 |  |
| `tmatrix-lut-property-wet-oblate-sband-trim-config` | `json` | committed `files/scattering/pytmatrix-0.3.3/property_p3_ishmael_wet_oblate_sband_trim/config.json` | 7,018 |  |
| `tmatrix-lut-property-wet-oblate-sband-trim-trim` | `json` | committed `files/scattering/pytmatrix-0.3.3/property_p3_ishmael_wet_oblate_sband_trim/trim.json` | 1,044 |  |
| `tmatrix-lut-property-rain-sband-trim` | `brslut-v1` | committed `files/scattering/pytmatrix-0.3.3/property_rain_sband_trim/table.lut` | 25,651 |  |
| `tmatrix-lut-property-rain-sband-trim-config` | `json` | committed `files/scattering/pytmatrix-0.3.3/property_rain_sband_trim/config.json` | 4,654 |  |
| `tmatrix-lut-property-rain-sband-trim-trim` | `json` | committed `files/scattering/pytmatrix-0.3.3/property_rain_sband_trim/trim.json` | 892 |  |

### Index by tag

Tags are grouped by the part before `:`. Ids are in manifest order. `prefix{a..b}suffix` stands for every id with a number from `a` to `b` (same digit count) in that place.

#### Tags without a namespace

- `archive` (5): `dorade-noxp-20090501-sweeps-tgz`, `dorade-noxp-20090525-sweeps-tgz`, `dorade-noxp-20090610-003210-heads-zip`, `l3-kbmx-19980416-archive-tarz`, `l3-knqa-20080205-archive-tarz`
- `avset` (4): `l2-kdgx-20230325-010651`, `l2-ktlx-20240315-000217`, `l2-kiwa-20260917-003629`, `l2-ktlx-20240315-000217-trim`
- `bench` (4): `l2-ktlx-19990504-002218`, `l2-ktlx-20130520-201643`, `l2-ktlx-20240315-000217`, `l2-kilx-20260418-013553`
- `derived` (60): `fuzz-odim-hdf5-local-heap-name-offset-overflow`, `fuzz-hdf5-chunk-offset-overflow`, `fuzz-cfradial-overlapping-sweep-ray-ranges`, `fuzz-level2-writer-nexrad-moment-nan-scale`, `fuzz-writers-l2-sweep-without-gates`, `fuzz-writers-l2-one-gate-sweep`, `fuzz-writers-l2-odim-rstart-beyond-20-km`, `fuzz-writers-dorade-ray-without-time`, `fuzz-writers-l2-empty-field-name`, `fuzz-writers-odim-gate-spacing-below-float`, `fuzz-writers-dorade-absent-rows-without-fill`, `fuzz-writers-cfradial1-ray-time-near-float-max`, `fuzz-level3-rcm-centroid-non-ascii`, `fuzz-level2-records-volume-header-date-overflow`, `odim-dkrom-20260820-1130-pvol-h5latest-trim`, `odim-dkrom-20260820-1130-pvol-h5edge-paged-ea`, `odim-dkrom-20260820-1130-pvol-h5edge-len4`, `odim-seang-20260924-2130-qcvol-dataset1-trim`, `odim-fianj-20260924-2130-pvol-dataset1-trim`, `odim-itdes-20260924-2135-pvol-class`, `cfrad1-xsapr-sgp-20110520-ppi-netcdf4-user-types`, `cfrad1-xsapr-sgp-20110520-ppi-netcdf4-szip-lzf`, `cfrad1-xsapr-sgp-20110520-ppi-classic`, `cfrad1-dow8-20211011-223602-rhi-trim3-classic`, `cfrad1-irene-sr2-20110827-120420-sur-sweeps01`, `cfrad2-radx-irene-sr2-20110827-120420-sur-r30km`, `cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32`, `cfrad2-xradar-xsapr-sgp-20110520-ppi`, `cfrad2-xradar-dow8-20211011-223602-rhi-r300`, `dorade-cow2-20260521-225514-sur-head24`, `dorade-noxp-20090501-190244-ppi`, `dorade-noxp-20090501-190324-ppi`, `dorade-noxp-20090525-203211-sector`, `dorade-dow6-20211230-222139-rhi-head41`, `dorade-noxp-20090610-003210-ppi-head6`, `dorade-noxp-20090610-003222-ppi-head6`, `dorade-noxp-20090610-003226-ppi-head6`, `dorade-noxp-20090610-003210-heads-zip`, `dorade-n42rf-ts-20181010-122951-air-head24`, `dorade-n42rf-tm-20181010-123925-air-head48`, `jma-n5-20191012-090000-rs47773`, `jma-n6-20191012-090000-rs47773`, `jma-n5-20260924-210000-rs47937`, `jma-n6-20260924-210000-rs47937`, `odim-au02-20260921-0000-pvol-subset`, `l3-kbmx-19980416-0006-nvw`, `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-per-ray-geometry`, `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-finest-geometry-netcdf4`, `l3-knqa-20080205-0018-rob`, `wrf-p3-lookup-table-1-v5.4-2momI-first-block`, `wrf-p3-lookup-table-1-v5.4-3momI-first-block`, `tmatrix-lut-property-dry-oblate-sband-trim`, `tmatrix-lut-property-dry-oblate-sband-trim-config`, `tmatrix-lut-property-dry-oblate-sband-trim-trim`, `tmatrix-lut-property-wet-oblate-sband-trim`, `tmatrix-lut-property-wet-oblate-sband-trim-config`, `tmatrix-lut-property-wet-oblate-sband-trim-trim`, `tmatrix-lut-property-rain-sband-trim`, `tmatrix-lut-property-rain-sband-trim-config`, `tmatrix-lut-property-rain-sband-trim-trim`
- `dualpol` (33): `l2-kvnx-20110315-000203`, `l2-ktlx-20130520-201643`, `l2-kgwx-20130601-235640`, `l2-koax-20140616-205305`, `l2-kewx-20160413-022531`, `l2-kdvn-20200810-175718`, `l2-kdvn-20200810-180401`, `l2-kdvn-20200810-181043`, `l2-kdvn-20200810-181724`, `l2-klix-20210829-180425`, `l2-klix-20210829-173117`, `l2-klix-20210829-175748`, `l2-kbox-20220129-150537`, `l2-tjua-20220918-190621`, `l2-kdgx-20230325-010651`, `l2-kmaf-20230331-230843`, `l2-pgua-20230524-030945`, `l2-kmtx-20240301-212827`, `l2-ktlx-20240315-000217`, `l2-ktlx-20240515-000014`, `l2-pahg-20250909-212549`, `l2-kilx-20260418-013553`, `l2-kiwa-20260917-003629`, `l2-ktlx-20130520-201643-trim`, `l2-koax-20140616-205305-trim`, `l2-kewx-20160413-022531-trim`, `l2-kdvn-20200810-180401-trim`, `l2-klix-20210829-180425-trim`, `l2-kbox-20220129-150537-trim`, `l2-pgua-20230524-030945-trim`, `l2-kmtx-20240301-212827-trim`, `l2-ktlx-20240315-000217-trim`, `l2-kilx-20260418-013553-trim`
- `fuzz-regression` (14): `fuzz-odim-hdf5-local-heap-name-offset-overflow`, `fuzz-hdf5-chunk-offset-overflow`, `fuzz-cfradial-overlapping-sweep-ray-ranges`, `fuzz-level2-writer-nexrad-moment-nan-scale`, `fuzz-writers-l2-sweep-without-gates`, `fuzz-writers-l2-one-gate-sweep`, `fuzz-writers-l2-odim-rstart-beyond-20-km`, `fuzz-writers-dorade-ray-without-time`, `fuzz-writers-l2-empty-field-name`, `fuzz-writers-odim-gate-spacing-below-float`, `fuzz-writers-dorade-absent-rows-without-fill`, `fuzz-writers-cfradial1-ray-time-near-float-max`, `fuzz-level3-rcm-centroid-non-ascii`, `fuzz-level2-records-volume-header-date-overflow`
- `golden-source` (1): `tmatrix-held-out-interpolation-report-v10`
- `http-request` (3): `http-request-head-curl-8.21.0`, `http-request-head-python-urllib-3.13`, `http-request-head-recast-radar-fetch`
- `long-pulse` (1): `l2-kmaf-20230331-230843`
- `mpda` (4): `l2-klix-20210829-180425`, `l2-klix-20210829-173117`, `l2-klix-20210829-175748`, `l2-klix-20210829-180425-trim`
- `no-metadata-record` (2): `l2-ktlx-19910605-162126`, `l2-ktlx-19910605-162126-trim`
- `no-msg5` (1): `l2-kvwx-20080415-235337`
- `part-of-scan` (5): `odim-bejab-20260612-1450-dbzh`, `odim-bejab-20260612-1450-vrad`, `odim-nohur-20260612-1445-dbzh`, `odim-nohur-20260612-1445-th`, `odim-nohur-20260612-1446-vradh`
- `partial-sweeps` (10): `l2-ktlx-20130520-201643-trim`, `l2-koax-20140616-205305-trim`, `l2-kewx-20160413-022531-trim`, `l2-kdvn-20200810-180401-trim`, `l2-klix-20210829-180425-trim`, `l2-kbox-20220129-150537-trim`, `l2-pgua-20230524-030945-trim`, `l2-kmtx-20240301-212827-trim`, `l2-ktlx-20240315-000217-trim`, `l2-kilx-20260418-013553-trim`
- `polling-server` (6): `polling-ndswc-kxwa-dir-list-20260925`, `polling-iem-config-cfg-20260926`, `polling-ewr-laredo-grlevel2-cfg-20260925`, `http-request-head-curl-8.21.0`, `http-request-head-python-urllib-3.13`, `http-request-head-recast-radar-fetch`
- `sails` (10): `l2-koax-20140616-205305`, `l2-kewx-20160413-022531`, `l2-klix-20210829-180425`, `l2-klix-20210829-173117`, `l2-klix-20210829-175748`, `l2-tjua-20220918-190621`, `l2-kiwa-20260917-003629`, `l2-koax-20140616-205305-trim`, `l2-kewx-20160413-022531-trim`, `l2-klix-20210829-180425-trim`
- `split-cut` (16): `l2-ktlx-19910605-162126-trim`, `l2-ktlx-19990504-002218-trim`, `l2-ktlx-20030508-221041-trim`, `l2-klix-20050829-130035-trim`, `l2-kdmx-20080525-205148-trim`, `l2-ktlx-20130520-201643-trim`, `l2-koax-20140616-205305-trim`, `l2-kewx-20160413-022531-trim`, `l2-kdvn-20200810-180401-trim`, `l2-klix-20210829-180425-trim`, `l2-kbox-20220129-150537-trim`, `l2-tstl-20230331-230314-trim`, `l2-pgua-20230524-030945-trim`, `l2-kmtx-20240301-212827-trim`, `l2-ktlx-20240315-000217-trim`, `l2-kilx-20260418-013553-trim`
- `subset` (2): `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-per-ray-geometry`, `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-finest-geometry-netcdf4`
- `text` (4): `wrf-p3-lookup-table-1-v5.4-2momI`, `wrf-p3-lookup-table-1-v5.4-3momI`, `wrf-p3-lookup-table-1-v5.4-2momI-first-block`, `wrf-p3-lookup-table-1-v5.4-3momI-first-block`
- `trim` (16): `l2-ktlx-19910605-162126`, `l2-ktlx-19990504-002218`, `l2-ktlx-20030508-221041`, `l2-klix-20050829-130035`, `l2-kdmx-20080525-205148`, `l2-ktlx-20130520-201643`, `l2-koax-20140616-205305`, `l2-kewx-20160413-022531`, `l2-kdvn-20200810-180401`, `l2-klix-20210829-180425`, `l2-kbox-20220129-150537`, `l2-tstl-20230331-230314`, `l2-pgua-20230524-030945`, `l2-kmtx-20240301-212827`, `l2-ktlx-20240315-000217`, `l2-kilx-20260418-013553`
- `trimmed` (16): `l2-ktlx-19910605-162126-trim`, `l2-ktlx-19990504-002218-trim`, `l2-ktlx-20030508-221041-trim`, `l2-klix-20050829-130035-trim`, `l2-kdmx-20080525-205148-trim`, `l2-ktlx-20130520-201643-trim`, `l2-koax-20140616-205305-trim`, `l2-kewx-20160413-022531-trim`, `l2-kdvn-20200810-180401-trim`, `l2-klix-20210829-180425-trim`, `l2-kbox-20220129-150537-trim`, `l2-tstl-20230331-230314-trim`, `l2-pgua-20230524-030945-trim`, `l2-kmtx-20240301-212827-trim`, `l2-ktlx-20240315-000217-trim`, `l2-kilx-20260418-013553-trim`

#### `awips:`

- `awips:016IND` (1): `l3-ind-016-19940910-0647`
- `awips:017IND` (1): `l3-ind-017-19940910-1104`
- `awips:018AKQ` (1): `l3-akq-018-19940810-0835`
- `awips:021CYS` (1): `l3-cys-021-19941114-1107`
- `awips:022FTG` (1): `l3-ftg-022-19940601-1917`
- `awips:024JAN` (1): `l3-jan-024-19951003-1312`
- `awips:026FTG` (1): `l3-ftg-026-19940930-0536`
- `awips:029BMX` (1): `l3-bmx-029-19940914-1621`
- `awips:035IND` (1): `l3-ind-035-19941031-2138`
- `awips:039GRR` (1): `l3-grr-039-20011011-0631`
- `awips:042IND` (1): `l3-ind-042-19940910-1642`
- `awips:043FTG` (1): `l3-ftg-043-19940930-1849`
- `awips:044FTG` (1): `l3-ftg-044-19940930-1849`
- `awips:045FTG` (1): `l3-ftg-045-19940930-1849`
- `awips:046FTG` (1): `l3-ftg-046-19940930-1849`
- `awips:050LOT` (1): `l3-lot-050-19941031-1358`
- `awips:051MLB` (1): `l3-mlb-051-19941116-0335`
- `awips:053CAE` (1): `l3-cae-053-19940629-1906`
- `awips:053LOT` (1): `l3-lot-053-19941106-0246`
- `awips:055LOT` (1): `l3-lot-055-19941031-1137`
- `awips:063TLX` (1): `l3-tlx-063-19940308-1930`
- `awips:064TLX` (1): `l3-tlx-064-19940308-1930`
- `awips:073IND` (1): `l3-ind-073-19940910-1555`
- `awips:084LOT` (1): `l3-lot-084-19931120-0721`
- `awips:087TLX` (1): `l3-tlx-087-19940308-1939`
- `awips:100FTG` (1): `l3-ftg-100-19940930-0102`
- `awips:101LOT` (1): `l3-lot-101-19930824-0005`
- `awips:101TLX` (1): `l3-tlx-101-20010503-0007`
- `awips:102FTG` (1): `l3-ftg-102-19940930-1932`
- `awips:102TLX` (1): `l3-tlx-102-19990504-0052`
- `awips:103TLX` (1): `l3-tlx-103-20010503-0007`
- `awips:104FTG` (1): `l3-ftg-104-19940930-0850`
- `awips:104TLX` (1): `l3-tlx-104-20010503-2355`
- `awips:107FTG` (1): `l3-ftg-107-19940930-0154`
- `awips:108LOT` (1): `l3-lot-108-19930824-0005`
- `awips:109FTG` (1): `l3-ftg-109-19940930-0102`
- `awips:DAATLX` (2): `l3-tlx-daa-20130520-2016`, `l3-tlx-daa-20260622-080623`
- `awips:DHRMCI` (1): `l3-mci-dhr-20160526-2154`
- `awips:DHRTLX` (2): `l3-tlx-dhr-20130520-2016`, `l3-tlx-dhr-20260622-080623`
- `awips:DODTLX` (2): `l3-tlx-dod-20130520-2016`, `l3-tlx-dod-20220503-005231`
- `awips:DPAFWS` (1): `l3-fws-dpa-19950517-2304`
- `awips:DPAMCI` (1): `l3-mci-dpa-20160526-2154`
- `awips:DPATLX` (2): `l3-tlx-dpa-20130520-2016`, `l3-tlx-dpa-20260629-173638`
- `awips:DPRTLX` (2): `l3-tlx-dpr-20130520-2016`, `l3-tlx-dpr-20260622-080623`
- `awips:DSDTLX` (2): `l3-tlx-dsd-20130520-2016`, `l3-tlx-dsd-20220503-005231`
- `awips:DSPMCI` (1): `l3-mci-dsp-20160526-2154`
- `awips:DSPTLX` (2): `l3-tlx-dsp-20130520-2016`, `l3-tlx-dsp-20260629-173638`
- `awips:DTARAX` (1): `l3-rax-dta-20200818-0454`
- `awips:DTATLX` (2): `l3-tlx-dta-20130520-2016`, `l3-tlx-dta-20260622-080623`
- `awips:DU3TLX` (2): `l3-tlx-du3-20130520-2008`, `l3-tlx-du3-20260622-080623`
- `awips:DU6TLX` (1): `l3-tlx-du6-20260622-120608`
- `awips:DVLTLX` (2): `l3-tlx-dvl-20130520-2016`, `l3-tlx-dvl-20260622-080623`
- `awips:EETTLX` (2): `l3-tlx-eet-20130520-2016`, `l3-tlx-eet-20260622-080623`
- `awips:FTMABR` (1): `l3-abr-ftm-20110428-1331`
- `awips:GSMDDC` (1): `l3-ddc-gsm-20200817-1000`
- `awips:GSMEAX` (1): `l3-eax-gsm-20200817-0933`
- `awips:GSMTLX` (1): `l3-tlx-gsm-20130520-2100`
- `awips:H0CLZK` (1): `l3-lzk-h0c-20200814-0417`
- `awips:H0VLZK` (1): `l3-lzk-h0v-20200812-1309`
- `awips:H0WLZK` (1): `l3-lzk-h0w-20200812-1305`
- `awips:H0ZLZK` (1): `l3-lzk-h0z-20200812-1318`
- `awips:HHCTLX` (2): `l3-tlx-hhc-20130520-2016`, `l3-tlx-hhc-20260622-080623`
- `awips:HMLSLC` (1): `wmo-text-kslc-20251012-0424-hmlslc`
- `awips:IRMILX` (1): `l3-ilx-irm-19960419-2309`
- `awips:IRMLOT` (1): `l3-lot-irm-19941031-0503`
- `awips:IRMTLX` (1): `l3-tlx-irm-19940308-1115`
- `awips:N0BFTG` (1): `l3-ftg-n0b-20220304-1820`
- `awips:N0BTLX` (1): `l3-tlx-n0b-20260622-080623`
- `awips:N0CTLX` (2): `l3-tlx-n0c-20130520-2016`, `l3-tlx-n0c-20260622-080623`
- `awips:N0FGJX` (1): `l3-gjx-n0f-20200817-0551`
- `awips:N0FTLX` (1): `l3-tlx-n0f-20220502-235926`
- `awips:N0GTLX` (1): `l3-tlx-n0g-20260622-080623`
- `awips:N0HTLX` (2): `l3-tlx-n0h-20130520-2016`, `l3-tlx-n0h-20260622-080623`
- `awips:N0KTLX` (2): `l3-tlx-n0k-20130520-2016`, `l3-tlx-n0k-20260622-080623`
- `awips:N0MTLX` (2): `l3-tlx-n0m-20130520-2016`, `l3-tlx-n0m-20260622-080623`
- `awips:N0QBYX` (1): `l3-byx-n0q-20150124-2106`
- `awips:N0QDDC` (2): `l3-ddc-n0q-20200817-0501`, `l3-ddc-n0q-20200817-0503`
- `awips:N0QEAX` (2): `l3-eax-n0q-20200817-0401`, `l3-eax-n0q-20200817-0405`
- `awips:N0QFFC` (1): `l3-ffc-n0q-20140407-1805`
- `awips:N0QTLX` (2): `l3-tlx-n0q-20130520-2016`, `l3-tlx-n0q-20220503-005231`
- `awips:N0RCAE` (1): `l3-cae-n0r-19940629-1906`
- `awips:N0RFWS` (1): `l3-fws-n0r-19950517-2304`
- `awips:N0RLOT` (1): `l3-lot-n0r-19941106-0246`
- `awips:N0RTLX` (2): `l3-tlx-n0r-20130520-2016`, `l3-tlx-n0r-20220908-131957`
- `awips:N0STLX` (2): `l3-tlx-n0s-20130520-2016`, `l3-tlx-n0s-20260622-080623`
- `awips:N0UTLX` (2): `l3-tlx-n0u-20130520-2016`, `l3-tlx-n0u-20220503-005231`
- `awips:N0VTLX` (2): `l3-tlx-n0v-20130520-2016`, `l3-tlx-n0v-20220908-131957`
- `awips:N0XTLX` (2): `l3-tlx-n0x-20130520-2016`, `l3-tlx-n0x-20260622-080623`
- `awips:N0ZTLX` (2): `l3-tlx-n0z-20130520-2016`, `l3-tlx-n0z-20220908-131957`
- `awips:N1CTLX` (1): `l3-tlx-n1c-20130520-2016`
- `awips:N1HTLX` (1): `l3-tlx-n1h-20130520-2016`
- `awips:N1KTLX` (1): `l3-tlx-n1k-20130520-2016`
- `awips:N1MTLX` (1): `l3-tlx-n1m-20130520-2016`
- `awips:N1PFWS` (1): `l3-fws-n1p-19950517-2304`
- `awips:N1PMCI` (1): `l3-mci-n1p-20160526-2154`
- `awips:N1PTLX` (2): `l3-tlx-n1p-20130520-2016`, `l3-tlx-n1p-20260629-173638`
- `awips:N1QTLX` (1): `l3-tlx-n1q-20130520-2016`
- `awips:N1STLX` (1): `l3-tlx-n1s-20130520-2016`
- `awips:N1UTLX` (1): `l3-tlx-n1u-20130520-2016`
- `awips:N1XTLX` (1): `l3-tlx-n1x-20130520-2016`
- `awips:N2CTLX` (1): `l3-tlx-n2c-20130520-2016`
- `awips:N2HTLX` (1): `l3-tlx-n2h-20130520-2016`
- `awips:N2KTLX` (1): `l3-tlx-n2k-20130520-2016`
- `awips:N2MTLX` (1): `l3-tlx-n2m-20130520-2016`
- `awips:N2QTLX` (1): `l3-tlx-n2q-20130520-2016`
- `awips:N2STLX` (1): `l3-tlx-n2s-20130520-2016`
- `awips:N2UTLX` (1): `l3-tlx-n2u-20130520-2016`
- `awips:N2XTLX` (1): `l3-tlx-n2x-20130520-2016`
- `awips:N3CTLX` (1): `l3-tlx-n3c-20130520-2016`
- `awips:N3HTLX` (1): `l3-tlx-n3h-20130520-2016`
- `awips:N3KTLX` (1): `l3-tlx-n3k-20130520-2016`
- `awips:N3MTLX` (1): `l3-tlx-n3m-20130520-2016`
- `awips:N3PTLX` (2): `l3-tlx-n3p-20130520-2012`, `l3-tlx-n3p-20220503-011226`
- `awips:N3QTLX` (1): `l3-tlx-n3q-20130520-2016`
- `awips:N3STLX` (1): `l3-tlx-n3s-20130520-2016`
- `awips:N3UTLX` (1): `l3-tlx-n3u-20130520-2016`
- `awips:N3XTLX` (1): `l3-tlx-n3x-20130520-2016`
- `awips:NACTLX` (1): `l3-tlx-nac-20130520-2016`
- `awips:NAFGJX` (1): `l3-gjx-naf-20200817-0551`
- `awips:NAHTLX` (1): `l3-tlx-nah-20130520-2016`
- `awips:NAKTLX` (1): `l3-tlx-nak-20130520-2016`
- `awips:NAMTLX` (1): `l3-tlx-nam-20130520-2016`
- `awips:NAQTLX` (1): `l3-tlx-naq-20130520-2016`
- `awips:NAUTLX` (1): `l3-tlx-nau-20130520-2016`
- `awips:NAXTLX` (1): `l3-tlx-nax-20130520-2016`
- `awips:NBCTLX` (1): `l3-tlx-nbc-20130520-2016`
- `awips:NBFGJX` (1): `l3-gjx-nbf-20200817-0551`
- `awips:NBHTLX` (1): `l3-tlx-nbh-20130520-2016`
- `awips:NBKTLX` (1): `l3-tlx-nbk-20130520-2016`
- `awips:NBMTLX` (1): `l3-tlx-nbm-20130520-2016`
- `awips:NBQTLX` (1): `l3-tlx-nbq-20130520-2016`
- `awips:NBUTLX` (1): `l3-tlx-nbu-20130520-2016`
- `awips:NBXTLX` (1): `l3-tlx-nbx-20130520-2016`
- `awips:NC1AKC` (1): `l3-akc-nc1-20210730-055033`
- `awips:NC1TLX` (1): `l3-tlx-nc1-20130520-2354`
- `awips:NC2TLX` (1): `l3-tlx-nc2-20130520-2354`
- `awips:NC3TLX` (1): `l3-tlx-nc3-20130520-2354`
- `awips:NC4TLX` (1): `l3-tlx-nc4-20130520-2354`
- `awips:NC5TLX` (1): `l3-tlx-nc5-20130520-2354`
- `awips:NCOTLX` (1): `l3-tlx-nco-20130520-1816`
- `awips:NCRMCI` (1): `l3-mci-ncr-20160526-2154`
- `awips:NCROKC` (1): `l3-okc-ncr-20260622-080623`
- `awips:NCRTLX` (2): `l3-tlx-ncr-20130520-2016`, `l3-tlx-ncr-20260622-080623`
- `awips:NCZFWS` (1): `l3-fws-ncz-19950517-2304`
- `awips:NCZILX` (1): `l3-ilx-ncz-19960419-2320`
- `awips:NCZLZK` (1): `l3-lzk-ncz-19970301-1912`
- `awips:NCZTLX` (3): `l3-tlx-ncz-19990503-2316`, `l3-tlx-ncz-20130520-2016`, `l3-tlx-ncz-20220503-005231`
- `awips:NETMCI` (1): `l3-mci-net-20160526-2154`
- `awips:NETOKC` (1): `l3-okc-net-20220503-005210`
- `awips:NETTLX` (2): `l3-tlx-net-20130520-2016`, `l3-tlx-net-20220503-005231`
- `awips:NHIFWS` (1): `l3-fws-nhi-19950517-1323`
- `awips:NHIILX` (1): `l3-ilx-nhi-19960419-2303`
- `awips:NHIOKC` (1): `l3-okc-nhi-20220503-005210`
- `awips:NHITLX` (2): `l3-tlx-nhi-20130520-2016`, `l3-tlx-nhi-20220503-005231`
- `awips:NHLTLX` (2): `l3-tlx-nhl-20130520-2016`, `l3-tlx-nhl-20220503-005231`
- `awips:NLATLX` (2): `l3-tlx-nla-20130520-2016`, `l3-tlx-nla-20220503-005231`
- `awips:NLLRAX` (1): `l3-rax-nll-20220510-155126`
- `awips:NLLTLX` (1): `l3-tlx-nll-20130520-2016`
- `awips:NMDMCI` (1): `l3-mci-nmd-20160526-2154`
- `awips:NMDOKC` (1): `l3-okc-nmd-20260622-080640`
- `awips:NMDTLX` (2): `l3-tlx-nmd-20130520-2016`, `l3-tlx-nmd-20260622-080623`
- `awips:NMEFWS` (1): `l3-fws-nme-19950517-2316`
- `awips:NMEILX` (1): `l3-ilx-nme-19960419-2303`
- `awips:NMELZK` (1): `l3-lzk-nme-19970301-2027`
- `awips:NMESGF` (1): `l3-sgf-nme-20030504-2332`
- `awips:NMLTLX` (2): `l3-tlx-nml-20130520-2016`, `l3-tlx-nml-20220503-005231`
- `awips:NOWFWS` (1): `l3-fws-now-19950517-2304`
- `awips:NRRTLX` (1): `l3-tlx-nrr-20260622-080623`
- `awips:NSPTLX` (1): `l3-tlx-nsp-20130520-2016`
- `awips:NSSFWS` (1): `l3-fws-nss-19950517-2304`
- `awips:NSSTLX` (2): `l3-tlx-nss-20130520-2016`, `l3-tlx-nss-20220503-005231`
- `awips:NSTFWS` (1): `l3-fws-nst-19950517-2304`
- `awips:NSTILX` (1): `l3-ilx-nst-19960419-2303`
- `awips:NSTMCI` (1): `l3-mci-nst-20160526-2154`
- `awips:NSTOKC` (1): `l3-okc-nst-20260622-080640`
- `awips:NSTTLX` (2): `l3-tlx-nst-20130520-2016`, `l3-tlx-nst-20260622-080623`
- `awips:NSWTLX` (2): `l3-tlx-nsw-20130520-2016`, `l3-tlx-nsw-20220503-005231`
- `awips:NTPFWS` (1): `l3-fws-ntp-19950517-2304`
- `awips:NTPMCI` (1): `l3-mci-ntp-20160526-2154`
- `awips:NTPTLX` (2): `l3-tlx-ntp-20130520-2016`, `l3-tlx-ntp-20260629-173638`
- `awips:NTVILX` (1): `l3-ilx-ntv-19960419-2303`
- `awips:NTVLZK` (1): `l3-lzk-ntv-19970301-2027`
- `awips:NTVOKC` (1): `l3-okc-ntv-20220503-005210`
- `awips:NTVSGF` (1): `l3-sgf-ntv-20030504-2352`
- `awips:NTVTLX` (2): `l3-tlx-ntv-20130520-2016`, `l3-tlx-ntv-20220503-005231`
- `awips:NVLMCI` (1): `l3-mci-nvl-20160526-2154`
- `awips:NVLOKC` (1): `l3-okc-nvl-20260622-080623`
- `awips:NVLTLX` (2): `l3-tlx-nvl-20130520-2012`, `l3-tlx-nvl-20260622-080623`
- `awips:NVWFWS` (1): `l3-fws-nvw-19950517-2322`
- `awips:NVWLOT` (1): `l3-lot-nvw-19931120-0721`
- `awips:NVWMCI` (1): `l3-mci-nvw-20160526-2154`
- `awips:NVWOKC` (1): `l3-okc-nvw-20260622-080623`
- `awips:NVWTLX` (2): `l3-tlx-nvw-20130520-2016`, `l3-tlx-nvw-20260622-080623`
- `awips:NWPFWS` (1): `l3-fws-nwp-19950517-2304`
- `awips:NXFGJX` (1): `l3-gjx-nxf-20200817-0600`
- `awips:NYFRAX` (1): `l3-rax-nyf-20200818-0001`
- `awips:NYQGJX` (1): `l3-gjx-nyq-20220503-005356`
- `awips:NZQSHV` (1): `l3-shv-nzq-20220503-005452`
- `awips:OHATLX` (2): `l3-tlx-oha-20130520-2016`, `l3-tlx-oha-20260622-080623`
- `awips:PTATLX` (2): `l3-tlx-pta-20130520-2016`, `l3-tlx-pta-20200501-000023`
- `awips:RCMFWS` (1): `l3-fws-rcm-19950517-2310`
- `awips:RCMILX` (1): `l3-ilx-rcm-19960419-2309`
- `awips:RCMTLX` (2): `l3-tlx-rcm-20130520-2016`, `l3-tlx-rcm-20220503-004553`
- `awips:ROBNQA` (1): `l3-knqa-20080205-0018-rob`
- `awips:RSLOKC` (1): `l3-okc-rsl-20220517-085551`
- `awips:RSLTLX` (2): `l3-tlx-rsl-20130520-2358`, `l3-tlx-rsl-20220502-235926`
- `awips:SPDTLX` (2): `l3-tlx-spd-20130520-2016`, `l3-tlx-spd-20220503-005231`
- `awips:SUPFWS` (1): `l3-fws-sup-19950517-2304`
- `awips:TR0JFK` (1): `l3-jfk-tr0-20210120-154051`
- `awips:TR0MCI` (1): `l3-mci-tr0-20160526-2154`
- `awips:TR1MCI` (1): `l3-mci-tr1-20160526-2154`
- `awips:TR2MCI` (1): `l3-mci-tr2-20160526-2154`
- `awips:TV0MCI` (1): `l3-mci-tv0-20160526-2154`
- `awips:TV0OKC` (1): `l3-okc-tv0-20260622-080547`
- `awips:TV0SLC` (1): `l3-slc-tv0-20160516-2359`
- `awips:TV1MCI` (1): `l3-mci-tv1-20160526-2154`
- `awips:TV2MCI` (1): `l3-mci-tv2-20160526-2154`
- `awips:TZ0DEN` (1): `l3-den-tz0-20200804-2226`
- `awips:TZ0OKC` (1): `l3-okc-tz0-20260622-080547`
- `awips:TZ1DEN` (1): `l3-den-tz1-20200804-2226`
- `awips:TZ2DEN` (1): `l3-den-tz2-20200804-2227`
- `awips:TZLMCI` (1): `l3-mci-tzl-20160526-2154`
- `awips:TZLOKC` (1): `l3-okc-tzl-20260622-080623`

#### `band:`

- `band:s` (5): `tmatrix-lut-rain-sband-pytmatrix-0.3.3`, `tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3`, `tmatrix-lut-property-dry-oblate-sband-trim`, `tmatrix-lut-property-wet-oblate-sband-trim`, `tmatrix-lut-property-rain-sband-trim`

#### `base-tilt:`

- `base-tilt:2` (3): `l2-kdgx-20230325-010651`, `l2-kmtx-20240301-212827`, `l2-kmtx-20240301-212827-trim`

#### `block:`

- `block:cell_trend` (2): `l3-tlx-nss-20130520-2016`, `l3-tlx-nss-20220503-005231`
- `block:graphic` (38): `l3-fws-ncz-19950517-2304`, `l3-fws-nhi-19950517-1323`, `l3-fws-nme-19950517-2316`, `l3-fws-nst-19950517-2304`, `l3-grr-039-20011011-0631`, `l3-ilx-ncz-19960419-2320`, `l3-ilx-nhi-19960419-2303`, `l3-ilx-nme-19960419-2303`, `l3-ilx-nst-19960419-2303`, `l3-ilx-ntv-19960419-2303`, `l3-ind-035-19941031-2138`, `l3-lzk-ncz-19970301-1912`, `l3-lzk-nme-19970301-2027`, `l3-lzk-ntv-19970301-2027`, `l3-mci-ncr-20160526-2154`, `l3-mci-nmd-20160526-2154`, `l3-mci-nst-20160526-2154`, `l3-okc-ncr-20260622-080623`, `l3-okc-nhi-20220503-005210`, `l3-okc-nmd-20260622-080640`, `l3-okc-nst-20260622-080640`, `l3-okc-ntv-20220503-005210`, `l3-sgf-nme-20030504-2332`, `l3-sgf-ntv-20030504-2352`, `l3-tlx-nco-20130520-1816`, `l3-tlx-ncr-20130520-2016`, `l3-tlx-ncr-20260622-080623`, `l3-tlx-ncz-19990503-2316`, `l3-tlx-ncz-20130520-2016`, `l3-tlx-ncz-20220503-005231`, `l3-tlx-nhi-20130520-2016`, `l3-tlx-nhi-20220503-005231`, `l3-tlx-nmd-20130520-2016`, `l3-tlx-nmd-20260622-080623`, `l3-tlx-nst-20130520-2016`, `l3-tlx-nst-20260622-080623`, `l3-tlx-ntv-20130520-2016`, `l3-tlx-ntv-20220503-005231`
- `block:rcm` (4): `l3-fws-rcm-19950517-2310`, `l3-ilx-rcm-19960419-2309`, `l3-tlx-rcm-20130520-2016`, `l3-tlx-rcm-20220503-004553`
- `block:standalone_tabular` (17): `l3-ftg-100-19940930-0102`, `l3-ftg-102-19940930-1932`, `l3-ftg-104-19940930-0850`, `l3-ftg-107-19940930-0154`, `l3-ftg-109-19940930-0102`, `l3-fws-nss-19950517-2304`, `l3-ind-073-19940910-1555`, `l3-lot-101-19930824-0005`, `l3-lot-108-19930824-0005`, `l3-tlx-101-20010503-0007`, `l3-tlx-102-19990504-0052`, `l3-tlx-103-20010503-0007`, `l3-tlx-104-20010503-2355`, `l3-tlx-nss-20130520-2016`, `l3-tlx-nss-20220503-005231`, `l3-tlx-spd-20130520-2016`, `l3-tlx-spd-20220503-005231`
- `block:symbology` (244): `l3-akc-nc1-20210730-055033`, `l3-akq-018-19940810-0835`, `l3-bmx-029-19940914-1621`, `l3-byx-n0q-20150124-2106`, `l3-cae-053-19940629-1906`, `l3-cae-n0r-19940629-1906`, `l3-cys-021-19941114-1107`, `l3-ddc-n0q-20200817-0501`, `l3-ddc-n0q-20200817-0503`, `l3-den-tz0-20200804-2226`, `l3-den-tz1-20200804-2226`, `l3-den-tz2-20200804-2227`, `l3-eax-n0q-20200817-0401`, `l3-eax-n0q-20200817-0405`, `l3-ffc-n0q-20140407-1805`, `l3-ftg-022-19940601-1917`, `l3-ftg-026-19940930-0536`, `l3-ftg-043-19940930-1849`, `l3-ftg-044-19940930-1849`, `l3-ftg-045-19940930-1849`, `l3-ftg-046-19940930-1849`, `l3-ftg-n0b-20220304-1820`, `l3-fws-dpa-19950517-2304`, `l3-fws-n0r-19950517-2304`, `l3-fws-n1p-19950517-2304`, `l3-fws-ncz-19950517-2304`, `l3-fws-nhi-19950517-1323`, `l3-fws-nme-19950517-2316`, `l3-fws-now-19950517-2304`, `l3-fws-nst-19950517-2304`, `l3-fws-ntp-19950517-2304`, `l3-fws-nvw-19950517-2322`, `l3-fws-nwp-19950517-2304`, `l3-fws-sup-19950517-2304`, `l3-gjx-n0f-20200817-0551`, `l3-gjx-naf-20200817-0551`, `l3-gjx-nbf-20200817-0551`, `l3-gjx-nxf-20200817-0600`, `l3-gjx-nyq-20220503-005356`, `l3-grr-039-20011011-0631`, `l3-ilx-irm-19960419-2309`, `l3-ilx-ncz-19960419-2320`, `l3-ilx-nhi-19960419-2303`, `l3-ilx-nme-19960419-2303`, `l3-ilx-nst-19960419-2303`, `l3-ilx-ntv-19960419-2303`, `l3-ind-016-19940910-0647`, `l3-ind-017-19940910-1104`, `l3-ind-035-19941031-2138`, `l3-ind-042-19940910-1642`, `l3-jan-024-19951003-1312`, `l3-jfk-tr0-20210120-154051`, `l3-lot-050-19941031-1358`, `l3-lot-053-19941106-0246`, `l3-lot-055-19941031-1137`, `l3-lot-084-19931120-0721`, `l3-lot-irm-19941031-0503`, `l3-lot-n0r-19941106-0246`, `l3-lot-nvw-19931120-0721`, `l3-lzk-h0c-20200814-0417`, `l3-lzk-h0v-20200812-1309`, `l3-lzk-h0w-20200812-1305`, `l3-lzk-h0z-20200812-1318`, `l3-lzk-ncz-19970301-1912`, `l3-lzk-nme-19970301-2027`, `l3-lzk-ntv-19970301-2027`, `l3-mci-dhr-20160526-2154`, `l3-mci-dpa-20160526-2154`, `l3-mci-dsp-20160526-2154`, `l3-mci-n1p-20160526-2154`, `l3-mci-ncr-20160526-2154`, `l3-mci-net-20160526-2154`, `l3-mci-nmd-20160526-2154`, `l3-mci-nst-20160526-2154`, `l3-mci-ntp-20160526-2154`, `l3-mci-nvl-20160526-2154`, `l3-mci-nvw-20160526-2154`, `l3-mci-tr0-20160526-2154`, `l3-mci-tr1-20160526-2154`, `l3-mci-tr2-20160526-2154`, `l3-mci-tv0-20160526-2154`, `l3-mci-tv1-20160526-2154`, `l3-mci-tv2-20160526-2154`, `l3-mci-tzl-20160526-2154`, `l3-mlb-051-19941116-0335`, `l3-okc-ncr-20260622-080623`, `l3-okc-net-20220503-005210`, `l3-okc-nhi-20220503-005210`, `l3-okc-nmd-20260622-080640`, `l3-okc-nst-20260622-080640`, `l3-okc-ntv-20220503-005210`, `l3-okc-nvl-20260622-080623`, `l3-okc-nvw-20260622-080623`, `l3-okc-rsl-20220517-085551`, `l3-okc-tv0-20260622-080547`, `l3-okc-tz0-20260622-080547`, `l3-okc-tzl-20260622-080623`, `l3-rax-dta-20200818-0454`, `l3-rax-nll-20220510-155126`, `l3-rax-nyf-20200818-0001`, `l3-sgf-nme-20030504-2332`, `l3-sgf-ntv-20030504-2352`, `l3-shv-nzq-20220503-005452`, `l3-slc-tv0-20160516-2359`, `l3-tlx-063-19940308-1930`, `l3-tlx-064-19940308-1930`, `l3-tlx-087-19940308-1939`, `l3-tlx-daa-20130520-2016`, `l3-tlx-daa-20260622-080623`, `l3-tlx-dhr-20130520-2016`, `l3-tlx-dhr-20260622-080623`, `l3-tlx-dod-20130520-2016`, `l3-tlx-dod-20220503-005231`, `l3-tlx-dpa-20130520-2016`, `l3-tlx-dpa-20260629-173638`, `l3-tlx-dpr-20130520-2016`, `l3-tlx-dpr-20260622-080623`, `l3-tlx-dsd-20130520-2016`, `l3-tlx-dsd-20220503-005231`, `l3-tlx-dsp-20130520-2016`, `l3-tlx-dsp-20260629-173638`, `l3-tlx-dta-20130520-2016`, `l3-tlx-dta-20260622-080623`, `l3-tlx-du3-20130520-2008`, `l3-tlx-du3-20260622-080623`, `l3-tlx-du6-20260622-120608`, `l3-tlx-dvl-20130520-2016`, `l3-tlx-dvl-20260622-080623`, `l3-tlx-eet-20130520-2016`, `l3-tlx-eet-20260622-080623`, `l3-tlx-hhc-20130520-2016`, `l3-tlx-hhc-20260622-080623`, `l3-tlx-irm-19940308-1115`, `l3-tlx-n0b-20260622-080623`, `l3-tlx-n0c-20130520-2016`, `l3-tlx-n0c-20260622-080623`, `l3-tlx-n0f-20220502-235926`, `l3-tlx-n0g-20260622-080623`, `l3-tlx-n0h-20130520-2016`, `l3-tlx-n0h-20260622-080623`, `l3-tlx-n0k-20130520-2016`, `l3-tlx-n0k-20260622-080623`, `l3-tlx-n0m-20130520-2016`, `l3-tlx-n0m-20260622-080623`, `l3-tlx-n0q-20130520-2016`, `l3-tlx-n0q-20220503-005231`, `l3-tlx-n0r-20130520-2016`, `l3-tlx-n0r-20220908-131957`, `l3-tlx-n0s-20130520-2016`, `l3-tlx-n0s-20260622-080623`, `l3-tlx-n0u-20130520-2016`, `l3-tlx-n0u-20220503-005231`, `l3-tlx-n0v-20130520-2016`, `l3-tlx-n0v-20220908-131957`, `l3-tlx-n0x-20130520-2016`, `l3-tlx-n0x-20260622-080623`, `l3-tlx-n0z-20130520-2016`, `l3-tlx-n0z-20220908-131957`, `l3-tlx-n1c-20130520-2016`, `l3-tlx-n1h-20130520-2016`, `l3-tlx-n1k-20130520-2016`, `l3-tlx-n1m-20130520-2016`, `l3-tlx-n1p-20130520-2016`, `l3-tlx-n1p-20260629-173638`, `l3-tlx-n1q-20130520-2016`, `l3-tlx-n1s-20130520-2016`, `l3-tlx-n1u-20130520-2016`, `l3-tlx-n1x-20130520-2016`, `l3-tlx-n2c-20130520-2016`, `l3-tlx-n2h-20130520-2016`, `l3-tlx-n2k-20130520-2016`, `l3-tlx-n2m-20130520-2016`, `l3-tlx-n2q-20130520-2016`, `l3-tlx-n2s-20130520-2016`, `l3-tlx-n2u-20130520-2016`, `l3-tlx-n2x-20130520-2016`, `l3-tlx-n3c-20130520-2016`, `l3-tlx-n3h-20130520-2016`, `l3-tlx-n3k-20130520-2016`, `l3-tlx-n3m-20130520-2016`, `l3-tlx-n3p-20130520-2012`, `l3-tlx-n3p-20220503-011226`, `l3-tlx-n3q-20130520-2016`, `l3-tlx-n3s-20130520-2016`, `l3-tlx-n3u-20130520-2016`, `l3-tlx-n3x-20130520-2016`, `l3-tlx-nac-20130520-2016`, `l3-tlx-nah-20130520-2016`, `l3-tlx-nak-20130520-2016`, `l3-tlx-nam-20130520-2016`, `l3-tlx-naq-20130520-2016`, `l3-tlx-nau-20130520-2016`, `l3-tlx-nax-20130520-2016`, `l3-tlx-nbc-20130520-2016`, `l3-tlx-nbh-20130520-2016`, `l3-tlx-nbk-20130520-2016`, `l3-tlx-nbm-20130520-2016`, `l3-tlx-nbq-20130520-2016`, `l3-tlx-nbu-20130520-2016`, `l3-tlx-nbx-20130520-2016`, `l3-tlx-nc1-20130520-2354`, `l3-tlx-nc2-20130520-2354`, `l3-tlx-nc3-20130520-2354`, `l3-tlx-nc4-20130520-2354`, `l3-tlx-nc5-20130520-2354`, `l3-tlx-nco-20130520-1816`, `l3-tlx-ncr-20130520-2016`, `l3-tlx-ncr-20260622-080623`, `l3-tlx-ncz-19990503-2316`, `l3-tlx-ncz-20130520-2016`, `l3-tlx-ncz-20220503-005231`, `l3-tlx-net-20130520-2016`, `l3-tlx-net-20220503-005231`, `l3-tlx-nhi-20130520-2016`, `l3-tlx-nhi-20220503-005231`, `l3-tlx-nhl-20130520-2016`, `l3-tlx-nhl-20220503-005231`, `l3-tlx-nla-20130520-2016`, `l3-tlx-nla-20220503-005231`, `l3-tlx-nll-20130520-2016`, `l3-tlx-nmd-20130520-2016`, `l3-tlx-nmd-20260622-080623`, `l3-tlx-nml-20130520-2016`, `l3-tlx-nml-20220503-005231`, `l3-tlx-nrr-20260622-080623`, `l3-tlx-nsp-20130520-2016`, `l3-tlx-nst-20130520-2016`, `l3-tlx-nst-20260622-080623`, `l3-tlx-nsw-20130520-2016`, `l3-tlx-nsw-20220503-005231`, `l3-tlx-ntp-20130520-2016`, `l3-tlx-ntp-20260629-173638`, `l3-tlx-ntv-20130520-2016`, `l3-tlx-ntv-20220503-005231`, `l3-tlx-nvl-20130520-2012`, `l3-tlx-nvl-20260622-080623`, `l3-tlx-nvw-20130520-2016`, `l3-tlx-nvw-20260622-080623`, `l3-tlx-oha-20130520-2016`, `l3-tlx-oha-20260622-080623`, `l3-tlx-pta-20130520-2016`, `l3-tlx-pta-20200501-000023`, `l3-tlx-rsl-20130520-2358`, `l3-tlx-rsl-20220502-235926`
- `block:tabular` (47): `l3-fws-n1p-19950517-2304`, `l3-fws-nhi-19950517-1323`, `l3-fws-nme-19950517-2316`, `l3-fws-nst-19950517-2304`, `l3-fws-ntp-19950517-2304`, `l3-fws-nvw-19950517-2322`, `l3-ilx-irm-19960419-2309`, `l3-ilx-nhi-19960419-2303`, `l3-ilx-nme-19960419-2303`, `l3-ilx-nst-19960419-2303`, `l3-ilx-ntv-19960419-2303`, `l3-lzk-nme-19970301-2027`, `l3-lzk-ntv-19970301-2027`, `l3-mci-n1p-20160526-2154`, `l3-mci-nmd-20160526-2154`, `l3-mci-nst-20160526-2154`, `l3-mci-ntp-20160526-2154`, `l3-mci-nvw-20160526-2154`, `l3-okc-nhi-20220503-005210`, `l3-okc-nmd-20260622-080640`, `l3-okc-nst-20260622-080640`, `l3-okc-ntv-20220503-005210`, `l3-okc-nvw-20260622-080623`, `l3-rax-dta-20200818-0454`, `l3-sgf-nme-20030504-2332`, `l3-sgf-ntv-20030504-2352`, `l3-tlx-087-19940308-1939`, `l3-tlx-dta-20260622-080623`, `l3-tlx-irm-19940308-1115`, `l3-tlx-n1p-20130520-2016`, `l3-tlx-n1p-20260629-173638`, `l3-tlx-n3p-20130520-2012`, `l3-tlx-n3p-20220503-011226`, `l3-tlx-nhi-20130520-2016`, `l3-tlx-nhi-20220503-005231`, `l3-tlx-nmd-20130520-2016`, `l3-tlx-nmd-20260622-080623`, `l3-tlx-nst-20130520-2016`, `l3-tlx-nst-20260622-080623`, `l3-tlx-ntp-20130520-2016`, `l3-tlx-ntp-20260629-173638`, `l3-tlx-ntv-20130520-2016`, `l3-tlx-ntv-20220503-005231`, `l3-tlx-nvw-20130520-2016`, `l3-tlx-nvw-20260622-080623`, `l3-tlx-pta-20130520-2016`, `l3-tlx-pta-20200501-000023`

#### `bucket:`

- `bucket:unidata-nexrad-level2` (34): `l2-ktlx-19910605-162126`, `l2-ktlx-19990503-230052`, `l2-ktlx-19990504-002218`, `l2-ktlx-20030508-221041`, `l2-klix-20050829-130035`, `l2-kvwx-20080415-235337`, `l2-kpah-20080415-235014`, `l2-kdmx-20080525-205148`, `l2-kvnx-20110315-000203`, `l2-ktlx-20130520-201643`, `l2-kgwx-20130601-235640`, `l2-koax-20140616-205305`, `l2-kewx-20160413-022531`, `l2-kdvn-20200810-175718`, `l2-kdvn-20200810-180401`, `l2-kdvn-20200810-181043`, `l2-kdvn-20200810-181724`, `l2-klix-20210829-180425`, `l2-klix-20210829-173117`, `l2-klix-20210829-175748`, `l2-klix-20210829-175748-mdm`, `l2-kbox-20220129-150537`, `l2-tjua-20220918-190621`, `l2-kdgx-20230325-010651`, `l2-kmaf-20230331-230843`, `l2-tstl-20230331-230314`, `l2-pgua-20230524-030945`, `l2-tbwi-20230601-175101-stub`, `l2-kmtx-20240301-212827`, `l2-ktlx-20240315-000217`, `l2-ktlx-20240515-000014`, `l2-pahg-20250909-212549`, `l2-kilx-20260418-013553`, `l2-kiwa-20260917-003629`
- `bucket:unidata-nexrad-level2-chunks` (75): `l2chunk-kiwa-307-20260917-003629-001-s`, `l2chunk-kiwa-307-20260917-003629-{002..069}-i`, `l2chunk-kiwa-307-20260917-003629-070-e`, `l2chunk-tlas-998-20260917-012843-001-s`, `l2chunk-tlas-999-20260917-013443-001-s`, `l2chunk-tlas-3-20260917-015242-001-s`, `l2chunk-tlas-3-20260917-015242-002-i`, `l2chunk-tlas-3-20260917-015242-003-i`
- `bucket:unidata-nexrad-level3` (5): `l3-kdvn-20200810-1757-nst`, `l3-kdvn-20200810-1804-nst`, `l3-kdvn-20200810-1810-nst`, `l3-kdvn-20200810-1817-nst`, `l3-ktlx-20260622-080806-n0b-sails`

#### `build:`

- `build:10.0` (3): `l2-kpah-20080415-235014`, `l2-kdmx-20080525-205148`, `l2-kdmx-20080525-205148-trim`
- `build:12.0` (1): `l2-kvnx-20110315-000203`
- `build:13.1` (1): `l2-kgwx-20130601-235640`
- `build:13.2` (2): `l2-ktlx-20130520-201643`, `l2-ktlx-20130520-201643-trim`
- `build:14.0` (2): `l2-koax-20140616-205305`, `l2-koax-20140616-205305-trim`
- `build:16.1` (2): `l2-kewx-20160413-022531`, `l2-kewx-20160413-022531-trim`
- `build:18.2` (5): `l2-kdvn-20200810-175718`, `l2-kdvn-20200810-180401`, `l2-kdvn-20200810-181043`, `l2-kdvn-20200810-181724`, `l2-kdvn-20200810-180401-trim`
- `build:19.1` (4): `l2-klix-20210829-180425`, `l2-klix-20210829-173117`, `l2-klix-20210829-175748`, `l2-klix-20210829-180425-trim`
- `build:20.1` (3): `l2-kbox-20220129-150537`, `l2-tjua-20220918-190621`, `l2-kbox-20220129-150537-trim`
- `build:21.0` (1): `l2-kmaf-20230331-230843`
- `build:21.1` (3): `l2-kdgx-20230325-010651`, `l2-pgua-20230524-030945`, `l2-pgua-20230524-030945-trim`
- `build:22.0` (4): `l2-kmtx-20240301-212827`, `l2-ktlx-20240315-000217`, `l2-kmtx-20240301-212827-trim`, `l2-ktlx-20240315-000217-trim`
- `build:22.1` (1): `l2-ktlx-20240515-000014`
- `build:23.1` (3): `l2-pahg-20250909-212549`, `l2-kilx-20260418-013553`, `l2-kilx-20260418-013553-trim`
- `build:24.1` (71): `l2-kiwa-20260917-003629`, `l2chunk-kiwa-307-20260917-003629-001-s`, `l2chunk-kiwa-307-20260917-003629-{002..069}-i`, `l2chunk-kiwa-307-20260917-003629-070-e`

#### `carryover:`

- `carryover:bowecho` (14): `odim-bejab-20190606-0000-pvol`, `odim-bewid-20130429-0430-pvol-dbzh-scan1`, `odim-norst-20170421-0908-pvol`, `odim-espdg-20260707-1927-pvol-dbzh-vradh`, `odim-imgw-ram-20260711-0015-kdp-max`, `odim-imgw-ram-20260711-0015-phidp-max`, `odim-imgw-ram-20260711-0015-rhohv-max`, `odim-imgw-ram-20260711-0015-zdr-max`, `cfrad1-xsapr-sgp-20110520-ppi-netcdf4`, `cfrad1-xsapr-sgp-20110520-ppi-classic`, `cfrad1-dow8-20211011-223602-rhi-trim3-classic`, `cfrad2-xradar-xsapr-sgp-20110520-ppi`, `dorade-cow2-20260521-225514-sur-head24`, `l3-kbmx-19980416-0006-nvw`

#### `cfradial:`

- `cfradial:1.2` (5): `cfrad1-xsapr-sgp-20110520-ppi-netcdf4`, `cfrad1-xsapr-sgp-20110520-ppi-netcdf4-user-types`, `cfrad1-xsapr-sgp-20110520-ppi-netcdf4-szip-lzf`, `cfrad1-xsapr-sgp-20110520-ppi-classic`, `cfrad1-spol-20080604-002217-sur`
- `cfradial:1.3` (1): `cfrad1-irene-sr2-20110827-120420-sur-sweeps01`
- `cfradial:1.4` (4): `cfrad1-dow8-20211011-223602-rhi`, `cfrad1-dow8-20211011-223602-rhi-trim3-classic`, `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-per-ray-geometry`, `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-finest-geometry-netcdf4`
- `cfradial:2` (5): `cfrad2-spol-20080604-002217-sur`, `cfrad2-radx-irene-sr2-20110827-120420-sur-r30km`, `cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32`, `cfrad2-xradar-xsapr-sgp-20110520-ppi`, `cfrad2-xradar-dow8-20211011-223602-rhi-r300`
- `cfradial:n-gates-vary` (2): `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-per-ray-geometry`, `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-finest-geometry-netcdf4`
- `cfradial:range-time-range` (1): `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-per-ray-geometry`

#### `cfradial2:`

- `cfradial2:azimuth-ray-dim` (2): `cfrad2-xradar-xsapr-sgp-20110520-ppi`, `cfrad2-xradar-dow8-20211011-223602-rhi-r300`
- `cfradial2:monitoring-group` (1): `cfrad2-radx-irene-sr2-20110827-120420-sur-r30km`
- `cfradial2:per-sweep-packing` (1): `cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32`
- `cfradial2:per-sweep-range` (1): `cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32`
- `cfradial2:r-calib` (1): `cfrad2-radx-irene-sr2-20110827-120420-sur-r30km`
- `cfradial2:root-platform-track` (1): `cfrad2-xradar-dow8-20211011-223602-rhi-r300`

#### `chunk:`

- `chunk:end` (1): `l2chunk-kiwa-307-20260917-003629-070-e`
- `chunk:intermediate` (70): `l2chunk-kiwa-307-20260917-003629-{002..069}-i`, `l2chunk-tlas-3-20260917-015242-002-i`, `l2chunk-tlas-3-20260917-015242-003-i`
- `chunk:start` (4): `l2chunk-kiwa-307-20260917-003629-001-s`, `l2chunk-tlas-998-20260917-012843-001-s`, `l2chunk-tlas-999-20260917-013443-001-s`, `l2chunk-tlas-3-20260917-015242-001-s`

#### `chunk-volume:`

- `chunk-volume:kiwa-307` (71): `l2-kiwa-20260917-003629`, `l2chunk-kiwa-307-20260917-003629-001-s`, `l2chunk-kiwa-307-20260917-003629-{002..069}-i`, `l2chunk-kiwa-307-20260917-003629-070-e`
- `chunk-volume:tlas-3` (3): `l2chunk-tlas-3-20260917-015242-001-s`, `l2chunk-tlas-3-20260917-015242-002-i`, `l2chunk-tlas-3-20260917-015242-003-i`
- `chunk-volume:tlas-998` (1): `l2chunk-tlas-998-20260917-012843-001-s`
- `chunk-volume:tlas-999` (1): `l2chunk-tlas-999-20260917-013443-001-s`

#### `compression:`

- `compression:bzip2` (104): `l3-byx-n0q-20150124-2106`, `l3-ddc-n0q-20200817-0501`, `l3-ddc-n0q-20200817-0503`, `l3-den-tz0-20200804-2226`, `l3-den-tz1-20200804-2226`, `l3-den-tz2-20200804-2227`, `l3-eax-n0q-20200817-0401`, `l3-eax-n0q-20200817-0405`, `l3-ffc-n0q-20140407-1805`, `l3-ftg-n0b-20220304-1820`, `l3-gjx-n0f-20200817-0551`, `l3-gjx-naf-20200817-0551`, `l3-gjx-nbf-20200817-0551`, `l3-gjx-nxf-20200817-0600`, `l3-gjx-nyq-20220503-005356`, `l3-lzk-h0c-20200814-0417`, `l3-lzk-h0v-20200812-1309`, `l3-lzk-h0w-20200812-1305`, `l3-lzk-h0z-20200812-1318`, `l3-mci-dhr-20160526-2154`, `l3-mci-tv0-20160526-2154`, `l3-mci-tv1-20160526-2154`, `l3-mci-tv2-20160526-2154`, `l3-mci-tzl-20160526-2154`, `l3-okc-rsl-20220517-085551`, `l3-okc-tv0-20260622-080547`, `l3-okc-tz0-20260622-080547`, `l3-okc-tzl-20260622-080623`, `l3-rax-dta-20200818-0454`, `l3-rax-nyf-20200818-0001`, `l3-shv-nzq-20220503-005452`, `l3-slc-tv0-20160516-2359`, `l3-tlx-daa-20130520-2016`, `l3-tlx-daa-20260622-080623`, `l3-tlx-dhr-20130520-2016`, `l3-tlx-dhr-20260622-080623`, `l3-tlx-dod-20130520-2016`, `l3-tlx-dod-20220503-005231`, `l3-tlx-dpr-20130520-2016`, `l3-tlx-dpr-20260622-080623`, `l3-tlx-dsd-20130520-2016`, `l3-tlx-dsd-20220503-005231`, `l3-tlx-dsp-20130520-2016`, `l3-tlx-dsp-20260629-173638`, `l3-tlx-dta-20130520-2016`, `l3-tlx-dta-20260622-080623`, `l3-tlx-du3-20130520-2008`, `l3-tlx-du3-20260622-080623`, `l3-tlx-du6-20260622-120608`, `l3-tlx-dvl-20130520-2016`, `l3-tlx-dvl-20260622-080623`, `l3-tlx-eet-20130520-2016`, `l3-tlx-eet-20260622-080623`, `l3-tlx-hhc-20130520-2016`, `l3-tlx-hhc-20260622-080623`, `l3-tlx-n0b-20260622-080623`, `l3-tlx-n0c-20130520-2016`, `l3-tlx-n0c-20260622-080623`, `l3-tlx-n0f-20220502-235926`, `l3-tlx-n0g-20260622-080623`, `l3-tlx-n0h-20130520-2016`, `l3-tlx-n0h-20260622-080623`, `l3-tlx-n0k-20130520-2016`, `l3-tlx-n0k-20260622-080623`, `l3-tlx-n0q-20130520-2016`, `l3-tlx-n0q-20220503-005231`, `l3-tlx-n0u-20130520-2016`, `l3-tlx-n0u-20220503-005231`, `l3-tlx-n0x-20130520-2016`, `l3-tlx-n0x-20260622-080623`, `l3-tlx-n1c-20130520-2016`, `l3-tlx-n1h-20130520-2016`, `l3-tlx-n1k-20130520-2016`, `l3-tlx-n1q-20130520-2016`, `l3-tlx-n1u-20130520-2016`, `l3-tlx-n1x-20130520-2016`, `l3-tlx-n2c-20130520-2016`, `l3-tlx-n2h-20130520-2016`, `l3-tlx-n2k-20130520-2016`, `l3-tlx-n2q-20130520-2016`, `l3-tlx-n2u-20130520-2016`, `l3-tlx-n2x-20130520-2016`, `l3-tlx-n3c-20130520-2016`, `l3-tlx-n3h-20130520-2016`, `l3-tlx-n3k-20130520-2016`, `l3-tlx-n3q-20130520-2016`, `l3-tlx-n3u-20130520-2016`, `l3-tlx-n3x-20130520-2016`, `l3-tlx-nac-20130520-2016`, `l3-tlx-nah-20130520-2016`, `l3-tlx-nak-20130520-2016`, `l3-tlx-naq-20130520-2016`, `l3-tlx-nau-20130520-2016`, `l3-tlx-nax-20130520-2016`, `l3-tlx-nbc-20130520-2016`, `l3-tlx-nbh-20130520-2016`, `l3-tlx-nbk-20130520-2016`, `l3-tlx-nbq-20130520-2016`, `l3-tlx-nbu-20130520-2016`, `l3-tlx-nbx-20130520-2016`, `l3-tlx-nrr-20260622-080623`, `l3-tlx-rsl-20130520-2358`, `l3-tlx-rsl-20220502-235926`, `l3-ktlx-20260622-080806-n0b-sails`
- `compression:gzip` (13): `l2-ktlx-19910605-162126`, `l2-ktlx-19990503-230052`, `l2-ktlx-19990504-002218`, `l2-ktlx-20030508-221041`, `l2-klix-20050829-130035`, `l2-kvwx-20080415-235337`, `l2-kpah-20080415-235014`, `l2-kdmx-20080525-205148`, `l2-kvnx-20110315-000203`, `l2-ktlx-20130520-201643`, `l2-kgwx-20130601-235640`, `l2-koax-20140616-205305`, `l2-kewx-20160413-022531`
- `compression:ldm-bzip2` (37): `l2-kdvn-20200810-175718`, `l2-kdvn-20200810-180401`, `l2-kdvn-20200810-181043`, `l2-kdvn-20200810-181724`, `l2-klix-20210829-180425`, `l2-klix-20210829-173117`, `l2-klix-20210829-175748`, `l2-klix-20210829-175748-mdm`, `l2-kbox-20220129-150537`, `l2-tjua-20220918-190621`, `l2-kdgx-20230325-010651`, `l2-kmaf-20230331-230843`, `l2-tstl-20230331-230314`, `l2-pgua-20230524-030945`, `l2-tbwi-20230601-175101-stub`, `l2-kmtx-20240301-212827`, `l2-ktlx-20240315-000217`, `l2-ktlx-20240515-000014`, `l2-pahg-20250909-212549`, `l2-kilx-20260418-013553`, `l2-kiwa-20260917-003629`, `l2-ktlx-19910605-162126-trim`, `l2-ktlx-19990504-002218-trim`, `l2-ktlx-20030508-221041-trim`, `l2-klix-20050829-130035-trim`, `l2-kdmx-20080525-205148-trim`, `l2-ktlx-20130520-201643-trim`, `l2-koax-20140616-205305-trim`, `l2-kewx-20160413-022531-trim`, `l2-kdvn-20200810-180401-trim`, `l2-klix-20210829-180425-trim`, `l2-kbox-20220129-150537-trim`, `l2-tstl-20230331-230314-trim`, `l2-pgua-20230524-030945-trim`, `l2-kmtx-20240301-212827-trim`, `l2-ktlx-20240315-000217-trim`, `l2-kilx-20260418-013553-trim`

#### `container:`

- `container:netcdf3-classic` (5): `fuzz-cfradial-overlapping-sweep-ray-ranges`, `fuzz-writers-cfradial1-ray-time-near-float-max`, `cfrad1-xsapr-sgp-20110520-ppi-classic`, `cfrad1-dow8-20211011-223602-rhi-trim3-classic`, `cfrad1-irene-sr2-20110827-120420-sur-sweeps01`
- `container:netcdf4` (10): `cfrad1-xsapr-sgp-20110520-ppi-netcdf4`, `cfrad1-xsapr-sgp-20110520-ppi-netcdf4-user-types`, `cfrad1-xsapr-sgp-20110520-ppi-netcdf4-szip-lzf`, `cfrad1-dow8-20211011-223602-rhi`, `cfrad1-spol-20080604-002217-sur`, `cfrad2-spol-20080604-002217-sur`, `cfrad2-radx-irene-sr2-20110827-120420-sur-r30km`, `cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32`, `cfrad2-xradar-xsapr-sgp-20110520-ppi`, `cfrad2-xradar-dow8-20211011-223602-rhi-r300`

#### `contains:`

- `contains:dorade` (3): `dorade-noxp-20090501-sweeps-tgz`, `dorade-noxp-20090525-sweeps-tgz`, `dorade-noxp-20090610-003210-heads-zip`
- `contains:nexrad-level3` (2): `l3-kbmx-19980416-archive-tarz`, `l3-knqa-20080205-archive-tarz`
- `contains:odim-h5` (1): `odim-au24-20260610-000300-nci-zip-member`

#### `country:`

- `country:AU` (2): `odim-au24-20260610-000300-nci-zip-member`, `odim-au02-20260921-0000-pvol-subset`
- `country:BE` (4): `odim-bejab-20190606-0000-pvol`, `odim-bewid-20130429-0430-pvol-dbzh-scan1`, `odim-bejab-20260612-1450-dbzh`, `odim-bejab-20260612-1450-vrad`
- `country:DE` (1): `odim-deboo-20260924-2130-sweep-th-00`
- `country:DK` (4): `odim-dkrom-20260820-1130-pvol`, `odim-dkrom-20260820-1130-pvol-h5latest-trim`, `odim-dkrom-20260820-1130-pvol-h5edge-paged-ea`, `odim-dkrom-20260820-1130-pvol-h5edge-len4`
- `country:ES` (1): `odim-espdg-20260707-1927-pvol-dbzh-vradh`
- `country:FI` (3): `odim-fianj-20260924-2130-pvol-dataset1-trim`, `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-per-ray-geometry`, `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-finest-geometry-netcdf4`
- `country:IE` (2): `odim-iesha-20260305-0115-pvol`, `cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32`
- `country:IT` (1): `odim-itdes-20260924-2135-pvol-class`
- `country:JP` (8): `jma-n5-20191012-090000`, `jma-n6-20191012-090000`, `jma-n5-20191012-090000-rs47773`, `jma-n6-20191012-090000-rs47773`, `jma-n5-20260924-210000`, `jma-n6-20260924-210000`, `jma-n5-20260924-210000-rs47937`, `jma-n6-20260924-210000-rs47937`
- `country:NO` (4): `odim-norst-20170421-0908-pvol`, `odim-nohur-20260612-1445-dbzh`, `odim-nohur-20260612-1445-th`, `odim-nohur-20260612-1446-vradh`
- `country:PL` (4): `odim-imgw-ram-20260711-0015-kdp-max`, `odim-imgw-ram-20260711-0015-phidp-max`, `odim-imgw-ram-20260711-0015-rhohv-max`, `odim-imgw-ram-20260711-0015-zdr-max`
- `country:SE` (1): `odim-seang-20260924-2130-qcvol-dataset1-trim`
- `country:US` (2): `ndswc-kxwa-20260924-214316`, `ndswc-kxwa-20260924-214316-head41`

#### `derivation:`

- `derivation:archive-member` (9): `dorade-noxp-20090501-190244-ppi`, `dorade-noxp-20090501-190324-ppi`, `dorade-noxp-20090525-203211-sector`, `jma-n5-20191012-090000-rs47773`, `jma-n6-20191012-090000-rs47773`, `jma-n5-20260924-210000-rs47937`, `jma-n6-20260924-210000-rs47937`, `l3-kbmx-19980416-0006-nvw`, `l3-knqa-20080205-0018-rob`
- `derivation:container-conversion` (10): `odim-dkrom-20260820-1130-pvol-h5latest-trim`, `odim-dkrom-20260820-1130-pvol-h5edge-paged-ea`, `odim-dkrom-20260820-1130-pvol-h5edge-len4`, `cfrad1-xsapr-sgp-20110520-ppi-classic`, `cfrad1-dow8-20211011-223602-rhi-trim3-classic`, `cfrad2-radx-irene-sr2-20110827-120420-sur-r30km`, `cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32`, `cfrad2-xradar-xsapr-sgp-20110520-ppi`, `cfrad2-xradar-dow8-20211011-223602-rhi-r300`, `dorade-noxp-20090610-003210-heads-zip`
- `derivation:fuzz-mutation` (14): `fuzz-odim-hdf5-local-heap-name-offset-overflow`, `fuzz-hdf5-chunk-offset-overflow`, `fuzz-cfradial-overlapping-sweep-ray-ranges`, `fuzz-level2-writer-nexrad-moment-nan-scale`, `fuzz-writers-l2-sweep-without-gates`, `fuzz-writers-l2-one-gate-sweep`, `fuzz-writers-l2-odim-rstart-beyond-20-km`, `fuzz-writers-dorade-ray-without-time`, `fuzz-writers-l2-empty-field-name`, `fuzz-writers-odim-gate-spacing-below-float`, `fuzz-writers-dorade-absent-rows-without-fill`, `fuzz-writers-cfradial1-ray-time-near-float-max`, `fuzz-level3-rcm-centroid-non-ascii`, `fuzz-level2-records-volume-header-date-overflow`
- `derivation:gunzip` (1): `odim-itdes-20260924-2135-pvol-class`
- `derivation:head-trim` (7): `dorade-cow2-20260521-225514-sur-head24`, `dorade-dow6-20211230-222139-rhi-head41`, `dorade-noxp-20090610-003210-ppi-head6`, `dorade-noxp-20090610-003222-ppi-head6`, `dorade-noxp-20090610-003226-ppi-head6`, `dorade-n42rf-ts-20181010-122951-air-head24`, `dorade-n42rf-tm-20181010-123925-air-head48`
- `derivation:prefix` (2): `wrf-p3-lookup-table-1-v5.4-2momI-first-block`, `wrf-p3-lookup-table-1-v5.4-3momI-first-block`
- `derivation:subset` (15): `odim-seang-20260924-2130-qcvol-dataset1-trim`, `odim-fianj-20260924-2130-pvol-dataset1-trim`, `cfrad1-irene-sr2-20110827-120420-sur-sweeps01`, `cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32`, `cfrad2-xradar-dow8-20211011-223602-rhi-r300`, `odim-au02-20260921-0000-pvol-subset`, `tmatrix-lut-property-dry-oblate-sband-trim`, `tmatrix-lut-property-dry-oblate-sband-trim-config`, `tmatrix-lut-property-dry-oblate-sband-trim-trim`, `tmatrix-lut-property-wet-oblate-sband-trim`, `tmatrix-lut-property-wet-oblate-sband-trim-config`, `tmatrix-lut-property-wet-oblate-sband-trim-trim`, `tmatrix-lut-property-rain-sband-trim`, `tmatrix-lut-property-rain-sband-trim-config`, `tmatrix-lut-property-rain-sband-trim-trim`

#### `dorade:`

- `dorade:celv` (5): `dorade-dow6-20211230-222139-rhi-head41`, `dorade-n42rf-ts-20181010-122951-air`, `dorade-n42rf-ts-20181010-122951-air-head24`, `dorade-n42rf-tm-20181010-123925-air`, `dorade-n42rf-tm-20181010-123925-air-head48`
- `dorade:csfd` (7): `dorade-cow2-20260521-225514-sur-head24`, `dorade-noxp-20090501-190244-ppi`, `dorade-noxp-20090501-190324-ppi`, `dorade-noxp-20090525-203211-sector`, `dorade-noxp-20090610-003210-ppi-head6`, `dorade-noxp-20090610-003222-ppi-head6`, `dorade-noxp-20090610-003226-ppi-head6`
- `dorade:rle` (6): `dorade-cow2-20260521-225514-sur-head24`, `dorade-dow6-20211230-222139-rhi-head41`, `dorade-n42rf-ts-20181010-122951-air`, `dorade-n42rf-ts-20181010-122951-air-head24`, `dorade-n42rf-tm-20181010-123925-air`, `dorade-n42rf-tm-20181010-123925-air-head48`
- `dorade:uncompressed` (6): `dorade-noxp-20090501-190244-ppi`, `dorade-noxp-20090501-190324-ppi`, `dorade-noxp-20090525-203211-sector`, `dorade-noxp-20090610-003210-ppi-head6`, `dorade-noxp-20090610-003222-ppi-head6`, `dorade-noxp-20090610-003226-ppi-head6`

#### `dtype:`

- `dtype:float64` (1): `odim-espdg-20260707-1927-pvol-dbzh-vradh`
- `dtype:int8-packed` (2): `cfrad1-irene-sr2-20110827-120420-sur-sweeps01`, `cfrad2-radx-irene-sr2-20110827-120420-sur-r30km`
- `dtype:int16` (1): `odim-seang-20260924-2130-qcvol-dataset1-trim`
- `dtype:int16-packed` (1): `cfrad2-xradar-dow8-20211011-223602-rhi-r300`
- `dtype:int32-packed` (1): `cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32`

#### `echo:`

- `echo:convective` (1): `dorade-noxp-20090525-203211-sector`
- `echo:widespread` (1): `odim-iesha-20260305-0115-pvol`

#### `edge:`

- `edge:header-version-mismatch` (1): `l2-kvwx-20080415-235337`
- `edge:no-volume-header` (1): `l2-klix-20210829-175748-mdm`
- `edge:size-0xffff` (1): `l2-klix-20210829-175748-mdm`
- `edge:status-only` (1): `l2-tbwi-20230601-175101-stub`
- `edge:truncated` (1): `l2-ktlx-19990503-230052`

#### `elev:`

- `elev:1` (8): `l2chunk-kiwa-307-20260917-003629-{002..007}-i`, `l2chunk-tlas-3-20260917-015242-002-i`, `l2chunk-tlas-3-20260917-015242-003-i`
- `elev:2` (6): `l2chunk-kiwa-307-20260917-003629-{008..013}-i`
- `elev:3` (6): `l2chunk-kiwa-307-20260917-003629-{014..019}-i`
- `elev:4` (6): `l2chunk-kiwa-307-20260917-003629-{020..025}-i`
- `elev:5` (6): `l2chunk-kiwa-307-20260917-003629-{026..031}-i`
- `elev:6` (6): `l2chunk-kiwa-307-20260917-003629-{032..037}-i`
- `elev:7` (3): `l2chunk-kiwa-307-20260917-003629-{038..040}-i`
- `elev:8` (6): `l2chunk-kiwa-307-20260917-003629-{041..046}-i`
- `elev:9` (6): `l2chunk-kiwa-307-20260917-003629-{047..052}-i`
- `elev:10` (3): `l2chunk-kiwa-307-20260917-003629-{053..055}-i`
- `elev:11` (3): `l2chunk-kiwa-307-20260917-003629-{056..058}-i`
- `elev:12` (3): `l2chunk-kiwa-307-20260917-003629-{059..061}-i`
- `elev:13` (3): `l2chunk-kiwa-307-20260917-003629-{062..064}-i`
- `elev:14` (3): `l2chunk-kiwa-307-20260917-003629-{065..067}-i`
- `elev:15` (3): `l2chunk-kiwa-307-20260917-003629-068-i`, `l2chunk-kiwa-307-20260917-003629-069-i`, `l2chunk-kiwa-307-20260917-003629-070-e`

#### `endian:`

- `endian:big` (2): `fuzz-writers-dorade-ray-without-time`, `dorade-cow2-20260521-225514-sur-head24`
- `endian:little` (12): `fuzz-writers-dorade-absent-rows-without-fill`, `dorade-noxp-20090501-190244-ppi`, `dorade-noxp-20090501-190324-ppi`, `dorade-noxp-20090525-203211-sector`, `dorade-dow6-20211230-222139-rhi-head41`, `dorade-noxp-20090610-003210-ppi-head6`, `dorade-noxp-20090610-003222-ppi-head6`, `dorade-noxp-20090610-003226-ppi-head6`, `dorade-n42rf-ts-20181010-122951-air`, `dorade-n42rf-ts-20181010-122951-air-head24`, `dorade-n42rf-tm-20181010-123925-air`, `dorade-n42rf-tm-20181010-123925-air-head48`

#### `era:`

- `era:1991` (2): `l2-ktlx-19910605-162126`, `l2-ktlx-19910605-162126-trim`
- `era:1993` (4): `l3-lot-084-19931120-0721`, `l3-lot-101-19930824-0005`, `l3-lot-108-19930824-0005`, `l3-lot-nvw-19931120-0721`
- `era:1994` (31): `l3-akq-018-19940810-0835`, `l3-bmx-029-19940914-1621`, `l3-cae-053-19940629-1906`, `l3-cae-n0r-19940629-1906`, `l3-cys-021-19941114-1107`, `l3-ftg-022-19940601-1917`, `l3-ftg-026-19940930-0536`, `l3-ftg-043-19940930-1849`, `l3-ftg-044-19940930-1849`, `l3-ftg-045-19940930-1849`, `l3-ftg-046-19940930-1849`, `l3-ftg-100-19940930-0102`, `l3-ftg-102-19940930-1932`, `l3-ftg-104-19940930-0850`, `l3-ftg-107-19940930-0154`, `l3-ftg-109-19940930-0102`, `l3-ind-016-19940910-0647`, `l3-ind-017-19940910-1104`, `l3-ind-035-19941031-2138`, `l3-ind-042-19940910-1642`, `l3-ind-073-19940910-1555`, `l3-lot-050-19941031-1358`, `l3-lot-053-19941106-0246`, `l3-lot-055-19941031-1137`, `l3-lot-irm-19941031-0503`, `l3-lot-n0r-19941106-0246`, `l3-mlb-051-19941116-0335`, `l3-tlx-063-19940308-1930`, `l3-tlx-064-19940308-1930`, `l3-tlx-087-19940308-1939`, `l3-tlx-irm-19940308-1115`
- `era:1995` (15): `l3-fws-dpa-19950517-2304`, `l3-fws-n0r-19950517-2304`, `l3-fws-n1p-19950517-2304`, `l3-fws-ncz-19950517-2304`, `l3-fws-nhi-19950517-1323`, `l3-fws-nme-19950517-2316`, `l3-fws-now-19950517-2304`, `l3-fws-nss-19950517-2304`, `l3-fws-nst-19950517-2304`, `l3-fws-ntp-19950517-2304`, `l3-fws-nvw-19950517-2322`, `l3-fws-nwp-19950517-2304`, `l3-fws-rcm-19950517-2310`, `l3-fws-sup-19950517-2304`, `l3-jan-024-19951003-1312`
- `era:1996` (7): `l3-ilx-irm-19960419-2309`, `l3-ilx-ncz-19960419-2320`, `l3-ilx-nhi-19960419-2303`, `l3-ilx-nme-19960419-2303`, `l3-ilx-nst-19960419-2303`, `l3-ilx-ntv-19960419-2303`, `l3-ilx-rcm-19960419-2309`
- `era:1997` (3): `l3-lzk-ncz-19970301-1912`, `l3-lzk-nme-19970301-2027`, `l3-lzk-ntv-19970301-2027`
- `era:1998` (2): `l3-kbmx-19980416-archive-tarz`, `l3-kbmx-19980416-0006-nvw`
- `era:1999` (5): `l2-ktlx-19990503-230052`, `l2-ktlx-19990504-002218`, `l2-ktlx-19990504-002218-trim`, `l3-tlx-102-19990504-0052`, `l3-tlx-ncz-19990503-2316`
- `era:2001` (4): `l3-grr-039-20011011-0631`, `l3-tlx-101-20010503-0007`, `l3-tlx-103-20010503-0007`, `l3-tlx-104-20010503-2355`
- `era:2003` (4): `l2-ktlx-20030508-221041`, `l2-ktlx-20030508-221041-trim`, `l3-sgf-nme-20030504-2332`, `l3-sgf-ntv-20030504-2352`
- `era:2005` (2): `l2-klix-20050829-130035`, `l2-klix-20050829-130035-trim`
- `era:2008` (8): `l2-kvwx-20080415-235337`, `l2-kpah-20080415-235014`, `l2-kdmx-20080525-205148`, `l2-kdmx-20080525-205148-trim`, `cfrad1-spol-20080604-002217-sur`, `cfrad2-spol-20080604-002217-sur`, `l3-knqa-20080205-archive-tarz`, `l3-knqa-20080205-0018-rob`
- `era:2009` (9): `dorade-noxp-20090501-sweeps-tgz`, `dorade-noxp-20090525-sweeps-tgz`, `dorade-noxp-20090501-190244-ppi`, `dorade-noxp-20090501-190324-ppi`, `dorade-noxp-20090525-203211-sector`, `dorade-noxp-20090610-003210-ppi-head6`, `dorade-noxp-20090610-003222-ppi-head6`, `dorade-noxp-20090610-003226-ppi-head6`, `dorade-noxp-20090610-003210-heads-zip`
- `era:2011` (9): `l2-kvnx-20110315-000203`, `l3-abr-ftm-20110428-1331`, `cfrad1-xsapr-sgp-20110520-ppi-netcdf4`, `cfrad1-xsapr-sgp-20110520-ppi-netcdf4-user-types`, `cfrad1-xsapr-sgp-20110520-ppi-netcdf4-szip-lzf`, `cfrad1-xsapr-sgp-20110520-ppi-classic`, `cfrad1-irene-sr2-20110827-120420-sur-sweeps01`, `cfrad2-radx-irene-sr2-20110827-120420-sur-r30km`, `cfrad2-xradar-xsapr-sgp-20110520-ppi`
- `era:2013` (96): `l2-ktlx-20130520-201643`, `l2-kgwx-20130601-235640`, `l2-ktlx-20130520-201643-trim`, `l3-tlx-daa-20130520-2016`, `l3-tlx-dhr-20130520-2016`, `l3-tlx-dod-20130520-2016`, `l3-tlx-dpa-20130520-2016`, `l3-tlx-dpr-20130520-2016`, `l3-tlx-dsd-20130520-2016`, `l3-tlx-dsp-20130520-2016`, `l3-tlx-dta-20130520-2016`, `l3-tlx-du3-20130520-2008`, `l3-tlx-dvl-20130520-2016`, `l3-tlx-eet-20130520-2016`, `l3-tlx-gsm-20130520-2100`, `l3-tlx-hhc-20130520-2016`, `l3-tlx-n0c-20130520-2016`, `l3-tlx-n0h-20130520-2016`, `l3-tlx-n0k-20130520-2016`, `l3-tlx-n0m-20130520-2016`, `l3-tlx-n0q-20130520-2016`, `l3-tlx-n0r-20130520-2016`, `l3-tlx-n0s-20130520-2016`, `l3-tlx-n0u-20130520-2016`, `l3-tlx-n0v-20130520-2016`, `l3-tlx-n0x-20130520-2016`, `l3-tlx-n0z-20130520-2016`, `l3-tlx-n1c-20130520-2016`, `l3-tlx-n1h-20130520-2016`, `l3-tlx-n1k-20130520-2016`, `l3-tlx-n1m-20130520-2016`, `l3-tlx-n1p-20130520-2016`, `l3-tlx-n1q-20130520-2016`, `l3-tlx-n1s-20130520-2016`, `l3-tlx-n1u-20130520-2016`, `l3-tlx-n1x-20130520-2016`, `l3-tlx-n2c-20130520-2016`, `l3-tlx-n2h-20130520-2016`, `l3-tlx-n2k-20130520-2016`, `l3-tlx-n2m-20130520-2016`, `l3-tlx-n2q-20130520-2016`, `l3-tlx-n2s-20130520-2016`, `l3-tlx-n2u-20130520-2016`, `l3-tlx-n2x-20130520-2016`, `l3-tlx-n3c-20130520-2016`, `l3-tlx-n3h-20130520-2016`, `l3-tlx-n3k-20130520-2016`, `l3-tlx-n3m-20130520-2016`, `l3-tlx-n3p-20130520-2012`, `l3-tlx-n3q-20130520-2016`, `l3-tlx-n3s-20130520-2016`, `l3-tlx-n3u-20130520-2016`, `l3-tlx-n3x-20130520-2016`, `l3-tlx-nac-20130520-2016`, `l3-tlx-nah-20130520-2016`, `l3-tlx-nak-20130520-2016`, `l3-tlx-nam-20130520-2016`, `l3-tlx-naq-20130520-2016`, `l3-tlx-nau-20130520-2016`, `l3-tlx-nax-20130520-2016`, `l3-tlx-nbc-20130520-2016`, `l3-tlx-nbh-20130520-2016`, `l3-tlx-nbk-20130520-2016`, `l3-tlx-nbm-20130520-2016`, `l3-tlx-nbq-20130520-2016`, `l3-tlx-nbu-20130520-2016`, `l3-tlx-nbx-20130520-2016`, `l3-tlx-nc1-20130520-2354`, `l3-tlx-nc2-20130520-2354`, `l3-tlx-nc3-20130520-2354`, `l3-tlx-nc4-20130520-2354`, `l3-tlx-nc5-20130520-2354`, `l3-tlx-nco-20130520-1816`, `l3-tlx-ncr-20130520-2016`, `l3-tlx-ncz-20130520-2016`, `l3-tlx-net-20130520-2016`, `l3-tlx-nhi-20130520-2016`, `l3-tlx-nhl-20130520-2016`, `l3-tlx-nla-20130520-2016`, `l3-tlx-nll-20130520-2016`, `l3-tlx-nmd-20130520-2016`, `l3-tlx-nml-20130520-2016`, `l3-tlx-nsp-20130520-2016`, `l3-tlx-nss-20130520-2016`, `l3-tlx-nst-20130520-2016`, `l3-tlx-nsw-20130520-2016`, `l3-tlx-ntp-20130520-2016`, `l3-tlx-ntv-20130520-2016`, `l3-tlx-nvl-20130520-2012`, `l3-tlx-nvw-20130520-2016`, `l3-tlx-oha-20130520-2016`, `l3-tlx-pta-20130520-2016`, `l3-tlx-rcm-20130520-2016`, `l3-tlx-rsl-20130520-2358`, `l3-tlx-spd-20130520-2016`, `odim-bewid-20130429-0430-pvol-dbzh-scan1`
- `era:2014` (3): `l2-koax-20140616-205305`, `l2-koax-20140616-205305-trim`, `l3-ffc-n0q-20140407-1805`
- `era:2015` (1): `l3-byx-n0q-20150124-2106`
- `era:2016` (21): `l2-kewx-20160413-022531`, `l2-kewx-20160413-022531-trim`, `l3-mci-dhr-20160526-2154`, `l3-mci-dpa-20160526-2154`, `l3-mci-dsp-20160526-2154`, `l3-mci-n1p-20160526-2154`, `l3-mci-ncr-20160526-2154`, `l3-mci-net-20160526-2154`, `l3-mci-nmd-20160526-2154`, `l3-mci-nst-20160526-2154`, `l3-mci-ntp-20160526-2154`, `l3-mci-nvl-20160526-2154`, `l3-mci-nvw-20160526-2154`, `l3-mci-tr0-20160526-2154`, `l3-mci-tr1-20160526-2154`, `l3-mci-tr2-20160526-2154`, `l3-mci-tv0-20160526-2154`, `l3-mci-tv1-20160526-2154`, `l3-mci-tv2-20160526-2154`, `l3-mci-tzl-20160526-2154`, `l3-slc-tv0-20160516-2359`
- `era:2017` (1): `odim-norst-20170421-0908-pvol`
- `era:2018` (4): `dorade-n42rf-ts-20181010-122951-air`, `dorade-n42rf-ts-20181010-122951-air-head24`, `dorade-n42rf-tm-20181010-123925-air`, `dorade-n42rf-tm-20181010-123925-air-head48`
- `era:2019` (5): `odim-bejab-20190606-0000-pvol`, `jma-n5-20191012-090000`, `jma-n6-20191012-090000`, `jma-n5-20191012-090000-rs47773`, `jma-n6-20191012-090000-rs47773`
- `era:2020` (29): `l2-kdvn-20200810-175718`, `l2-kdvn-20200810-180401`, `l2-kdvn-20200810-181043`, `l2-kdvn-20200810-181724`, `l2-kdvn-20200810-180401-trim`, `l3-ddc-gsm-20200817-1000`, `l3-ddc-n0q-20200817-0501`, `l3-ddc-n0q-20200817-0503`, `l3-den-tz0-20200804-2226`, `l3-den-tz1-20200804-2226`, `l3-den-tz2-20200804-2227`, `l3-eax-gsm-20200817-0933`, `l3-eax-n0q-20200817-0401`, `l3-eax-n0q-20200817-0405`, `l3-gjx-n0f-20200817-0551`, `l3-gjx-naf-20200817-0551`, `l3-gjx-nbf-20200817-0551`, `l3-gjx-nxf-20200817-0600`, `l3-lzk-h0c-20200814-0417`, `l3-lzk-h0v-20200812-1309`, `l3-lzk-h0w-20200812-1305`, `l3-lzk-h0z-20200812-1318`, `l3-rax-dta-20200818-0454`, `l3-rax-nyf-20200818-0001`, `l3-tlx-pta-20200501-000023`, `l3-kdvn-20200810-1757-nst`, `l3-kdvn-20200810-1804-nst`, `l3-kdvn-20200810-1810-nst`, `l3-kdvn-20200810-1817-nst`
- `era:2021` (11): `l2-klix-20210829-180425`, `l2-klix-20210829-173117`, `l2-klix-20210829-175748`, `l2-klix-20210829-175748-mdm`, `l2-klix-20210829-180425-trim`, `l3-akc-nc1-20210730-055033`, `l3-jfk-tr0-20210120-154051`, `cfrad1-dow8-20211011-223602-rhi`, `cfrad1-dow8-20211011-223602-rhi-trim3-classic`, `cfrad2-xradar-dow8-20211011-223602-rhi-r300`, `dorade-dow6-20211230-222139-rhi-head41`
- `era:2022` (32): `l2-kbox-20220129-150537`, `l2-tjua-20220918-190621`, `l2-kbox-20220129-150537-trim`, `l3-ftg-n0b-20220304-1820`, `l3-gjx-nyq-20220503-005356`, `l3-okc-net-20220503-005210`, `l3-okc-nhi-20220503-005210`, `l3-okc-ntv-20220503-005210`, `l3-okc-rsl-20220517-085551`, `l3-rax-nll-20220510-155126`, `l3-shv-nzq-20220503-005452`, `l3-tlx-dod-20220503-005231`, `l3-tlx-dsd-20220503-005231`, `l3-tlx-n0f-20220502-235926`, `l3-tlx-n0q-20220503-005231`, `l3-tlx-n0r-20220908-131957`, `l3-tlx-n0u-20220503-005231`, `l3-tlx-n0v-20220908-131957`, `l3-tlx-n0z-20220908-131957`, `l3-tlx-n3p-20220503-011226`, `l3-tlx-ncz-20220503-005231`, `l3-tlx-net-20220503-005231`, `l3-tlx-nhi-20220503-005231`, `l3-tlx-nhl-20220503-005231`, `l3-tlx-nla-20220503-005231`, `l3-tlx-nml-20220503-005231`, `l3-tlx-nss-20220503-005231`, `l3-tlx-nsw-20220503-005231`, `l3-tlx-ntv-20220503-005231`, `l3-tlx-rcm-20220503-004553`, `l3-tlx-rsl-20220502-235926`, `l3-tlx-spd-20220503-005231`
- `era:2023` (7): `l2-kdgx-20230325-010651`, `l2-kmaf-20230331-230843`, `l2-tstl-20230331-230314`, `l2-pgua-20230524-030945`, `l2-tbwi-20230601-175101-stub`, `l2-tstl-20230331-230314-trim`, `l2-pgua-20230524-030945-trim`
- `era:2024` (5): `l2-kmtx-20240301-212827`, `l2-ktlx-20240315-000217`, `l2-ktlx-20240515-000014`, `l2-kmtx-20240301-212827-trim`, `l2-ktlx-20240315-000217-trim`
- `era:2025` (2): `l2-pahg-20250909-212549`, `wmo-text-kslc-20251012-0424-hmlslc`
- `era:2026` (149): `ndswc-kxwa-20260924-214316`, `ndswc-kxwa-20260924-214316-head41`, `polling-ndswc-kxwa-dir-list-20260925`, `polling-iem-config-cfg-20260926`, `polling-ewr-laredo-grlevel2-cfg-20260925`, `l2-kilx-20260418-013553`, `l2-kiwa-20260917-003629`, `l2chunk-kiwa-307-20260917-003629-001-s`, `l2chunk-kiwa-307-20260917-003629-{002..069}-i`, `l2chunk-kiwa-307-20260917-003629-070-e`, `l2chunk-tlas-998-20260917-012843-001-s`, `l2chunk-tlas-999-20260917-013443-001-s`, `l2chunk-tlas-3-20260917-015242-001-s`, `l2chunk-tlas-3-20260917-015242-002-i`, `l2chunk-tlas-3-20260917-015242-003-i`, `l2-kilx-20260418-013553-trim`, `l3-okc-ncr-20260622-080623`, `l3-okc-nmd-20260622-080640`, `l3-okc-nst-20260622-080640`, `l3-okc-nvl-20260622-080623`, `l3-okc-nvw-20260622-080623`, `l3-okc-tv0-20260622-080547`, `l3-okc-tz0-20260622-080547`, `l3-okc-tzl-20260622-080623`, `l3-tlx-daa-20260622-080623`, `l3-tlx-dhr-20260622-080623`, `l3-tlx-dpa-20260629-173638`, `l3-tlx-dpr-20260622-080623`, `l3-tlx-dsp-20260629-173638`, `l3-tlx-dta-20260622-080623`, `l3-tlx-du3-20260622-080623`, `l3-tlx-du6-20260622-120608`, `l3-tlx-dvl-20260622-080623`, `l3-tlx-eet-20260622-080623`, `l3-tlx-hhc-20260622-080623`, `l3-tlx-n0b-20260622-080623`, `l3-tlx-n0c-20260622-080623`, `l3-tlx-n0g-20260622-080623`, `l3-tlx-n0h-20260622-080623`, `l3-tlx-n0k-20260622-080623`, `l3-tlx-n0m-20260622-080623`, `l3-tlx-n0s-20260622-080623`, `l3-tlx-n0x-20260622-080623`, `l3-tlx-n1p-20260629-173638`, `l3-tlx-ncr-20260622-080623`, `l3-tlx-nmd-20260622-080623`, `l3-tlx-nrr-20260622-080623`, `l3-tlx-nst-20260622-080623`, `l3-tlx-ntp-20260629-173638`, `l3-tlx-nvl-20260622-080623`, `l3-tlx-nvw-20260622-080623`, `l3-tlx-oha-20260622-080623`, `odim-espdg-20260707-1927-pvol-dbzh-vradh`, `odim-imgw-ram-20260711-0015-kdp-max`, `odim-imgw-ram-20260711-0015-phidp-max`, `odim-imgw-ram-20260711-0015-rhohv-max`, `odim-imgw-ram-20260711-0015-zdr-max`, `odim-iesha-20260305-0115-pvol`, `odim-dkrom-20260820-1130-pvol`, `odim-dkrom-20260820-1130-pvol-h5latest-trim`, `odim-dkrom-20260820-1130-pvol-h5edge-paged-ea`, `odim-dkrom-20260820-1130-pvol-h5edge-len4`, `odim-au24-20260610-000300-nci-zip-member`, `odim-seang-20260924-2130-qcvol-dataset1-trim`, `odim-fianj-20260924-2130-pvol-dataset1-trim`, `odim-deboo-20260924-2130-sweep-th-00`, `odim-itdes-20260924-2135-pvol-class`, `cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32`, `dorade-cow2-20260521-225514-sur-head24`, `jma-n5-20260924-210000`, `jma-n6-20260924-210000`, `jma-n5-20260924-210000-rs47937`, `jma-n6-20260924-210000-rs47937`, `odim-bejab-20260612-1450-dbzh`, `odim-bejab-20260612-1450-vrad`, `odim-nohur-20260612-1445-dbzh`, `odim-nohur-20260612-1445-th`, `odim-nohur-20260612-1446-vradh`, `odim-au02-20260921-0000-pvol-subset`, `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-per-ray-geometry`, `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-finest-geometry-netcdf4`, `l3-ktlx-20260622-080806-n0b-sails`

#### `file:`

- `file:mdm` (1): `l2-klix-20210829-175748-mdm`

#### `first-gate:`

- `first-gate:125m` (1): `l2-kgwx-20130601-235640`

#### `framing:`

- `framing:ccb` (14): `l3-mci-dpa-20160526-2154`, `l3-mci-dsp-20160526-2154`, `l3-mci-n1p-20160526-2154`, `l3-mci-ncr-20160526-2154`, `l3-mci-net-20160526-2154`, `l3-mci-nmd-20160526-2154`, `l3-mci-nst-20160526-2154`, `l3-mci-ntp-20160526-2154`, `l3-mci-nvl-20160526-2154`, `l3-mci-nvw-20160526-2154`, `l3-mci-tr0-20160526-2154`, `l3-mci-tr1-20160526-2154`, `l3-mci-tr2-20160526-2154`, `l3-tlx-pta-20200501-000023`
- `framing:noaaport` (35): `l3-ddc-n0q-20200817-0501`, `l3-ddc-n0q-20200817-0503`, `l3-den-tz0-20200804-2226`, `l3-den-tz1-20200804-2226`, `l3-den-tz2-20200804-2227`, `l3-eax-n0q-20200817-0401`, `l3-eax-n0q-20200817-0405`, `l3-ffc-n0q-20140407-1805`, `l3-ftg-n0b-20220304-1820`, `l3-gjx-n0f-20200817-0551`, `l3-gjx-naf-20200817-0551`, `l3-gjx-nbf-20200817-0551`, `l3-gjx-nxf-20200817-0600`, `l3-mci-dhr-20160526-2154`, `l3-mci-dpa-20160526-2154`, `l3-mci-dsp-20160526-2154`, `l3-mci-n1p-20160526-2154`, `l3-mci-ncr-20160526-2154`, `l3-mci-net-20160526-2154`, `l3-mci-nmd-20160526-2154`, `l3-mci-nst-20160526-2154`, `l3-mci-ntp-20160526-2154`, `l3-mci-nvl-20160526-2154`, `l3-mci-nvw-20160526-2154`, `l3-mci-tr0-20160526-2154`, `l3-mci-tr1-20160526-2154`, `l3-mci-tr2-20160526-2154`, `l3-mci-tv0-20160526-2154`, `l3-mci-tv1-20160526-2154`, `l3-mci-tv2-20160526-2154`, `l3-mci-tzl-20160526-2154`, `l3-rax-dta-20200818-0454`, `l3-rax-nyf-20200818-0001`, `l3-slc-tv0-20160516-2359`, `l3-tlx-pta-20200501-000023`
- `framing:text-only` (3): `l3-abr-ftm-20110428-1331`, `l3-knqa-20080205-0018-rob`, `wmo-text-kslc-20251012-0424-hmlslc`
- `framing:zlib` (14): `l3-mci-dpa-20160526-2154`, `l3-mci-dsp-20160526-2154`, `l3-mci-n1p-20160526-2154`, `l3-mci-ncr-20160526-2154`, `l3-mci-net-20160526-2154`, `l3-mci-nmd-20160526-2154`, `l3-mci-nst-20160526-2154`, `l3-mci-ntp-20160526-2154`, `l3-mci-nvl-20160526-2154`, `l3-mci-nvw-20160526-2154`, `l3-mci-tr0-20160526-2154`, `l3-mci-tr1-20160526-2154`, `l3-mci-tr2-20160526-2154`, `l3-tlx-pta-20200501-000023`

#### `fuzz-finding:`

- `fuzz-finding:crash` (13): `fuzz-odim-hdf5-local-heap-name-offset-overflow`, `fuzz-hdf5-chunk-offset-overflow`, `fuzz-level2-writer-nexrad-moment-nan-scale`, `fuzz-writers-l2-sweep-without-gates`, `fuzz-writers-l2-one-gate-sweep`, `fuzz-writers-l2-odim-rstart-beyond-20-km`, `fuzz-writers-dorade-ray-without-time`, `fuzz-writers-l2-empty-field-name`, `fuzz-writers-odim-gate-spacing-below-float`, `fuzz-writers-dorade-absent-rows-without-fill`, `fuzz-writers-cfradial1-ray-time-near-float-max`, `fuzz-level3-rcm-centroid-non-ascii`, `fuzz-level2-records-volume-header-date-overflow`
- `fuzz-finding:oom` (1): `fuzz-cfradial-overlapping-sweep-ray-ranges`

#### `fuzz-target:`

- `fuzz-target:cfradial` (1): `fuzz-cfradial-overlapping-sweep-ray-ranges`
- `fuzz-target:cli-open` (1): `fuzz-level2-records-volume-header-date-overflow`
- `fuzz-target:hdf5` (1): `fuzz-hdf5-chunk-offset-overflow`
- `fuzz-target:level2-records` (1): `fuzz-level2-records-volume-header-date-overflow`
- `fuzz-target:level2-writer` (1): `fuzz-level2-writer-nexrad-moment-nan-scale`
- `fuzz-target:level3` (1): `fuzz-level3-rcm-centroid-non-ascii`
- `fuzz-target:odim` (1): `fuzz-odim-hdf5-local-heap-name-offset-overflow`
- `fuzz-target:writers` (8): `fuzz-writers-l2-sweep-without-gates`, `fuzz-writers-l2-one-gate-sweep`, `fuzz-writers-l2-odim-rstart-beyond-20-km`, `fuzz-writers-dorade-ray-without-time`, `fuzz-writers-l2-empty-field-name`, `fuzz-writers-odim-gate-spacing-below-float`, `fuzz-writers-dorade-absent-rows-without-fill`, `fuzz-writers-cfradial1-ray-time-near-float-max`

#### `generator:`

- `generator:pytmatrix-0.3.3` (17): `tmatrix-lut-rain-sband-pytmatrix-0.3.3`, `tmatrix-lut-rain-sband-pytmatrix-0.3.3-config`, `tmatrix-lut-rain-sband-pytmatrix-0.3.3-manifest`, `tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3`, `tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3-config`, `tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3-manifest`, `tmatrix-held-out-interpolation-report-v10`, `tmatrix-held-out-nodes-v10`, `tmatrix-lut-property-dry-oblate-sband-trim`, `tmatrix-lut-property-dry-oblate-sband-trim-config`, `tmatrix-lut-property-dry-oblate-sband-trim-trim`, `tmatrix-lut-property-wet-oblate-sband-trim`, `tmatrix-lut-property-wet-oblate-sband-trim-config`, `tmatrix-lut-property-wet-oblate-sband-trim-trim`, `tmatrix-lut-property-rain-sband-trim`, `tmatrix-lut-property-rain-sband-trim-config`, `tmatrix-lut-property-rain-sband-trim-trim`

#### `hdf5:`

- `hdf5:committed-datatype` (2): `odim-dkrom-20260820-1130-pvol-h5edge-paged-ea`, `odim-dkrom-20260820-1130-pvol-h5edge-len4`
- `hdf5:compound-char-array` (1): `odim-itdes-20260924-2135-pvol-class`
- `hdf5:compound-vlen-string` (1): `odim-fianj-20260924-2130-pvol-dataset1-trim`
- `hdf5:creation-order-index` (1): `odim-dkrom-20260820-1130-pvol-h5latest-trim`
- `hdf5:dense-attributes` (3): `odim-dkrom-20260820-1130-pvol-h5latest-trim`, `odim-dkrom-20260820-1130-pvol-h5edge-paged-ea`, `odim-dkrom-20260820-1130-pvol-h5edge-len4`
- `hdf5:dense-links` (2): `odim-dkrom-20260820-1130-pvol-h5latest-trim`, `odim-dkrom-20260820-1130-pvol-h5edge-len4`
- `hdf5:extensible-array` (2): `odim-dkrom-20260820-1130-pvol-h5latest-trim`, `odim-dkrom-20260820-1130-pvol-h5edge-paged-ea`
- `hdf5:filtered-fractal-heap` (1): `odim-dkrom-20260820-1130-pvol-h5edge-len4`
- `hdf5:fixed-array` (1): `odim-dkrom-20260820-1130-pvol-h5latest-trim`
- `hdf5:fletcher32` (1): `odim-dkrom-20260820-1130-pvol-h5latest-trim`
- `hdf5:gzip-chunked` (15): `odim-bejab-20190606-0000-pvol`, `odim-bewid-20130429-0430-pvol-dbzh-scan1`, `odim-norst-20170421-0908-pvol`, `odim-espdg-20260707-1927-pvol-dbzh-vradh`, `odim-iesha-20260305-0115-pvol`, `odim-dkrom-20260820-1130-pvol`, `odim-seang-20260924-2130-qcvol-dataset1-trim`, `odim-fianj-20260924-2130-pvol-dataset1-trim`, `odim-deboo-20260924-2130-sweep-th-00`, `odim-bejab-20260612-1450-dbzh`, `odim-bejab-20260612-1450-vrad`, `odim-nohur-20260612-1445-dbzh`, `odim-nohur-20260612-1445-th`, `odim-nohur-20260612-1446-vradh`, `odim-au02-20260921-0000-pvol-subset`
- `hdf5:huge-heap-objects` (1): `odim-dkrom-20260820-1130-pvol-h5latest-trim`
- `hdf5:implicit-chunks` (1): `odim-dkrom-20260820-1130-pvol-h5latest-trim`
- `hdf5:length-size-4` (1): `odim-dkrom-20260820-1130-pvol-h5edge-len4`
- `hdf5:local-heap` (1): `fuzz-odim-hdf5-local-heap-name-offset-overflow`
- `hdf5:lzf` (1): `cfrad1-xsapr-sgp-20110520-ppi-netcdf4-szip-lzf`
- `hdf5:offset-size-4` (2): `odim-dkrom-20260820-1130-pvol-h5edge-paged-ea`, `odim-dkrom-20260820-1130-pvol-h5edge-len4`
- `hdf5:paged-extensible-array` (1): `odim-dkrom-20260820-1130-pvol-h5edge-paged-ea`
- `hdf5:single-chunk` (1): `odim-dkrom-20260820-1130-pvol-h5latest-trim`
- `hdf5:superblock-v0` (17): `fuzz-odim-hdf5-local-heap-name-offset-overflow`, `fuzz-writers-odim-gate-spacing-below-float`, `odim-bejab-20190606-0000-pvol`, `odim-bewid-20130429-0430-pvol-dbzh-scan1`, `odim-espdg-20260707-1927-pvol-dbzh-vradh`, `odim-iesha-20260305-0115-pvol`, `odim-dkrom-20260820-1130-pvol`, `odim-seang-20260924-2130-qcvol-dataset1-trim`, `odim-fianj-20260924-2130-pvol-dataset1-trim`, `odim-deboo-20260924-2130-sweep-th-00`, `odim-itdes-20260924-2135-pvol-class`, `odim-bejab-20260612-1450-dbzh`, `odim-bejab-20260612-1450-vrad`, `odim-nohur-20260612-1445-dbzh`, `odim-nohur-20260612-1445-th`, `odim-nohur-20260612-1446-vradh`, `odim-au02-20260921-0000-pvol-subset`
- `hdf5:superblock-v1` (1): `odim-norst-20170421-0908-pvol`
- `hdf5:superblock-v3` (4): `fuzz-hdf5-chunk-offset-overflow`, `odim-dkrom-20260820-1130-pvol-h5latest-trim`, `odim-dkrom-20260820-1130-pvol-h5edge-paged-ea`, `odim-dkrom-20260820-1130-pvol-h5edge-len4`
- `hdf5:szip` (1): `cfrad1-xsapr-sgp-20110520-ppi-netcdf4-szip-lzf`
- `hdf5:user-block` (1): `odim-dkrom-20260820-1130-pvol-h5latest-trim`
- `hdf5:v2-btree-chunks` (2): `fuzz-hdf5-chunk-offset-overflow`, `odim-dkrom-20260820-1130-pvol-h5latest-trim`
- `hdf5:v2-object-headers` (2): `odim-espdg-20260707-1927-pvol-dbzh-vradh`, `odim-dkrom-20260820-1130-pvol-h5latest-trim`
- `hdf5:vlen-strings` (3): `odim-bewid-20130429-0430-pvol-dbzh-scan1`, `odim-dkrom-20260820-1130-pvol-h5edge-paged-ea`, `odim-dkrom-20260820-1130-pvol-h5edge-len4`

#### `header:`

- `header:AR2V0001` (3): `l2-klix-20050829-130035`, `l2-kvwx-20080415-235337`, `l2-klix-20050829-130035-trim`
- `header:AR2V0003` (2): `l2-kdmx-20080525-205148`, `l2-kdmx-20080525-205148-trim`
- `header:AR2V0004` (1): `l2-kpah-20080415-235014`
- `header:AR2V0006` (32): `l2-kvnx-20110315-000203`, `l2-ktlx-20130520-201643`, `l2-koax-20140616-205305`, `l2-kewx-20160413-022531`, `l2-kdvn-20200810-175718`, `l2-kdvn-20200810-180401`, `l2-kdvn-20200810-181043`, `l2-kdvn-20200810-181724`, `l2-klix-20210829-180425`, `l2-klix-20210829-173117`, `l2-klix-20210829-175748`, `l2-kbox-20220129-150537`, `l2-tjua-20220918-190621`, `l2-kdgx-20230325-010651`, `l2-kmaf-20230331-230843`, `l2-pgua-20230524-030945`, `l2-kmtx-20240301-212827`, `l2-ktlx-20240315-000217`, `l2-ktlx-20240515-000014`, `l2-pahg-20250909-212549`, `l2-kilx-20260418-013553`, `l2-kiwa-20260917-003629`, `l2-ktlx-20130520-201643-trim`, `l2-koax-20140616-205305-trim`, `l2-kewx-20160413-022531-trim`, `l2-kdvn-20200810-180401-trim`, `l2-klix-20210829-180425-trim`, `l2-kbox-20220129-150537-trim`, `l2-pgua-20230524-030945-trim`, `l2-kmtx-20240301-212827-trim`, `l2-ktlx-20240315-000217-trim`, `l2-kilx-20260418-013553-trim`
- `header:AR2V0007` (1): `l2-kgwx-20130601-235640`
- `header:AR2V0008` (4): `fuzz-level2-records-volume-header-date-overflow`, `l2-tstl-20230331-230314`, `l2-tbwi-20230601-175101-stub`, `l2-tstl-20230331-230314-trim`
- `header:ARCHIVE2` (7): `l2-ktlx-19910605-162126`, `l2-ktlx-19990503-230052`, `l2-ktlx-19990504-002218`, `l2-ktlx-20030508-221041`, `l2-ktlx-19910605-162126-trim`, `l2-ktlx-19990504-002218-trim`, `l2-ktlx-20030508-221041-trim`

#### `icao:`

- `icao:blank` (2): `l2-ktlx-19910605-162126`, `l2-ktlx-19910605-162126-trim`
- `icao:nul` (5): `l2-ktlx-19990503-230052`, `l2-ktlx-19990504-002218`, `l2-ktlx-20030508-221041`, `l2-ktlx-19990504-002218-trim`, `l2-ktlx-20030508-221041-trim`

#### `known-failure:`

- `known-failure:jma-lowest-level-valid` (1): `jma-n5-20191012-090000-rs47773`
- `known-failure:level2-ldm-block-limit` (1): `ndswc-kxwa-20260924-214316`

#### `layout:`

- `layout:ldm-one-radial-per-record` (2): `ndswc-kxwa-20260924-214316`, `ndswc-kxwa-20260924-214316-head41`

#### `license:`

- `license:BSD-3-Clause` (5): `cfrad1-xsapr-sgp-20110520-ppi-netcdf4`, `cfrad1-xsapr-sgp-20110520-ppi-netcdf4-user-types`, `cfrad1-xsapr-sgp-20110520-ppi-netcdf4-szip-lzf`, `cfrad1-xsapr-sgp-20110520-ppi-classic`, `cfrad2-xradar-xsapr-sgp-20110520-ppi`
- `license:CC-BY-4.0` (31): `odim-espdg-20260707-1927-pvol-dbzh-vradh`, `odim-iesha-20260305-0115-pvol`, `odim-dkrom-20260820-1130-pvol`, `odim-dkrom-20260820-1130-pvol-h5latest-trim`, `odim-dkrom-20260820-1130-pvol-h5edge-paged-ea`, `odim-dkrom-20260820-1130-pvol-h5edge-len4`, `odim-au24-20260610-000300-nci-zip-member`, `odim-seang-20260924-2130-qcvol-dataset1-trim`, `odim-fianj-20260924-2130-pvol-dataset1-trim`, `odim-deboo-20260924-2130-sweep-th-00`, `cfrad1-irene-sr2-20110827-120420-sur-sweeps01`, `cfrad2-radx-irene-sr2-20110827-120420-sur-r30km`, `cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32`, `dorade-noxp-20090501-sweeps-tgz`, `dorade-noxp-20090525-sweeps-tgz`, `dorade-noxp-20090501-190244-ppi`, `dorade-noxp-20090501-190324-ppi`, `dorade-noxp-20090525-203211-sector`, `dorade-dow6-20211230-222139-rhi-head41`, `dorade-noxp-20090610-003210-ppi-head6`, `dorade-noxp-20090610-003222-ppi-head6`, `dorade-noxp-20090610-003226-ppi-head6`, `dorade-noxp-20090610-003210-heads-zip`, `odim-bejab-20260612-1450-dbzh`, `odim-bejab-20260612-1450-vrad`, `odim-nohur-20260612-1445-dbzh`, `odim-nohur-20260612-1445-th`, `odim-nohur-20260612-1446-vradh`, `odim-au02-20260921-0000-pvol-subset`, `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-per-ray-geometry`, `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-finest-geometry-netcdf4`
- `license:MIT` (8): `odim-bejab-20190606-0000-pvol`, `odim-bewid-20130429-0430-pvol-dbzh-scan1`, `odim-norst-20170421-0908-pvol`, `cfrad1-dow8-20211011-223602-rhi`, `cfrad1-dow8-20211011-223602-rhi-trim3-classic`, `cfrad1-spol-20080604-002217-sur`, `cfrad2-spol-20080604-002217-sur`, `cfrad2-xradar-dow8-20211011-223602-rhi-r300`
- `license:imgw-attribution` (4): `odim-imgw-ram-20260711-0015-kdp-max`, `odim-imgw-ram-20260711-0015-phidp-max`, `odim-imgw-ram-20260711-0015-rhohv-max`, `odim-imgw-ram-20260711-0015-zdr-max`
- `license:public-domain` (10): `l3-kbmx-19980416-archive-tarz`, `l3-kbmx-19980416-0006-nvw`, `l3-kdvn-20200810-1757-nst`, `l3-kdvn-20200810-1804-nst`, `l3-kdvn-20200810-1810-nst`, `l3-kdvn-20200810-1817-nst`, `l3-ktlx-20260622-080806-n0b-sails`, `l3-knqa-20080205-archive-tarz`, `l3-knqa-20080205-0018-rob`, `wmo-text-kslc-20251012-0424-hmlslc`
- `license:unknown` (19): `ndswc-kxwa-20260924-214316`, `ndswc-kxwa-20260924-214316-head41`, `polling-ndswc-kxwa-dir-list-20260925`, `polling-iem-config-cfg-20260926`, `polling-ewr-laredo-grlevel2-cfg-20260925`, `odim-itdes-20260924-2135-pvol-class`, `dorade-cow2-20260521-225514-sur-head24`, `dorade-n42rf-ts-20181010-122951-air`, `dorade-n42rf-ts-20181010-122951-air-head24`, `dorade-n42rf-tm-20181010-123925-air`, `dorade-n42rf-tm-20181010-123925-air-head48`, `jma-n5-20191012-090000`, `jma-n6-20191012-090000`, `jma-n5-20191012-090000-rs47773`, `jma-n6-20191012-090000-rs47773`, `jma-n5-20260924-210000`, `jma-n6-20260924-210000`, `jma-n5-20260924-210000-rs47937`, `jma-n6-20260924-210000-rs47937`
- `license:wrf-public-domain` (4): `wrf-p3-lookup-table-1-v5.4-2momI`, `wrf-p3-lookup-table-1-v5.4-3momI`, `wrf-p3-lookup-table-1-v5.4-2momI-first-block`, `wrf-p3-lookup-table-1-v5.4-3momI-first-block`

#### `lut:`

- `lut:generator-config` (5): `tmatrix-lut-rain-sband-pytmatrix-0.3.3-config`, `tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3-config`, `tmatrix-lut-property-dry-oblate-sband-trim-config`, `tmatrix-lut-property-wet-oblate-sband-trim-config`, `tmatrix-lut-property-rain-sband-trim-config`
- `lut:generator-manifest` (5): `tmatrix-lut-rain-sband-pytmatrix-0.3.3-manifest`, `tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3-manifest`, `tmatrix-lut-property-dry-oblate-sband-trim-trim`, `tmatrix-lut-property-wet-oblate-sband-trim-trim`, `tmatrix-lut-property-rain-sband-trim-trim`
- `lut:schema-1` (5): `tmatrix-lut-rain-sband-pytmatrix-0.3.3`, `tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3`, `tmatrix-lut-property-dry-oblate-sband-trim`, `tmatrix-lut-property-wet-oblate-sband-trim`, `tmatrix-lut-property-rain-sband-trim`

#### `meso-sails:`

- `meso-sails:2` (6): `l2-kdvn-20200810-175718`, `l2-kdvn-20200810-180401`, `l2-kdvn-20200810-181043`, `l2-kdvn-20200810-181724`, `l2-kdgx-20230325-010651`, `l2-kdvn-20200810-180401-trim`
- `meso-sails:3` (4): `l2-pgua-20230524-030945`, `l2-ktlx-20240315-000217`, `l2-pgua-20230524-030945-trim`, `l2-ktlx-20240315-000217-trim`

#### `message:`

- `message:gsm` (3): `l3-ddc-gsm-20200817-1000`, `l3-eax-gsm-20200817-0933`, `l3-tlx-gsm-20130520-2100`

#### `mnemonic:`

- `mnemonic:APR` (2): `l3-tlx-nla-20130520-2016`, `l3-tlx-nla-20220503-005231`
- `mnemonic:ASP` (3): `l3-okc-rsl-20220517-085551`, `l3-tlx-rsl-20130520-2358`, `l3-tlx-rsl-20220502-235926`
- `mnemonic:CR` (12): `l3-fws-ncz-19950517-2304`, `l3-ilx-ncz-19960419-2320`, `l3-ind-035-19941031-2138`, `l3-lzk-ncz-19970301-1912`, `l3-mci-ncr-20160526-2154`, `l3-okc-ncr-20260622-080623`, `l3-tlx-nco-20130520-1816`, `l3-tlx-ncr-20130520-2016`, `l3-tlx-ncr-20260622-080623`, `l3-tlx-ncz-19990503-2316`, `l3-tlx-ncz-20130520-2016`, `l3-tlx-ncz-20220503-005231`
- `mnemonic:CS` (1): `l3-tlx-087-19940308-1939`
- `mnemonic:DAA` (2): `l3-tlx-daa-20130520-2016`, `l3-tlx-daa-20260622-080623`
- `mnemonic:DCC` (7): `l3-tlx-n0c-20130520-2016`, `l3-tlx-n0c-20260622-080623`, `l3-tlx-n1c-20130520-2016`, `l3-tlx-n2c-20130520-2016`, `l3-tlx-n3c-20130520-2016`, `l3-tlx-nac-20130520-2016`, `l3-tlx-nbc-20130520-2016`
- `mnemonic:DHC` (7): `l3-tlx-n0h-20130520-2016`, `l3-tlx-n0h-20260622-080623`, `l3-tlx-n1h-20130520-2016`, `l3-tlx-n2h-20130520-2016`, `l3-tlx-n3h-20130520-2016`, `l3-tlx-nah-20130520-2016`, `l3-tlx-nbh-20130520-2016`
- `mnemonic:DHR` (3): `l3-mci-dhr-20160526-2154`, `l3-tlx-dhr-20130520-2016`, `l3-tlx-dhr-20260622-080623`
- `mnemonic:DKD` (7): `l3-tlx-n0k-20130520-2016`, `l3-tlx-n0k-20260622-080623`, `l3-tlx-n1k-20130520-2016`, `l3-tlx-n2k-20130520-2016`, `l3-tlx-n3k-20130520-2016`, `l3-tlx-nak-20130520-2016`, `l3-tlx-nbk-20130520-2016`
- `mnemonic:DOD` (2): `l3-tlx-dod-20130520-2016`, `l3-tlx-dod-20220503-005231`
- `mnemonic:DPA` (4): `l3-fws-dpa-19950517-2304`, `l3-mci-dpa-20160526-2154`, `l3-tlx-dpa-20130520-2016`, `l3-tlx-dpa-20260629-173638`
- `mnemonic:DPR` (2): `l3-tlx-dpr-20130520-2016`, `l3-tlx-dpr-20260622-080623`
- `mnemonic:DR` (21): `l3-byx-n0q-20150124-2106`, `l3-ddc-n0q-20200817-0501`, `l3-ddc-n0q-20200817-0503`, `l3-den-tz0-20200804-2226`, `l3-den-tz1-20200804-2226`, `l3-den-tz2-20200804-2227`, `l3-eax-n0q-20200817-0401`, `l3-eax-n0q-20200817-0405`, `l3-ffc-n0q-20140407-1805`, `l3-gjx-nyq-20220503-005356`, `l3-mci-tzl-20160526-2154`, `l3-okc-tz0-20260622-080547`, `l3-okc-tzl-20260622-080623`, `l3-shv-nzq-20220503-005452`, `l3-tlx-n0q-20130520-2016`, `l3-tlx-n0q-20220503-005231`, `l3-tlx-n1q-20130520-2016`, `l3-tlx-n2q-20130520-2016`, `l3-tlx-n3q-20130520-2016`, `l3-tlx-naq-20130520-2016`, `l3-tlx-nbq-20130520-2016`
- `mnemonic:DSA` (3): `l3-rax-dta-20200818-0454`, `l3-tlx-dta-20130520-2016`, `l3-tlx-dta-20260622-080623`
- `mnemonic:DSD` (2): `l3-tlx-dsd-20130520-2016`, `l3-tlx-dsd-20220503-005231`
- `mnemonic:DSP` (3): `l3-mci-dsp-20160526-2154`, `l3-tlx-dsp-20130520-2016`, `l3-tlx-dsp-20260629-173638`
- `mnemonic:DUA` (3): `l3-tlx-du3-20130520-2008`, `l3-tlx-du3-20260622-080623`, `l3-tlx-du6-20260622-120608`
- `mnemonic:DV` (12): `l3-mci-tv0-20160526-2154`, `l3-mci-tv1-20160526-2154`, `l3-mci-tv2-20160526-2154`, `l3-okc-tv0-20260622-080547`, `l3-slc-tv0-20160516-2359`, `l3-tlx-n0u-20130520-2016`, `l3-tlx-n0u-20220503-005231`, `l3-tlx-n1u-20130520-2016`, `l3-tlx-n2u-20130520-2016`, `l3-tlx-n3u-20130520-2016`, `l3-tlx-nau-20130520-2016`, `l3-tlx-nbu-20130520-2016`
- `mnemonic:DVL` (2): `l3-tlx-dvl-20130520-2016`, `l3-tlx-dvl-20260622-080623`
- `mnemonic:DZD` (7): `l3-tlx-n0x-20130520-2016`, `l3-tlx-n0x-20260622-080623`, `l3-tlx-n1x-20130520-2016`, `l3-tlx-n2x-20130520-2016`, `l3-tlx-n3x-20130520-2016`, `l3-tlx-nax-20130520-2016`, `l3-tlx-nbx-20130520-2016`
- `mnemonic:EET` (2): `l3-tlx-eet-20130520-2016`, `l3-tlx-eet-20260622-080623`
- `mnemonic:ET` (4): `l3-mci-net-20160526-2154`, `l3-okc-net-20220503-005210`, `l3-tlx-net-20130520-2016`, `l3-tlx-net-20220503-005231`
- `mnemonic:FTM` (1): `l3-abr-ftm-20110428-1331`
- `mnemonic:HHC` (2): `l3-tlx-hhc-20130520-2016`, `l3-tlx-hhc-20260622-080623`
- `mnemonic:HI` (5): `l3-fws-nhi-19950517-1323`, `l3-ilx-nhi-19960419-2303`, `l3-okc-nhi-20220503-005210`, `l3-tlx-nhi-20130520-2016`, `l3-tlx-nhi-20220503-005231`
- `mnemonic:IRM` (3): `l3-ilx-irm-19960419-2309`, `l3-lot-irm-19941031-0503`, `l3-tlx-irm-19940308-1115`
- `mnemonic:LRA` (2): `l3-tlx-063-19940308-1930`, `l3-tlx-064-19940308-1930`
- `mnemonic:LRM` (6): `l3-rax-nll-20220510-155126`, `l3-tlx-nhl-20130520-2016`, `l3-tlx-nhl-20220503-005231`, `l3-tlx-nll-20130520-2016`, `l3-tlx-nml-20130520-2016`, `l3-tlx-nml-20220503-005231`
- `mnemonic:M` (4): `l3-fws-nme-19950517-2316`, `l3-ilx-nme-19960419-2303`, `l3-lzk-nme-19970301-2027`, `l3-sgf-nme-20030504-2332`
- `mnemonic:MD` (4): `l3-mci-nmd-20160526-2154`, `l3-okc-nmd-20260622-080640`, `l3-tlx-nmd-20130520-2016`, `l3-tlx-nmd-20260622-080623`
- `mnemonic:ML` (7): `l3-tlx-n0m-20130520-2016`, `l3-tlx-n0m-20260622-080623`, `l3-tlx-n1m-20130520-2016`, `l3-tlx-n2m-20130520-2016`, `l3-tlx-n3m-20130520-2016`, `l3-tlx-nam-20130520-2016`, `l3-tlx-nbm-20130520-2016`
- `mnemonic:OHA` (2): `l3-tlx-oha-20130520-2016`, `l3-tlx-oha-20260622-080623`
- `mnemonic:OHP` (4): `l3-fws-n1p-19950517-2304`, `l3-mci-n1p-20160526-2154`, `l3-tlx-n1p-20130520-2016`, `l3-tlx-n1p-20260629-173638`
- `mnemonic:PRC` (6): `l3-gjx-n0f-20200817-0551`, `l3-gjx-naf-20200817-0551`, `l3-gjx-nbf-20200817-0551`, `l3-gjx-nxf-20200817-0600`, `l3-rax-nyf-20200818-0001`, `l3-tlx-n0f-20220502-235926`
- `mnemonic:R` (11): `l3-akq-018-19940810-0835`, `l3-cae-n0r-19940629-1906`, `l3-cys-021-19941114-1107`, `l3-fws-n0r-19950517-2304`, `l3-ind-016-19940910-0647`, `l3-ind-017-19940910-1104`, `l3-lot-n0r-19941106-0246`, `l3-tlx-n0r-20130520-2016`, `l3-tlx-n0r-20220908-131957`, `l3-tlx-n0z-20130520-2016`, `l3-tlx-n0z-20220908-131957`
- `mnemonic:RCM` (4): `l3-fws-rcm-19950517-2310`, `l3-ilx-rcm-19960419-2309`, `l3-tlx-rcm-20130520-2016`, `l3-tlx-rcm-20220503-004553`
- `mnemonic:RCS` (1): `l3-lot-050-19941031-1358`
- `mnemonic:RRC` (1): `l3-tlx-nrr-20260622-080623`
- `mnemonic:SDC` (1): `l3-lzk-h0c-20200814-0417`
- `mnemonic:SDR` (3): `l3-ftg-n0b-20220304-1820`, `l3-lzk-h0z-20200812-1318`, `l3-tlx-n0b-20260622-080623`
- `mnemonic:SDV` (2): `l3-lzk-h0v-20200812-1309`, `l3-tlx-n0g-20260622-080623`
- `mnemonic:SDW` (1): `l3-lzk-h0w-20200812-1305`
- `mnemonic:SPD` (3): `l3-fws-sup-19950517-2304`, `l3-tlx-spd-20130520-2016`, `l3-tlx-spd-20220503-005231`
- `mnemonic:SRM` (5): `l3-tlx-n0s-20130520-2016`, `l3-tlx-n0s-20260622-080623`, `l3-tlx-n1s-20130520-2016`, `l3-tlx-n2s-20130520-2016`, `l3-tlx-n3s-20130520-2016`
- `mnemonic:SRR` (1): `l3-lot-055-19941031-1137`
- `mnemonic:SS` (3): `l3-fws-nss-19950517-2304`, `l3-tlx-nss-20130520-2016`, `l3-tlx-nss-20220503-005231`
- `mnemonic:STA` (2): `l3-tlx-pta-20130520-2016`, `l3-tlx-pta-20200501-000023`
- `mnemonic:STI` (6): `l3-fws-nst-19950517-2304`, `l3-ilx-nst-19960419-2303`, `l3-mci-nst-20160526-2154`, `l3-okc-nst-20260622-080640`, `l3-tlx-nst-20130520-2016`, `l3-tlx-nst-20260622-080623`
- `mnemonic:STP` (4): `l3-fws-ntp-19950517-2304`, `l3-mci-ntp-20160526-2154`, `l3-tlx-ntp-20130520-2016`, `l3-tlx-ntp-20260629-173638`
- `mnemonic:SW` (4): `l3-bmx-029-19940914-1621`, `l3-tlx-nsp-20130520-2016`, `l3-tlx-nsw-20130520-2016`, `l3-tlx-nsw-20220503-005231`
- `mnemonic:THP` (2): `l3-tlx-n3p-20130520-2012`, `l3-tlx-n3p-20220503-011226`
- `mnemonic:TVS` (6): `l3-ilx-ntv-19960419-2303`, `l3-lzk-ntv-19970301-2027`, `l3-okc-ntv-20220503-005210`, `l3-sgf-ntv-20030504-2352`, `l3-tlx-ntv-20130520-2016`, `l3-tlx-ntv-20220503-005231`
- `mnemonic:UAM` (1): `l3-ind-073-19940910-1555`
- `mnemonic:V` (6): `l3-ftg-022-19940601-1917`, `l3-ftg-026-19940930-0536`, `l3-fws-now-19950517-2304`, `l3-jan-024-19951003-1312`, `l3-tlx-n0v-20130520-2016`, `l3-tlx-n0v-20220908-131957`
- `mnemonic:VAD` (1): `l3-lot-084-19931120-0721`
- `mnemonic:VCS` (1): `l3-mlb-051-19941116-0335`
- `mnemonic:VIL` (4): `l3-mci-nvl-20160526-2154`, `l3-okc-nvl-20260622-080623`, `l3-tlx-nvl-20130520-2012`, `l3-tlx-nvl-20260622-080623`
- `mnemonic:VWP` (6): `l3-fws-nvw-19950517-2322`, `l3-lot-nvw-19931120-0721`, `l3-mci-nvw-20160526-2154`, `l3-okc-nvw-20260622-080623`, `l3-tlx-nvw-20130520-2016`, `l3-tlx-nvw-20260622-080623`

#### `moment:`

- `moment:CFP` (20): `l2-klix-20210829-180425`, `l2-klix-20210829-173117`, `l2-klix-20210829-175748`, `l2-kbox-20220129-150537`, `l2-tjua-20220918-190621`, `l2-kdgx-20230325-010651`, `l2-kmaf-20230331-230843`, `l2-pgua-20230524-030945`, `l2-kmtx-20240301-212827`, `l2-ktlx-20240315-000217`, `l2-ktlx-20240515-000014`, `l2-pahg-20250909-212549`, `l2-kilx-20260418-013553`, `l2-kiwa-20260917-003629`, `l2-klix-20210829-180425-trim`, `l2-kbox-20220129-150537-trim`, `l2-pgua-20230524-030945-trim`, `l2-kmtx-20240301-212827-trim`, `l2-ktlx-20240315-000217-trim`, `l2-kilx-20260418-013553-trim`

#### `moments:`

- `moments:class` (1): `odim-itdes-20260924-2135-pvol-class`
- `moments:dbzh` (6): `odim-bejab-20190606-0000-pvol`, `odim-bewid-20130429-0430-pvol-dbzh-scan1`, `odim-norst-20170421-0908-pvol`, `odim-fianj-20260924-2130-pvol-dataset1-trim`, `odim-bejab-20260612-1450-dbzh`, `odim-nohur-20260612-1445-dbzh`
- `moments:dbzh-th-vradh` (1): `odim-iesha-20260305-0115-pvol`
- `moments:dbzh-vrad` (1): `odim-dkrom-20260820-1130-pvol-h5edge-paged-ea`
- `moments:dbzh-vradh` (5): `odim-espdg-20260707-1927-pvol-dbzh-vradh`, `odim-seang-20260924-2130-qcvol-dataset1-trim`, `odim-au02-20260921-0000-pvol-subset`, `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-per-ray-geometry`, `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-finest-geometry-netcdf4`
- `moments:dualpol` (5): `odim-dkrom-20260820-1130-pvol`, `odim-dkrom-20260820-1130-pvol-h5latest-trim`, `odim-dkrom-20260820-1130-pvol-h5edge-len4`, `dorade-noxp-20090525-203211-sector`, `dorade-dow6-20211230-222139-rhi-head41`
- `moments:kdp` (1): `odim-imgw-ram-20260711-0015-kdp-max`
- `moments:phidp` (1): `odim-imgw-ram-20260711-0015-phidp-max`
- `moments:reflectivity` (4): `jma-n5-20191012-090000`, `jma-n5-20191012-090000-rs47773`, `jma-n5-20260924-210000`, `jma-n5-20260924-210000-rs47937`
- `moments:rhohv` (1): `odim-imgw-ram-20260711-0015-rhohv-max`
- `moments:th` (2): `odim-deboo-20260924-2130-sweep-th-00`, `odim-nohur-20260612-1445-th`
- `moments:velocity` (4): `jma-n6-20191012-090000`, `jma-n6-20191012-090000-rs47773`, `jma-n6-20260924-210000`, `jma-n6-20260924-210000-rs47937`
- `moments:vrad` (1): `odim-bejab-20260612-1450-vrad`
- `moments:vradh` (1): `odim-nohur-20260612-1446-vradh`
- `moments:zdr` (1): `odim-imgw-ram-20260711-0015-zdr-max`

#### `mrle:`

- `mrle:3` (2): `l2-kilx-20260418-013553`, `l2-kilx-20260418-013553-trim`

#### `msg:`

- `msg:1` (9): `l2-ktlx-19910605-162126`, `l2-ktlx-19990503-230052`, `l2-ktlx-19990504-002218`, `l2-ktlx-20030508-221041`, `l2-klix-20050829-130035`, `l2-ktlx-19910605-162126-trim`, `l2-ktlx-19990504-002218-trim`, `l2-ktlx-20030508-221041-trim`, `l2-klix-20050829-130035-trim`
- `msg:2` (16): `l2-ktlx-19910605-162126`, `l2-ktlx-19990503-230052`, `l2-ktlx-19990504-002218`, `l2-ktlx-20030508-221041`, `l2-klix-20050829-130035`, `l2-kvwx-20080415-235337`, `l2-tstl-20230331-230314`, `l2-tbwi-20230601-175101-stub`, `l2chunk-tlas-998-20260917-012843-001-s`, `l2chunk-tlas-999-20260917-013443-001-s`, `l2chunk-tlas-3-20260917-015242-001-s`, `l2-ktlx-19910605-162126-trim`, `l2-ktlx-19990504-002218-trim`, `l2-ktlx-20030508-221041-trim`, `l2-klix-20050829-130035-trim`, `l2-tstl-20230331-230314-trim`
- `msg:3` (2): `l2-klix-20050829-130035`, `l2-klix-20050829-130035-trim`
- `msg:5` (9): `ndswc-kxwa-20260924-214316`, `ndswc-kxwa-20260924-214316-head41`, `l2-klix-20050829-130035`, `l2-tstl-20230331-230314`, `l2chunk-tlas-998-20260917-012843-001-s`, `l2chunk-tlas-999-20260917-013443-001-s`, `l2chunk-tlas-3-20260917-015242-001-s`, `l2-klix-20050829-130035-trim`, `l2-tstl-20230331-230314-trim`
- `msg:13` (10): `l2-klix-20050829-130035`, `l2-kvwx-20080415-235337`, `l2-kdmx-20080525-205148`, `l2-kdvn-20200810-175718`, `l2-kdvn-20200810-180401`, `l2-kdvn-20200810-181043`, `l2-kdvn-20200810-181724`, `l2-klix-20050829-130035-trim`, `l2-kdmx-20080525-205148-trim`, `l2-kdvn-20200810-180401-trim`
- `msg:15` (3): `l2-klix-20050829-130035`, `l2-kvwx-20080415-235337`, `l2-klix-20050829-130035-trim`
- `msg:18` (2): `l2-klix-20050829-130035`, `l2-klix-20050829-130035-trim`
- `msg:29` (1): `l2-klix-20210829-175748-mdm`
- `msg:31` (48): `ndswc-kxwa-20260924-214316`, `ndswc-kxwa-20260924-214316-head41`, `fuzz-level2-writer-nexrad-moment-nan-scale`, `fuzz-writers-l2-sweep-without-gates`, `fuzz-writers-l2-one-gate-sweep`, `fuzz-writers-l2-odim-rstart-beyond-20-km`, `fuzz-writers-l2-empty-field-name`, `l2-kvwx-20080415-235337`, `l2-kpah-20080415-235014`, `l2-kdmx-20080525-205148`, `l2-kvnx-20110315-000203`, `l2-ktlx-20130520-201643`, `l2-kgwx-20130601-235640`, `l2-koax-20140616-205305`, `l2-kewx-20160413-022531`, `l2-kdvn-20200810-175718`, `l2-kdvn-20200810-180401`, `l2-kdvn-20200810-181043`, `l2-kdvn-20200810-181724`, `l2-klix-20210829-180425`, `l2-klix-20210829-173117`, `l2-klix-20210829-175748`, `l2-kbox-20220129-150537`, `l2-tjua-20220918-190621`, `l2-kdgx-20230325-010651`, `l2-kmaf-20230331-230843`, `l2-tstl-20230331-230314`, `l2-pgua-20230524-030945`, `l2-kmtx-20240301-212827`, `l2-ktlx-20240315-000217`, `l2-ktlx-20240515-000014`, `l2-pahg-20250909-212549`, `l2-kilx-20260418-013553`, `l2-kiwa-20260917-003629`, `l2chunk-tlas-3-20260917-015242-002-i`, `l2chunk-tlas-3-20260917-015242-003-i`, `l2-kdmx-20080525-205148-trim`, `l2-ktlx-20130520-201643-trim`, `l2-koax-20140616-205305-trim`, `l2-kewx-20160413-022531-trim`, `l2-kdvn-20200810-180401-trim`, `l2-klix-20210829-180425-trim`, `l2-kbox-20220129-150537-trim`, `l2-tstl-20230331-230314-trim`, `l2-pgua-20230524-030945-trim`, `l2-kmtx-20240301-212827-trim`, `l2-ktlx-20240315-000217-trim`, `l2-kilx-20260418-013553-trim`
- `msg:32` (4): `l2-pahg-20250909-212549`, `l2-kilx-20260418-013553`, `l2-kiwa-20260917-003629`, `l2-kilx-20260418-013553-trim`
- `msg:202` (2): `l2-ktlx-20030508-221041`, `l2-ktlx-20030508-221041-trim`

#### `netcdf:`

- `netcdf:classic` (1): `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-per-ray-geometry`
- `netcdf:netcdf4` (1): `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-finest-geometry-netcdf4`
- `netcdf:user-defined-types` (1): `cfrad1-xsapr-sgp-20110520-ppi-netcdf4-user-types`

#### `network:`

- `network:jma` (8): `jma-n5-20191012-090000`, `jma-n6-20191012-090000`, `jma-n5-20191012-090000-rs47773`, `jma-n6-20191012-090000-rs47773`, `jma-n5-20260924-210000`, `jma-n6-20260924-210000`, `jma-n5-20260924-210000-rs47937`, `jma-n6-20260924-210000-rs47937`

#### `object:`

- `object:image` (4): `odim-imgw-ram-20260711-0015-kdp-max`, `odim-imgw-ram-20260711-0015-phidp-max`, `odim-imgw-ram-20260711-0015-rhohv-max`, `odim-imgw-ram-20260711-0015-zdr-max`
- `object:pvol` (19): `odim-bejab-20190606-0000-pvol`, `odim-bewid-20130429-0430-pvol-dbzh-scan1`, `odim-norst-20170421-0908-pvol`, `odim-espdg-20260707-1927-pvol-dbzh-vradh`, `odim-iesha-20260305-0115-pvol`, `odim-dkrom-20260820-1130-pvol`, `odim-dkrom-20260820-1130-pvol-h5latest-trim`, `odim-dkrom-20260820-1130-pvol-h5edge-paged-ea`, `odim-dkrom-20260820-1130-pvol-h5edge-len4`, `odim-au24-20260610-000300-nci-zip-member`, `odim-seang-20260924-2130-qcvol-dataset1-trim`, `odim-fianj-20260924-2130-pvol-dataset1-trim`, `odim-itdes-20260924-2135-pvol-class`, `odim-bejab-20260612-1450-dbzh`, `odim-bejab-20260612-1450-vrad`, `odim-nohur-20260612-1445-dbzh`, `odim-nohur-20260612-1445-th`, `odim-nohur-20260612-1446-vradh`, `odim-au02-20260921-0000-pvol-subset`
- `object:scan` (1): `odim-deboo-20260924-2130-sweep-th-00`

#### `odim:`

- `odim:h5rad-2.0` (7): `odim-bejab-20190606-0000-pvol`, `odim-dkrom-20260820-1130-pvol`, `odim-dkrom-20260820-1130-pvol-h5latest-trim`, `odim-dkrom-20260820-1130-pvol-h5edge-paged-ea`, `odim-dkrom-20260820-1130-pvol-h5edge-len4`, `odim-bejab-20260612-1450-dbzh`, `odim-bejab-20260612-1450-vrad`
- `odim:h5rad-2.1` (1): `odim-bewid-20130429-0430-pvol-dbzh-scan1`
- `odim:h5rad-2.2` (7): `odim-norst-20170421-0908-pvol`, `odim-seang-20260924-2130-qcvol-dataset1-trim`, `odim-deboo-20260924-2130-sweep-th-00`, `odim-itdes-20260924-2135-pvol-class`, `odim-nohur-20260612-1445-dbzh`, `odim-nohur-20260612-1445-th`, `odim-nohur-20260612-1446-vradh`
- `odim:h5rad-2.3` (6): `odim-imgw-ram-20260711-0015-kdp-max`, `odim-imgw-ram-20260711-0015-phidp-max`, `odim-imgw-ram-20260711-0015-rhohv-max`, `odim-imgw-ram-20260711-0015-zdr-max`, `odim-iesha-20260305-0115-pvol`, `odim-fianj-20260924-2130-pvol-dataset1-trim`
- `odim:h5rad-2.4` (3): `odim-espdg-20260707-1927-pvol-dbzh-vradh`, `odim-au24-20260610-000300-nci-zip-member`, `odim-au02-20260921-0000-pvol-subset`
- `odim:how-subgroups` (2): `odim-seang-20260924-2130-qcvol-dataset1-trim`, `odim-deboo-20260924-2130-sweep-th-00`
- `odim:legend` (2): `odim-fianj-20260924-2130-pvol-dataset1-trim`, `odim-itdes-20260924-2135-pvol-class`
- `odim:per-ray-how-arrays` (1): `odim-au02-20260921-0000-pvol-subset`
- `odim:quality-groups` (3): `odim-seang-20260924-2130-qcvol-dataset1-trim`, `odim-fianj-20260924-2130-pvol-dataset1-trim`, `odim-au02-20260921-0000-pvol-subset`

#### `p3:`

- `p3:three-moment` (2): `wrf-p3-lookup-table-1-v5.4-3momI`, `wrf-p3-lookup-table-1-v5.4-3momI-first-block`
- `p3:two-moment` (2): `wrf-p3-lookup-table-1-v5.4-2momI`, `wrf-p3-lookup-table-1-v5.4-2momI-first-block`
- `p3:v5.4` (4): `wrf-p3-lookup-table-1-v5.4-2momI`, `wrf-p3-lookup-table-1-v5.4-3momI`, `wrf-p3-lookup-table-1-v5.4-2momI-first-block`, `wrf-p3-lookup-table-1-v5.4-3momI-first-block`

#### `packet:`

- `packet:0x0e03` (9): `l3-grr-039-20011011-0631`, `l3-ind-042-19940910-1642`, `l3-tlx-n0m-20130520-2016`, `l3-tlx-n0m-20260622-080623`, `l3-tlx-n1m-20130520-2016`, `l3-tlx-n2m-20130520-2016`, `l3-tlx-n3m-20130520-2016`, `l3-tlx-nam-20130520-2016`, `l3-tlx-nbm-20130520-2016`
- `packet:0x0802` (9): `l3-grr-039-20011011-0631`, `l3-ind-042-19940910-1642`, `l3-tlx-n0m-20130520-2016`, `l3-tlx-n0m-20260622-080623`, `l3-tlx-n1m-20130520-2016`, `l3-tlx-n2m-20130520-2016`, `l3-tlx-n3m-20130520-2016`, `l3-tlx-nam-20130520-2016`, `l3-tlx-nbm-20130520-2016`
- `packet:0xaf1f` (58): `l3-akc-nc1-20210730-055033`, `l3-akq-018-19940810-0835`, `l3-bmx-029-19940914-1621`, `l3-cae-n0r-19940629-1906`, `l3-cys-021-19941114-1107`, `l3-ftg-022-19940601-1917`, `l3-ftg-026-19940930-0536`, `l3-ftg-043-19940930-1849`, `l3-ftg-044-19940930-1849`, `l3-ftg-045-19940930-1849`, `l3-ftg-046-19940930-1849`, `l3-fws-n0r-19950517-2304`, `l3-fws-now-19950517-2304`, `l3-gjx-n0f-20200817-0551`, `l3-gjx-naf-20200817-0551`, `l3-gjx-nbf-20200817-0551`, `l3-gjx-nxf-20200817-0600`, `l3-ind-016-19940910-0647`, `l3-ind-017-19940910-1104`, `l3-jan-024-19951003-1312`, `l3-jfk-tr0-20210120-154051`, `l3-lot-055-19941031-1137`, `l3-lot-n0r-19941106-0246`, `l3-mci-n1p-20160526-2154`, `l3-mci-ntp-20160526-2154`, `l3-mci-tr0-20160526-2154`, `l3-mci-tr1-20160526-2154`, `l3-mci-tr2-20160526-2154`, `l3-rax-nyf-20200818-0001`, `l3-tlx-n0f-20220502-235926`, `l3-tlx-n0r-20130520-2016`, `l3-tlx-n0r-20220908-131957`, `l3-tlx-n0s-20130520-2016`, `l3-tlx-n0s-20260622-080623`, `l3-tlx-n0v-20130520-2016`, `l3-tlx-n0v-20220908-131957`, `l3-tlx-n0z-20130520-2016`, `l3-tlx-n0z-20220908-131957`, `l3-tlx-n1p-20130520-2016`, `l3-tlx-n1p-20260629-173638`, `l3-tlx-n1s-20130520-2016`, `l3-tlx-n2s-20130520-2016`, `l3-tlx-n3p-20130520-2012`, `l3-tlx-n3p-20220503-011226`, `l3-tlx-n3s-20130520-2016`, `l3-tlx-nc1-20130520-2354`, `l3-tlx-nc2-20130520-2354`, `l3-tlx-nc3-20130520-2354`, `l3-tlx-nc4-20130520-2354`, `l3-tlx-nc5-20130520-2354`, `l3-tlx-nsp-20130520-2016`, `l3-tlx-nsw-20130520-2016`, `l3-tlx-nsw-20220503-005231`, `l3-tlx-ntp-20130520-2016`, `l3-tlx-ntp-20260629-173638`, `l3-tlx-oha-20130520-2016`, `l3-tlx-oha-20260622-080623`, `l3-tlx-pta-20130520-2016`
- `packet:0xba07` (37): `l3-cae-053-19940629-1906`, `l3-fws-n1p-19950517-2304`, `l3-fws-ncz-19950517-2304`, `l3-fws-ntp-19950517-2304`, `l3-ilx-ncz-19960419-2320`, `l3-ind-035-19941031-2138`, `l3-lot-050-19941031-1358`, `l3-lot-053-19941106-0246`, `l3-lzk-ncz-19970301-1912`, `l3-mci-ncr-20160526-2154`, `l3-mci-net-20160526-2154`, `l3-mci-nvl-20160526-2154`, `l3-mlb-051-19941116-0335`, `l3-okc-ncr-20260622-080623`, `l3-okc-net-20220503-005210`, `l3-okc-nvl-20260622-080623`, `l3-rax-nll-20220510-155126`, `l3-tlx-063-19940308-1930`, `l3-tlx-064-19940308-1930`, `l3-tlx-087-19940308-1939`, `l3-tlx-nco-20130520-1816`, `l3-tlx-ncr-20130520-2016`, `l3-tlx-ncr-20260622-080623`, `l3-tlx-ncz-19990503-2316`, `l3-tlx-ncz-20130520-2016`, `l3-tlx-ncz-20220503-005231`, `l3-tlx-net-20130520-2016`, `l3-tlx-net-20220503-005231`, `l3-tlx-nhl-20130520-2016`, `l3-tlx-nhl-20220503-005231`, `l3-tlx-nla-20130520-2016`, `l3-tlx-nla-20220503-005231`, `l3-tlx-nll-20130520-2016`, `l3-tlx-nml-20130520-2016`, `l3-tlx-nml-20220503-005231`, `l3-tlx-nvl-20130520-2012`, `l3-tlx-nvl-20260622-080623`
- `packet:1` (19): `l3-cae-053-19940629-1906`, `l3-fws-dpa-19950517-2304`, `l3-fws-sup-19950517-2304`, `l3-lot-050-19941031-1358`, `l3-lot-053-19941106-0246`, `l3-mci-dhr-20160526-2154`, `l3-mci-dpa-20160526-2154`, `l3-mci-dsp-20160526-2154`, `l3-mlb-051-19941116-0335`, `l3-rax-dta-20200818-0454`, `l3-tlx-dhr-20130520-2016`, `l3-tlx-dhr-20260622-080623`, `l3-tlx-dpa-20130520-2016`, `l3-tlx-dpa-20260629-173638`, `l3-tlx-dsp-20130520-2016`, `l3-tlx-dsp-20260629-173638`, `l3-tlx-dta-20130520-2016`, `l3-tlx-dta-20260622-080623`, `l3-tlx-pta-20200501-000023`
- `packet:2` (12): `l3-fws-nst-19950517-2304`, `l3-ilx-irm-19960419-2309`, `l3-ilx-nst-19960419-2303`, `l3-mci-nmd-20160526-2154`, `l3-mci-nst-20160526-2154`, `l3-okc-nmd-20260622-080640`, `l3-okc-nst-20260622-080640`, `l3-tlx-irm-19940308-1115`, `l3-tlx-nmd-20130520-2016`, `l3-tlx-nmd-20260622-080623`, `l3-tlx-nst-20130520-2016`, `l3-tlx-nst-20260622-080623`
- `packet:3` (4): `l3-fws-nme-19950517-2316`, `l3-ilx-nme-19960419-2303`, `l3-lzk-nme-19970301-2027`, `l3-sgf-nme-20030504-2332`
- `packet:4` (6): `l3-fws-nvw-19950517-2322`, `l3-lot-nvw-19931120-0721`, `l3-mci-nvw-20160526-2154`, `l3-okc-nvw-20260622-080623`, `l3-tlx-nvw-20130520-2016`, `l3-tlx-nvw-20260622-080623`
- `packet:6` (10): `l3-fws-nst-19950517-2304`, `l3-ilx-nst-19960419-2303`, `l3-mci-nmd-20160526-2154`, `l3-mci-nst-20160526-2154`, `l3-okc-nmd-20260622-080640`, `l3-okc-nst-20260622-080640`, `l3-tlx-nmd-20130520-2016`, `l3-tlx-nmd-20260622-080623`, `l3-tlx-nst-20130520-2016`, `l3-tlx-nst-20260622-080623`
- `packet:7` (4): `l3-cae-053-19940629-1906`, `l3-lot-050-19941031-1358`, `l3-lot-053-19941106-0246`, `l3-mlb-051-19941116-0335`
- `packet:8` (46): `l3-fws-ncz-19950517-2304`, `l3-fws-nhi-19950517-1323`, `l3-fws-nme-19950517-2316`, `l3-fws-nst-19950517-2304`, `l3-fws-nvw-19950517-2322`, `l3-fws-nwp-19950517-2304`, `l3-grr-039-20011011-0631`, `l3-ilx-ncz-19960419-2320`, `l3-ilx-nhi-19960419-2303`, `l3-ilx-nme-19960419-2303`, `l3-ilx-nst-19960419-2303`, `l3-ilx-ntv-19960419-2303`, `l3-ind-035-19941031-2138`, `l3-lot-084-19931120-0721`, `l3-lot-nvw-19931120-0721`, `l3-lzk-ncz-19970301-1912`, `l3-lzk-nme-19970301-2027`, `l3-lzk-ntv-19970301-2027`, `l3-mci-ncr-20160526-2154`, `l3-mci-nmd-20160526-2154`, `l3-mci-nst-20160526-2154`, `l3-mci-nvw-20160526-2154`, `l3-okc-ncr-20260622-080623`, `l3-okc-nhi-20220503-005210`, `l3-okc-nmd-20260622-080640`, `l3-okc-nst-20260622-080640`, `l3-okc-ntv-20220503-005210`, `l3-okc-nvw-20260622-080623`, `l3-sgf-nme-20030504-2332`, `l3-sgf-ntv-20030504-2352`, `l3-tlx-nco-20130520-1816`, `l3-tlx-ncr-20130520-2016`, `l3-tlx-ncr-20260622-080623`, `l3-tlx-ncz-19990503-2316`, `l3-tlx-ncz-20130520-2016`, `l3-tlx-ncz-20220503-005231`, `l3-tlx-nhi-20130520-2016`, `l3-tlx-nhi-20220503-005231`, `l3-tlx-nmd-20130520-2016`, `l3-tlx-nmd-20260622-080623`, `l3-tlx-nst-20130520-2016`, `l3-tlx-nst-20260622-080623`, `l3-tlx-ntv-20130520-2016`, `l3-tlx-ntv-20220503-005231`, `l3-tlx-nvw-20130520-2016`, `l3-tlx-nvw-20260622-080623`
- `packet:9` (1): `l3-lot-084-19931120-0721`
- `packet:10` (45): `l3-fws-ncz-19950517-2304`, `l3-fws-nhi-19950517-1323`, `l3-fws-nme-19950517-2316`, `l3-fws-nst-19950517-2304`, `l3-fws-nvw-19950517-2322`, `l3-grr-039-20011011-0631`, `l3-ilx-ncz-19960419-2320`, `l3-ilx-nhi-19960419-2303`, `l3-ilx-nme-19960419-2303`, `l3-ilx-nst-19960419-2303`, `l3-ilx-ntv-19960419-2303`, `l3-ind-035-19941031-2138`, `l3-lot-084-19931120-0721`, `l3-lot-nvw-19931120-0721`, `l3-lzk-ncz-19970301-1912`, `l3-lzk-nme-19970301-2027`, `l3-lzk-ntv-19970301-2027`, `l3-mci-ncr-20160526-2154`, `l3-mci-nmd-20160526-2154`, `l3-mci-nst-20160526-2154`, `l3-mci-nvw-20160526-2154`, `l3-okc-ncr-20260622-080623`, `l3-okc-nhi-20220503-005210`, `l3-okc-nmd-20260622-080640`, `l3-okc-nst-20260622-080640`, `l3-okc-ntv-20220503-005210`, `l3-okc-nvw-20260622-080623`, `l3-sgf-nme-20030504-2332`, `l3-sgf-ntv-20030504-2352`, `l3-tlx-nco-20130520-1816`, `l3-tlx-ncr-20130520-2016`, `l3-tlx-ncr-20260622-080623`, `l3-tlx-ncz-19990503-2316`, `l3-tlx-ncz-20130520-2016`, `l3-tlx-ncz-20220503-005231`, `l3-tlx-nhi-20130520-2016`, `l3-tlx-nhi-20220503-005231`, `l3-tlx-nmd-20130520-2016`, `l3-tlx-nmd-20260622-080623`, `l3-tlx-nst-20130520-2016`, `l3-tlx-nst-20260622-080623`, `l3-tlx-ntv-20130520-2016`, `l3-tlx-ntv-20220503-005231`, `l3-tlx-nvw-20130520-2016`, `l3-tlx-nvw-20260622-080623`
- `packet:11` (3): `l3-fws-nme-19950517-2316`, `l3-ilx-nme-19960419-2303`, `l3-sgf-nme-20030504-2332`
- `packet:12` (6): `l3-ilx-ntv-19960419-2303`, `l3-lzk-ntv-19970301-2027`, `l3-okc-ntv-20220503-005210`, `l3-sgf-ntv-20030504-2352`, `l3-tlx-ntv-20130520-2016`, `l3-tlx-ntv-20220503-005231`
- `packet:13` (2): `l3-fws-nhi-19950517-1323`, `l3-ilx-nhi-19960419-2303`
- `packet:14` (2): `l3-fws-nhi-19950517-1323`, `l3-ilx-nhi-19960419-2303`
- `packet:15` (23): `l3-fws-nhi-19950517-1323`, `l3-fws-nme-19950517-2316`, `l3-fws-nst-19950517-2304`, `l3-ilx-irm-19960419-2309`, `l3-ilx-nhi-19960419-2303`, `l3-ilx-nme-19960419-2303`, `l3-ilx-nst-19960419-2303`, `l3-ilx-ntv-19960419-2303`, `l3-lzk-nme-19970301-2027`, `l3-lzk-ntv-19970301-2027`, `l3-mci-nst-20160526-2154`, `l3-okc-nhi-20220503-005210`, `l3-okc-nst-20260622-080640`, `l3-okc-ntv-20220503-005210`, `l3-sgf-nme-20030504-2332`, `l3-sgf-ntv-20030504-2352`, `l3-tlx-irm-19940308-1115`, `l3-tlx-nhi-20130520-2016`, `l3-tlx-nhi-20220503-005231`, `l3-tlx-nst-20130520-2016`, `l3-tlx-nst-20260622-080623`, `l3-tlx-ntv-20130520-2016`, `l3-tlx-ntv-20220503-005231`
- `packet:16` (93): `l3-byx-n0q-20150124-2106`, `l3-ddc-n0q-20200817-0501`, `l3-ddc-n0q-20200817-0503`, `l3-den-tz0-20200804-2226`, `l3-den-tz1-20200804-2226`, `l3-den-tz2-20200804-2227`, `l3-eax-n0q-20200817-0401`, `l3-eax-n0q-20200817-0405`, `l3-ffc-n0q-20140407-1805`, `l3-ftg-n0b-20220304-1820`, `l3-gjx-nyq-20220503-005356`, `l3-lzk-h0c-20200814-0417`, `l3-lzk-h0v-20200812-1309`, `l3-lzk-h0w-20200812-1305`, `l3-lzk-h0z-20200812-1318`, `l3-mci-dhr-20160526-2154`, `l3-mci-dsp-20160526-2154`, `l3-mci-tv0-20160526-2154`, `l3-mci-tv1-20160526-2154`, `l3-mci-tv2-20160526-2154`, `l3-mci-tzl-20160526-2154`, `l3-okc-tv0-20260622-080547`, `l3-okc-tz0-20260622-080547`, `l3-okc-tzl-20260622-080623`, `l3-rax-dta-20200818-0454`, `l3-shv-nzq-20220503-005452`, `l3-slc-tv0-20160516-2359`, `l3-tlx-daa-20130520-2016`, `l3-tlx-daa-20260622-080623`, `l3-tlx-dhr-20130520-2016`, `l3-tlx-dhr-20260622-080623`, `l3-tlx-dod-20130520-2016`, `l3-tlx-dod-20220503-005231`, `l3-tlx-dsd-20130520-2016`, `l3-tlx-dsd-20220503-005231`, `l3-tlx-dsp-20130520-2016`, `l3-tlx-dsp-20260629-173638`, `l3-tlx-dta-20130520-2016`, `l3-tlx-dta-20260622-080623`, `l3-tlx-du3-20130520-2008`, `l3-tlx-du3-20260622-080623`, `l3-tlx-du6-20260622-120608`, `l3-tlx-dvl-20130520-2016`, `l3-tlx-dvl-20260622-080623`, `l3-tlx-eet-20130520-2016`, `l3-tlx-eet-20260622-080623`, `l3-tlx-hhc-20130520-2016`, `l3-tlx-hhc-20260622-080623`, `l3-tlx-n0b-20260622-080623`, `l3-tlx-n0c-20130520-2016`, `l3-tlx-n0c-20260622-080623`, `l3-tlx-n0g-20260622-080623`, `l3-tlx-n0h-20130520-2016`, `l3-tlx-n0h-20260622-080623`, `l3-tlx-n0k-20130520-2016`, `l3-tlx-n0k-20260622-080623`, `l3-tlx-n0q-20130520-2016`, `l3-tlx-n0q-20220503-005231`, `l3-tlx-n0u-20130520-2016`, `l3-tlx-n0u-20220503-005231`, `l3-tlx-n0x-20130520-2016`, `l3-tlx-n0x-20260622-080623`, `l3-tlx-n1c-20130520-2016`, `l3-tlx-n1h-20130520-2016`, `l3-tlx-n1k-20130520-2016`, `l3-tlx-n1q-20130520-2016`, `l3-tlx-n1u-20130520-2016`, `l3-tlx-n1x-20130520-2016`, `l3-tlx-n2c-20130520-2016`, `l3-tlx-n2h-20130520-2016`, `l3-tlx-n2k-20130520-2016`, `l3-tlx-n2q-20130520-2016`, `l3-tlx-n2u-20130520-2016`, `l3-tlx-n2x-20130520-2016`, `l3-tlx-n3c-20130520-2016`, `l3-tlx-n3h-20130520-2016`, `l3-tlx-n3k-20130520-2016`, `l3-tlx-n3q-20130520-2016`, `l3-tlx-n3u-20130520-2016`, `l3-tlx-n3x-20130520-2016`, `l3-tlx-nac-20130520-2016`, `l3-tlx-nah-20130520-2016`, `l3-tlx-nak-20130520-2016`, `l3-tlx-naq-20130520-2016`, `l3-tlx-nau-20130520-2016`, `l3-tlx-nax-20130520-2016`, `l3-tlx-nbc-20130520-2016`, `l3-tlx-nbh-20130520-2016`, `l3-tlx-nbk-20130520-2016`, `l3-tlx-nbq-20130520-2016`, `l3-tlx-nbu-20130520-2016`, `l3-tlx-nbx-20130520-2016`, `l3-tlx-nrr-20260622-080623`
- `packet:17` (4): `l3-fws-dpa-19950517-2304`, `l3-mci-dpa-20160526-2154`, `l3-tlx-dpa-20130520-2016`, `l3-tlx-dpa-20260629-173638`
- `packet:18` (4): `l3-fws-dpa-19950517-2304`, `l3-fws-sup-19950517-2304`, `l3-mci-dpa-20160526-2154`, `l3-tlx-dpa-20130520-2016`
- `packet:19` (3): `l3-okc-nhi-20220503-005210`, `l3-tlx-nhi-20130520-2016`, `l3-tlx-nhi-20220503-005231`
- `packet:20` (4): `l3-mci-nmd-20160526-2154`, `l3-okc-nmd-20260622-080640`, `l3-tlx-nmd-20130520-2016`, `l3-tlx-nmd-20260622-080623`
- `packet:21` (2): `l3-tlx-nss-20130520-2016`, `l3-tlx-nss-20220503-005231`
- `packet:22` (2): `l3-tlx-nss-20130520-2016`, `l3-tlx-nss-20220503-005231`
- `packet:23` (8): `l3-mci-nmd-20160526-2154`, `l3-mci-nst-20160526-2154`, `l3-okc-nmd-20260622-080640`, `l3-okc-nst-20260622-080640`, `l3-tlx-nmd-20130520-2016`, `l3-tlx-nmd-20260622-080623`, `l3-tlx-nst-20130520-2016`, `l3-tlx-nst-20260622-080623`
- `packet:24` (8): `l3-mci-nmd-20160526-2154`, `l3-mci-nst-20160526-2154`, `l3-okc-nmd-20260622-080640`, `l3-okc-nst-20260622-080640`, `l3-tlx-nmd-20130520-2016`, `l3-tlx-nmd-20260622-080623`, `l3-tlx-nst-20130520-2016`, `l3-tlx-nst-20260622-080623`
- `packet:25` (1): `l3-tlx-nst-20260622-080623`
- `packet:28` (5): `l3-okc-rsl-20220517-085551`, `l3-tlx-dpr-20130520-2016`, `l3-tlx-dpr-20260622-080623`, `l3-tlx-rsl-20130520-2358`, `l3-tlx-rsl-20220502-235926`
- `packet:30` (3): `l3-ilx-irm-19960419-2309`, `l3-lot-irm-19941031-0503`, `l3-tlx-irm-19940308-1115`
- `packet:31` (3): `l3-ilx-irm-19960419-2309`, `l3-lot-irm-19941031-0503`, `l3-tlx-irm-19940308-1115`
- `packet:32` (3): `l3-ilx-irm-19960419-2309`, `l3-lot-irm-19941031-0503`, `l3-tlx-irm-19940308-1115`

#### `platform:`

- `platform:aircraft` (4): `dorade-n42rf-ts-20181010-122951-air`, `dorade-n42rf-ts-20181010-122951-air-head24`, `dorade-n42rf-tm-20181010-123925-air`, `dorade-n42rf-tm-20181010-123925-air-head48`
- `platform:mobile` (12): `cfrad1-dow8-20211011-223602-rhi`, `cfrad1-dow8-20211011-223602-rhi-trim3-classic`, `cfrad1-irene-sr2-20110827-120420-sur-sweeps01`, `cfrad2-xradar-dow8-20211011-223602-rhi-r300`, `dorade-cow2-20260521-225514-sur-head24`, `dorade-noxp-20090501-190244-ppi`, `dorade-noxp-20090501-190324-ppi`, `dorade-noxp-20090525-203211-sector`, `dorade-dow6-20211230-222139-rhi-head41`, `dorade-noxp-20090610-003210-ppi-head6`, `dorade-noxp-20090610-003222-ppi-head6`, `dorade-noxp-20090610-003226-ppi-head6`

#### `producer:`

- `producer:lrose-radx` (2): `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-per-ray-geometry`, `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-finest-geometry-netcdf4`

#### `product:`

- `product:16` (1): `l3-ind-016-19940910-0647`
- `product:17` (1): `l3-ind-017-19940910-1104`
- `product:18` (1): `l3-akq-018-19940810-0835`
- `product:19` (5): `l3-cae-n0r-19940629-1906`, `l3-fws-n0r-19950517-2304`, `l3-lot-n0r-19941106-0246`, `l3-tlx-n0r-20130520-2016`, `l3-tlx-n0r-20220908-131957`
- `product:20` (2): `l3-tlx-n0z-20130520-2016`, `l3-tlx-n0z-20220908-131957`
- `product:21` (1): `l3-cys-021-19941114-1107`
- `product:22` (1): `l3-ftg-022-19940601-1917`
- `product:24` (1): `l3-jan-024-19951003-1312`
- `product:25` (1): `l3-fws-now-19950517-2304`
- `product:26` (1): `l3-ftg-026-19940930-0536`
- `product:27` (2): `l3-tlx-n0v-20130520-2016`, `l3-tlx-n0v-20220908-131957`
- `product:28` (1): `l3-tlx-nsp-20130520-2016`
- `product:29` (1): `l3-bmx-029-19940914-1621`
- `product:30` (2): `l3-tlx-nsw-20130520-2016`, `l3-tlx-nsw-20220503-005231`
- `product:32` (3): `l3-mci-dhr-20160526-2154`, `l3-tlx-dhr-20130520-2016`, `l3-tlx-dhr-20260622-080623`
- `product:34` (6): `l3-akc-nc1-20210730-055033`, `l3-tlx-nc1-20130520-2354`, `l3-tlx-nc2-20130520-2354`, `l3-tlx-nc3-20130520-2354`, `l3-tlx-nc4-20130520-2354`, `l3-tlx-nc5-20130520-2354`
- `product:35` (1): `l3-ind-035-19941031-2138`
- `product:36` (1): `l3-tlx-nco-20130520-1816`
- `product:37` (4): `l3-mci-ncr-20160526-2154`, `l3-okc-ncr-20260622-080623`, `l3-tlx-ncr-20130520-2016`, `l3-tlx-ncr-20260622-080623`
- `product:38` (6): `l3-fws-ncz-19950517-2304`, `l3-ilx-ncz-19960419-2320`, `l3-lzk-ncz-19970301-1912`, `l3-tlx-ncz-19990503-2316`, `l3-tlx-ncz-20130520-2016`, `l3-tlx-ncz-20220503-005231`
- `product:39` (1): `l3-grr-039-20011011-0631`
- `product:41` (4): `l3-mci-net-20160526-2154`, `l3-okc-net-20220503-005210`, `l3-tlx-net-20130520-2016`, `l3-tlx-net-20220503-005231`
- `product:42` (1): `l3-ind-042-19940910-1642`
- `product:43` (1): `l3-ftg-043-19940930-1849`
- `product:44` (1): `l3-ftg-044-19940930-1849`
- `product:45` (1): `l3-ftg-045-19940930-1849`
- `product:46` (1): `l3-ftg-046-19940930-1849`
- `product:47` (1): `l3-fws-nwp-19950517-2304`
- `product:48` (7): `l3-fws-nvw-19950517-2322`, `l3-lot-nvw-19931120-0721`, `l3-mci-nvw-20160526-2154`, `l3-okc-nvw-20260622-080623`, `l3-tlx-nvw-20130520-2016`, `l3-tlx-nvw-20260622-080623`, `l3-kbmx-19980416-0006-nvw`
- `product:50` (1): `l3-lot-050-19941031-1358`
- `product:51` (1): `l3-mlb-051-19941116-0335`
- `product:53` (2): `l3-cae-053-19940629-1906`, `l3-lot-053-19941106-0246`
- `product:55` (1): `l3-lot-055-19941031-1137`
- `product:56` (5): `l3-tlx-n0s-20130520-2016`, `l3-tlx-n0s-20260622-080623`, `l3-tlx-n1s-20130520-2016`, `l3-tlx-n2s-20130520-2016`, `l3-tlx-n3s-20130520-2016`
- `product:57` (4): `l3-mci-nvl-20160526-2154`, `l3-okc-nvl-20260622-080623`, `l3-tlx-nvl-20130520-2012`, `l3-tlx-nvl-20260622-080623`
- `product:58` (10): `l3-fws-nst-19950517-2304`, `l3-ilx-nst-19960419-2303`, `l3-mci-nst-20160526-2154`, `l3-okc-nst-20260622-080640`, `l3-tlx-nst-20130520-2016`, `l3-tlx-nst-20260622-080623`, `l3-kdvn-20200810-1757-nst`, `l3-kdvn-20200810-1804-nst`, `l3-kdvn-20200810-1810-nst`, `l3-kdvn-20200810-1817-nst`
- `product:59` (5): `l3-fws-nhi-19950517-1323`, `l3-ilx-nhi-19960419-2303`, `l3-okc-nhi-20220503-005210`, `l3-tlx-nhi-20130520-2016`, `l3-tlx-nhi-20220503-005231`
- `product:60` (4): `l3-fws-nme-19950517-2316`, `l3-ilx-nme-19960419-2303`, `l3-lzk-nme-19970301-2027`, `l3-sgf-nme-20030504-2332`
- `product:61` (6): `l3-ilx-ntv-19960419-2303`, `l3-lzk-ntv-19970301-2027`, `l3-okc-ntv-20220503-005210`, `l3-sgf-ntv-20030504-2352`, `l3-tlx-ntv-20130520-2016`, `l3-tlx-ntv-20220503-005231`
- `product:62` (3): `l3-fws-nss-19950517-2304`, `l3-tlx-nss-20130520-2016`, `l3-tlx-nss-20220503-005231`
- `product:63` (1): `l3-tlx-063-19940308-1930`
- `product:64` (1): `l3-tlx-064-19940308-1930`
- `product:65` (2): `l3-rax-nll-20220510-155126`, `l3-tlx-nll-20130520-2016`
- `product:66` (2): `l3-tlx-nml-20130520-2016`, `l3-tlx-nml-20220503-005231`
- `product:67` (2): `l3-tlx-nla-20130520-2016`, `l3-tlx-nla-20220503-005231`
- `product:73` (1): `l3-ind-073-19940910-1555`
- `product:74` (5): `fuzz-level3-rcm-centroid-non-ascii`, `l3-fws-rcm-19950517-2310`, `l3-ilx-rcm-19960419-2309`, `l3-tlx-rcm-20130520-2016`, `l3-tlx-rcm-20220503-004553`
- `product:75` (1): `l3-abr-ftm-20110428-1331`
- `product:78` (4): `l3-fws-n1p-19950517-2304`, `l3-mci-n1p-20160526-2154`, `l3-tlx-n1p-20130520-2016`, `l3-tlx-n1p-20260629-173638`
- `product:79` (2): `l3-tlx-n3p-20130520-2012`, `l3-tlx-n3p-20220503-011226`
- `product:80` (4): `l3-fws-ntp-19950517-2304`, `l3-mci-ntp-20160526-2154`, `l3-tlx-ntp-20130520-2016`, `l3-tlx-ntp-20260629-173638`
- `product:81` (4): `l3-fws-dpa-19950517-2304`, `l3-mci-dpa-20160526-2154`, `l3-tlx-dpa-20130520-2016`, `l3-tlx-dpa-20260629-173638`
- `product:82` (3): `l3-fws-sup-19950517-2304`, `l3-tlx-spd-20130520-2016`, `l3-tlx-spd-20220503-005231`
- `product:83` (3): `l3-ilx-irm-19960419-2309`, `l3-lot-irm-19941031-0503`, `l3-tlx-irm-19940308-1115`
- `product:84` (1): `l3-lot-084-19931120-0721`
- `product:87` (1): `l3-tlx-087-19940308-1939`
- `product:90` (2): `l3-tlx-nhl-20130520-2016`, `l3-tlx-nhl-20220503-005231`
- `product:94` (15): `l3-byx-n0q-20150124-2106`, `l3-ddc-n0q-20200817-0501`, `l3-ddc-n0q-20200817-0503`, `l3-eax-n0q-20200817-0401`, `l3-eax-n0q-20200817-0405`, `l3-ffc-n0q-20140407-1805`, `l3-gjx-nyq-20220503-005356`, `l3-shv-nzq-20220503-005452`, `l3-tlx-n0q-20130520-2016`, `l3-tlx-n0q-20220503-005231`, `l3-tlx-n1q-20130520-2016`, `l3-tlx-n2q-20130520-2016`, `l3-tlx-n3q-20130520-2016`, `l3-tlx-naq-20130520-2016`, `l3-tlx-nbq-20130520-2016`
- `product:99` (7): `l3-tlx-n0u-20130520-2016`, `l3-tlx-n0u-20220503-005231`, `l3-tlx-n1u-20130520-2016`, `l3-tlx-n2u-20130520-2016`, `l3-tlx-n3u-20130520-2016`, `l3-tlx-nau-20130520-2016`, `l3-tlx-nbu-20130520-2016`
- `product:100` (1): `l3-ftg-100-19940930-0102`
- `product:101` (2): `l3-lot-101-19930824-0005`, `l3-tlx-101-20010503-0007`
- `product:102` (2): `l3-ftg-102-19940930-1932`, `l3-tlx-102-19990504-0052`
- `product:103` (1): `l3-tlx-103-20010503-0007`
- `product:104` (2): `l3-ftg-104-19940930-0850`, `l3-tlx-104-20010503-2355`
- `product:107` (1): `l3-ftg-107-19940930-0154`
- `product:108` (1): `l3-lot-108-19930824-0005`
- `product:109` (1): `l3-ftg-109-19940930-0102`
- `product:113` (6): `l3-gjx-n0f-20200817-0551`, `l3-gjx-naf-20200817-0551`, `l3-gjx-nbf-20200817-0551`, `l3-gjx-nxf-20200817-0600`, `l3-rax-nyf-20200818-0001`, `l3-tlx-n0f-20220502-235926`
- `product:134` (2): `l3-tlx-dvl-20130520-2016`, `l3-tlx-dvl-20260622-080623`
- `product:135` (2): `l3-tlx-eet-20130520-2016`, `l3-tlx-eet-20260622-080623`
- `product:138` (3): `l3-mci-dsp-20160526-2154`, `l3-tlx-dsp-20130520-2016`, `l3-tlx-dsp-20260629-173638`
- `product:141` (4): `l3-mci-nmd-20160526-2154`, `l3-okc-nmd-20260622-080640`, `l3-tlx-nmd-20130520-2016`, `l3-tlx-nmd-20260622-080623`
- `product:152` (3): `l3-okc-rsl-20220517-085551`, `l3-tlx-rsl-20130520-2358`, `l3-tlx-rsl-20220502-235926`
- `product:153` (4): `l3-ftg-n0b-20220304-1820`, `l3-lzk-h0z-20200812-1318`, `l3-tlx-n0b-20260622-080623`, `l3-ktlx-20260622-080806-n0b-sails`
- `product:154` (2): `l3-lzk-h0v-20200812-1309`, `l3-tlx-n0g-20260622-080623`
- `product:155` (1): `l3-lzk-h0w-20200812-1305`
- `product:159` (7): `l3-tlx-n0x-20130520-2016`, `l3-tlx-n0x-20260622-080623`, `l3-tlx-n1x-20130520-2016`, `l3-tlx-n2x-20130520-2016`, `l3-tlx-n3x-20130520-2016`, `l3-tlx-nax-20130520-2016`, `l3-tlx-nbx-20130520-2016`
- `product:161` (7): `l3-tlx-n0c-20130520-2016`, `l3-tlx-n0c-20260622-080623`, `l3-tlx-n1c-20130520-2016`, `l3-tlx-n2c-20130520-2016`, `l3-tlx-n3c-20130520-2016`, `l3-tlx-nac-20130520-2016`, `l3-tlx-nbc-20130520-2016`
- `product:163` (7): `l3-tlx-n0k-20130520-2016`, `l3-tlx-n0k-20260622-080623`, `l3-tlx-n1k-20130520-2016`, `l3-tlx-n2k-20130520-2016`, `l3-tlx-n3k-20130520-2016`, `l3-tlx-nak-20130520-2016`, `l3-tlx-nbk-20130520-2016`
- `product:165` (7): `l3-tlx-n0h-20130520-2016`, `l3-tlx-n0h-20260622-080623`, `l3-tlx-n1h-20130520-2016`, `l3-tlx-n2h-20130520-2016`, `l3-tlx-n3h-20130520-2016`, `l3-tlx-nah-20130520-2016`, `l3-tlx-nbh-20130520-2016`
- `product:166` (7): `l3-tlx-n0m-20130520-2016`, `l3-tlx-n0m-20260622-080623`, `l3-tlx-n1m-20130520-2016`, `l3-tlx-n2m-20130520-2016`, `l3-tlx-n3m-20130520-2016`, `l3-tlx-nam-20130520-2016`, `l3-tlx-nbm-20130520-2016`
- `product:167` (1): `l3-lzk-h0c-20200814-0417`
- `product:169` (2): `l3-tlx-oha-20130520-2016`, `l3-tlx-oha-20260622-080623`
- `product:170` (2): `l3-tlx-daa-20130520-2016`, `l3-tlx-daa-20260622-080623`
- `product:171` (2): `l3-tlx-pta-20130520-2016`, `l3-tlx-pta-20200501-000023`
- `product:172` (3): `l3-rax-dta-20200818-0454`, `l3-tlx-dta-20130520-2016`, `l3-tlx-dta-20260622-080623`
- `product:173` (3): `l3-tlx-du3-20130520-2008`, `l3-tlx-du3-20260622-080623`, `l3-tlx-du6-20260622-120608`
- `product:174` (2): `l3-tlx-dod-20130520-2016`, `l3-tlx-dod-20220503-005231`
- `product:175` (2): `l3-tlx-dsd-20130520-2016`, `l3-tlx-dsd-20220503-005231`
- `product:176` (2): `l3-tlx-dpr-20130520-2016`, `l3-tlx-dpr-20260622-080623`
- `product:177` (2): `l3-tlx-hhc-20130520-2016`, `l3-tlx-hhc-20260622-080623`
- `product:180` (4): `l3-den-tz0-20200804-2226`, `l3-den-tz1-20200804-2226`, `l3-den-tz2-20200804-2227`, `l3-okc-tz0-20260622-080547`
- `product:181` (4): `l3-jfk-tr0-20210120-154051`, `l3-mci-tr0-20160526-2154`, `l3-mci-tr1-20160526-2154`, `l3-mci-tr2-20160526-2154`
- `product:182` (5): `l3-mci-tv0-20160526-2154`, `l3-mci-tv1-20160526-2154`, `l3-mci-tv2-20160526-2154`, `l3-okc-tv0-20260622-080547`, `l3-slc-tv0-20160516-2359`
- `product:186` (2): `l3-mci-tzl-20160526-2154`, `l3-okc-tzl-20260622-080623`
- `product:197` (1): `l3-tlx-nrr-20260622-080623`
- `product:N0B` (1): `l3-ktlx-20260622-080806-n0b-sails`
- `product:NST` (4): `l3-kdvn-20200810-1757-nst`, `l3-kdvn-20200810-1804-nst`, `l3-kdvn-20200810-1810-nst`, `l3-kdvn-20200810-1817-nst`
- `product:NVW` (1): `l3-kbmx-19980416-0006-nvw`
- `product:max` (4): `odim-imgw-ram-20260711-0015-kdp-max`, `odim-imgw-ram-20260711-0015-phidp-max`, `odim-imgw-ram-20260711-0015-rhohv-max`, `odim-imgw-ram-20260711-0015-zdr-max`

#### `project:`

- `project:vortex2` (9): `dorade-noxp-20090501-sweeps-tgz`, `dorade-noxp-20090525-sweeps-tgz`, `dorade-noxp-20090501-190244-ppi`, `dorade-noxp-20090501-190324-ppi`, `dorade-noxp-20090525-203211-sector`, `dorade-noxp-20090610-003210-ppi-head6`, `dorade-noxp-20090610-003222-ppi-head6`, `dorade-noxp-20090610-003226-ppi-head6`, `dorade-noxp-20090610-003210-heads-zip`

#### `provider:`

- `provider:arpa-lombardia` (1): `odim-itdes-20260924-2135-pvol-class`
- `provider:aws` (114): `l2-ktlx-19910605-162126`, `l2-ktlx-19990503-230052`, `l2-ktlx-19990504-002218`, `l2-ktlx-20030508-221041`, `l2-klix-20050829-130035`, `l2-kvwx-20080415-235337`, `l2-kpah-20080415-235014`, `l2-kdmx-20080525-205148`, `l2-kvnx-20110315-000203`, `l2-ktlx-20130520-201643`, `l2-kgwx-20130601-235640`, `l2-koax-20140616-205305`, `l2-kewx-20160413-022531`, `l2-kdvn-20200810-175718`, `l2-kdvn-20200810-180401`, `l2-kdvn-20200810-181043`, `l2-kdvn-20200810-181724`, `l2-klix-20210829-180425`, `l2-klix-20210829-173117`, `l2-klix-20210829-175748`, `l2-klix-20210829-175748-mdm`, `l2-kbox-20220129-150537`, `l2-tjua-20220918-190621`, `l2-kdgx-20230325-010651`, `l2-kmaf-20230331-230843`, `l2-tstl-20230331-230314`, `l2-pgua-20230524-030945`, `l2-tbwi-20230601-175101-stub`, `l2-kmtx-20240301-212827`, `l2-ktlx-20240315-000217`, `l2-ktlx-20240515-000014`, `l2-pahg-20250909-212549`, `l2-kilx-20260418-013553`, `l2-kiwa-20260917-003629`, `l2chunk-kiwa-307-20260917-003629-001-s`, `l2chunk-kiwa-307-20260917-003629-{002..069}-i`, `l2chunk-kiwa-307-20260917-003629-070-e`, `l2chunk-tlas-998-20260917-012843-001-s`, `l2chunk-tlas-999-20260917-013443-001-s`, `l2chunk-tlas-3-20260917-015242-001-s`, `l2chunk-tlas-3-20260917-015242-002-i`, `l2chunk-tlas-3-20260917-015242-003-i`, `l3-kdvn-20200810-1757-nst`, `l3-kdvn-20200810-1804-nst`, `l3-kdvn-20200810-1810-nst`, `l3-kdvn-20200810-1817-nst`, `l3-ktlx-20260622-080806-n0b-sails`
- `provider:aws-unidata-nexrad-level3` (67): `l3-akc-nc1-20210730-055033`, `l3-gjx-nyq-20220503-005356`, `l3-jfk-tr0-20210120-154051`, `l3-okc-ncr-20260622-080623`, `l3-okc-net-20220503-005210`, `l3-okc-nhi-20220503-005210`, `l3-okc-nmd-20260622-080640`, `l3-okc-nst-20260622-080640`, `l3-okc-ntv-20220503-005210`, `l3-okc-nvl-20260622-080623`, `l3-okc-nvw-20260622-080623`, `l3-okc-rsl-20220517-085551`, `l3-okc-tv0-20260622-080547`, `l3-okc-tz0-20260622-080547`, `l3-okc-tzl-20260622-080623`, `l3-rax-nll-20220510-155126`, `l3-shv-nzq-20220503-005452`, `l3-tlx-daa-20260622-080623`, `l3-tlx-dhr-20260622-080623`, `l3-tlx-dod-20220503-005231`, `l3-tlx-dpa-20260629-173638`, `l3-tlx-dpr-20260622-080623`, `l3-tlx-dsd-20220503-005231`, `l3-tlx-dsp-20260629-173638`, `l3-tlx-dta-20260622-080623`, `l3-tlx-du3-20260622-080623`, `l3-tlx-du6-20260622-120608`, `l3-tlx-dvl-20260622-080623`, `l3-tlx-eet-20260622-080623`, `l3-tlx-hhc-20260622-080623`, `l3-tlx-n0b-20260622-080623`, `l3-tlx-n0c-20260622-080623`, `l3-tlx-n0f-20220502-235926`, `l3-tlx-n0g-20260622-080623`, `l3-tlx-n0h-20260622-080623`, `l3-tlx-n0k-20260622-080623`, `l3-tlx-n0m-20260622-080623`, `l3-tlx-n0q-20220503-005231`, `l3-tlx-n0r-20220908-131957`, `l3-tlx-n0s-20260622-080623`, `l3-tlx-n0u-20220503-005231`, `l3-tlx-n0v-20220908-131957`, `l3-tlx-n0x-20260622-080623`, `l3-tlx-n0z-20220908-131957`, `l3-tlx-n1p-20260629-173638`, `l3-tlx-n3p-20220503-011226`, `l3-tlx-ncr-20260622-080623`, `l3-tlx-ncz-20220503-005231`, `l3-tlx-net-20220503-005231`, `l3-tlx-nhi-20220503-005231`, `l3-tlx-nhl-20220503-005231`, `l3-tlx-nla-20220503-005231`, `l3-tlx-nmd-20260622-080623`, `l3-tlx-nml-20220503-005231`, `l3-tlx-nrr-20260622-080623`, `l3-tlx-nss-20220503-005231`, `l3-tlx-nst-20260622-080623`, `l3-tlx-nsw-20220503-005231`, `l3-tlx-ntp-20260629-173638`, `l3-tlx-ntv-20220503-005231`, `l3-tlx-nvl-20260622-080623`, `l3-tlx-nvw-20260622-080623`, `l3-tlx-oha-20260622-080623`, `l3-tlx-pta-20200501-000023`, `l3-tlx-rcm-20220503-004553`, `l3-tlx-rsl-20220502-235926`, `l3-tlx-spd-20220503-005231`
- `provider:cswr` (1): `dorade-cow2-20260521-225514-sur-head24`
- `provider:dwd` (1): `odim-deboo-20260924-2130-sweep-th-00`
- `provider:ewr` (1): `polling-ewr-laredo-grlevel2-cfg-20260925`
- `provider:fmi` (3): `odim-fianj-20260924-2130-pvol-dataset1-trim`, `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-per-ray-geometry`, `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-finest-geometry-netcdf4`
- `provider:gcp-nexrad-l3` (4): `l3-kbmx-19980416-archive-tarz`, `l3-kbmx-19980416-0006-nvw`, `l3-knqa-20080205-archive-tarz`, `l3-knqa-20080205-0018-rob`
- `provider:github-radarqc-scans` (4): `dorade-n42rf-ts-20181010-122951-air`, `dorade-n42rf-ts-20181010-122951-air-head24`, `dorade-n42rf-tm-20181010-123925-air`, `dorade-n42rf-tm-20181010-123925-air-head48`
- `provider:iem` (1): `polling-iem-config-cfg-20260926`
- `provider:imgw-pib` (4): `odim-imgw-ram-20260711-0015-kdp-max`, `odim-imgw-ram-20260711-0015-phidp-max`, `odim-imgw-ram-20260711-0015-rhohv-max`, `odim-imgw-ram-20260711-0015-zdr-max`
- `provider:metpy-staticdata` (134): `l3-abr-ftm-20110428-1331`, `l3-byx-n0q-20150124-2106`, `l3-ddc-gsm-20200817-1000`, `l3-ddc-n0q-20200817-0501`, `l3-ddc-n0q-20200817-0503`, `l3-den-tz0-20200804-2226`, `l3-den-tz1-20200804-2226`, `l3-den-tz2-20200804-2227`, `l3-eax-gsm-20200817-0933`, `l3-eax-n0q-20200817-0401`, `l3-eax-n0q-20200817-0405`, `l3-ffc-n0q-20140407-1805`, `l3-ftg-n0b-20220304-1820`, `l3-gjx-n0f-20200817-0551`, `l3-gjx-naf-20200817-0551`, `l3-gjx-nbf-20200817-0551`, `l3-gjx-nxf-20200817-0600`, `l3-lzk-h0c-20200814-0417`, `l3-lzk-h0v-20200812-1309`, `l3-lzk-h0w-20200812-1305`, `l3-lzk-h0z-20200812-1318`, `l3-mci-dhr-20160526-2154`, `l3-mci-dpa-20160526-2154`, `l3-mci-dsp-20160526-2154`, `l3-mci-n1p-20160526-2154`, `l3-mci-ncr-20160526-2154`, `l3-mci-net-20160526-2154`, `l3-mci-nmd-20160526-2154`, `l3-mci-nst-20160526-2154`, `l3-mci-ntp-20160526-2154`, `l3-mci-nvl-20160526-2154`, `l3-mci-nvw-20160526-2154`, `l3-mci-tr0-20160526-2154`, `l3-mci-tr1-20160526-2154`, `l3-mci-tr2-20160526-2154`, `l3-mci-tv0-20160526-2154`, `l3-mci-tv1-20160526-2154`, `l3-mci-tv2-20160526-2154`, `l3-mci-tzl-20160526-2154`, `l3-rax-dta-20200818-0454`, `l3-rax-nyf-20200818-0001`, `l3-slc-tv0-20160516-2359`, `l3-tlx-daa-20130520-2016`, `l3-tlx-dhr-20130520-2016`, `l3-tlx-dod-20130520-2016`, `l3-tlx-dpa-20130520-2016`, `l3-tlx-dpr-20130520-2016`, `l3-tlx-dsd-20130520-2016`, `l3-tlx-dsp-20130520-2016`, `l3-tlx-dta-20130520-2016`, `l3-tlx-du3-20130520-2008`, `l3-tlx-dvl-20130520-2016`, `l3-tlx-eet-20130520-2016`, `l3-tlx-gsm-20130520-2100`, `l3-tlx-hhc-20130520-2016`, `l3-tlx-n0c-20130520-2016`, `l3-tlx-n0h-20130520-2016`, `l3-tlx-n0k-20130520-2016`, `l3-tlx-n0m-20130520-2016`, `l3-tlx-n0q-20130520-2016`, `l3-tlx-n0r-20130520-2016`, `l3-tlx-n0s-20130520-2016`, `l3-tlx-n0u-20130520-2016`, `l3-tlx-n0v-20130520-2016`, `l3-tlx-n0x-20130520-2016`, `l3-tlx-n0z-20130520-2016`, `l3-tlx-n1c-20130520-2016`, `l3-tlx-n1h-20130520-2016`, `l3-tlx-n1k-20130520-2016`, `l3-tlx-n1m-20130520-2016`, `l3-tlx-n1p-20130520-2016`, `l3-tlx-n1q-20130520-2016`, `l3-tlx-n1s-20130520-2016`, `l3-tlx-n1u-20130520-2016`, `l3-tlx-n1x-20130520-2016`, `l3-tlx-n2c-20130520-2016`, `l3-tlx-n2h-20130520-2016`, `l3-tlx-n2k-20130520-2016`, `l3-tlx-n2m-20130520-2016`, `l3-tlx-n2q-20130520-2016`, `l3-tlx-n2s-20130520-2016`, `l3-tlx-n2u-20130520-2016`, `l3-tlx-n2x-20130520-2016`, `l3-tlx-n3c-20130520-2016`, `l3-tlx-n3h-20130520-2016`, `l3-tlx-n3k-20130520-2016`, `l3-tlx-n3m-20130520-2016`, `l3-tlx-n3p-20130520-2012`, `l3-tlx-n3q-20130520-2016`, `l3-tlx-n3s-20130520-2016`, `l3-tlx-n3u-20130520-2016`, `l3-tlx-n3x-20130520-2016`, `l3-tlx-nac-20130520-2016`, `l3-tlx-nah-20130520-2016`, `l3-tlx-nak-20130520-2016`, `l3-tlx-nam-20130520-2016`, `l3-tlx-naq-20130520-2016`, `l3-tlx-nau-20130520-2016`, `l3-tlx-nax-20130520-2016`, `l3-tlx-nbc-20130520-2016`, `l3-tlx-nbh-20130520-2016`, `l3-tlx-nbk-20130520-2016`, `l3-tlx-nbm-20130520-2016`, `l3-tlx-nbq-20130520-2016`, `l3-tlx-nbu-20130520-2016`, `l3-tlx-nbx-20130520-2016`, `l3-tlx-nc1-20130520-2354`, `l3-tlx-nc2-20130520-2354`, `l3-tlx-nc3-20130520-2354`, `l3-tlx-nc4-20130520-2354`, `l3-tlx-nc5-20130520-2354`, `l3-tlx-nco-20130520-1816`, `l3-tlx-ncr-20130520-2016`, `l3-tlx-ncz-20130520-2016`, `l3-tlx-net-20130520-2016`, `l3-tlx-nhi-20130520-2016`, `l3-tlx-nhl-20130520-2016`, `l3-tlx-nla-20130520-2016`, `l3-tlx-nll-20130520-2016`, `l3-tlx-nmd-20130520-2016`, `l3-tlx-nml-20130520-2016`, `l3-tlx-nsp-20130520-2016`, `l3-tlx-nss-20130520-2016`, `l3-tlx-nst-20130520-2016`, `l3-tlx-nsw-20130520-2016`, `l3-tlx-ntp-20130520-2016`, `l3-tlx-ntv-20130520-2016`, `l3-tlx-nvl-20130520-2012`, `l3-tlx-nvw-20130520-2016`, `l3-tlx-oha-20130520-2016`, `l3-tlx-pta-20130520-2016`, `l3-tlx-rcm-20130520-2016`, `l3-tlx-rsl-20130520-2358`, `l3-tlx-spd-20130520-2016`
- `provider:ncei-nexrad-l3-gcp` (68): `l3-akq-018-19940810-0835`, `l3-bmx-029-19940914-1621`, `l3-cae-053-19940629-1906`, `l3-cae-n0r-19940629-1906`, `l3-cys-021-19941114-1107`, `l3-ftg-022-19940601-1917`, `l3-ftg-026-19940930-0536`, `l3-ftg-043-19940930-1849`, `l3-ftg-044-19940930-1849`, `l3-ftg-045-19940930-1849`, `l3-ftg-046-19940930-1849`, `l3-ftg-100-19940930-0102`, `l3-ftg-102-19940930-1932`, `l3-ftg-104-19940930-0850`, `l3-ftg-107-19940930-0154`, `l3-ftg-109-19940930-0102`, `l3-fws-dpa-19950517-2304`, `l3-fws-n0r-19950517-2304`, `l3-fws-n1p-19950517-2304`, `l3-fws-ncz-19950517-2304`, `l3-fws-nhi-19950517-1323`, `l3-fws-nme-19950517-2316`, `l3-fws-now-19950517-2304`, `l3-fws-nss-19950517-2304`, `l3-fws-nst-19950517-2304`, `l3-fws-ntp-19950517-2304`, `l3-fws-nvw-19950517-2322`, `l3-fws-nwp-19950517-2304`, `l3-fws-rcm-19950517-2310`, `l3-fws-sup-19950517-2304`, `l3-grr-039-20011011-0631`, `l3-ilx-irm-19960419-2309`, `l3-ilx-ncz-19960419-2320`, `l3-ilx-nhi-19960419-2303`, `l3-ilx-nme-19960419-2303`, `l3-ilx-nst-19960419-2303`, `l3-ilx-ntv-19960419-2303`, `l3-ilx-rcm-19960419-2309`, `l3-ind-016-19940910-0647`, `l3-ind-017-19940910-1104`, `l3-ind-035-19941031-2138`, `l3-ind-042-19940910-1642`, `l3-ind-073-19940910-1555`, `l3-jan-024-19951003-1312`, `l3-lot-050-19941031-1358`, `l3-lot-053-19941106-0246`, `l3-lot-055-19941031-1137`, `l3-lot-084-19931120-0721`, `l3-lot-101-19930824-0005`, `l3-lot-108-19930824-0005`, `l3-lot-irm-19941031-0503`, `l3-lot-n0r-19941106-0246`, `l3-lot-nvw-19931120-0721`, `l3-lzk-ncz-19970301-1912`, `l3-lzk-nme-19970301-2027`, `l3-lzk-ntv-19970301-2027`, `l3-mlb-051-19941116-0335`, `l3-sgf-nme-20030504-2332`, `l3-sgf-ntv-20030504-2352`, `l3-tlx-063-19940308-1930`, `l3-tlx-064-19940308-1930`, `l3-tlx-087-19940308-1939`, `l3-tlx-101-20010503-0007`, `l3-tlx-102-19990504-0052`, `l3-tlx-103-20010503-0007`, `l3-tlx-104-20010503-2355`, `l3-tlx-irm-19940308-1115`, `l3-tlx-ncz-19990503-2316`
- `provider:nci-thredds` (2): `odim-au24-20260610-000300-nci-zip-member`, `odim-au02-20260921-0000-pvol-subset`
- `provider:nd-swc` (3): `ndswc-kxwa-20260924-214316`, `ndswc-kxwa-20260924-214316-head41`, `polling-ndswc-kxwa-dir-list-20260925`
- `provider:nict-jma` (8): `jma-n5-20191012-090000`, `jma-n6-20191012-090000`, `jma-n5-20191012-090000-rs47773`, `jma-n6-20191012-090000-rs47773`, `jma-n5-20260924-210000`, `jma-n6-20260924-210000`, `jma-n5-20260924-210000-rs47937`, `jma-n6-20260924-210000-rs47937`
- `provider:nws-tgftp` (1): `wmo-text-kslc-20251012-0424-hmlslc`
- `provider:open-radar-data` (6): `odim-norst-20170421-0908-pvol`, `cfrad1-dow8-20211011-223602-rhi`, `cfrad1-dow8-20211011-223602-rhi-trim3-classic`, `cfrad1-spol-20080604-002217-sur`, `cfrad2-spol-20080604-002217-sur`, `cfrad2-xradar-dow8-20211011-223602-rhi-r300`
- `provider:opera-ord` (12): `odim-espdg-20260707-1927-pvol-dbzh-vradh`, `odim-iesha-20260305-0115-pvol`, `odim-dkrom-20260820-1130-pvol`, `odim-dkrom-20260820-1130-pvol-h5latest-trim`, `odim-dkrom-20260820-1130-pvol-h5edge-paged-ea`, `odim-dkrom-20260820-1130-pvol-h5edge-len4`, `cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32`, `odim-bejab-20260612-1450-dbzh`, `odim-bejab-20260612-1450-vrad`, `odim-nohur-20260612-1445-dbzh`, `odim-nohur-20260612-1445-th`, `odim-nohur-20260612-1446-vradh`
- `provider:pyart` (5): `cfrad1-xsapr-sgp-20110520-ppi-netcdf4`, `cfrad1-xsapr-sgp-20110520-ppi-netcdf4-user-types`, `cfrad1-xsapr-sgp-20110520-ppi-netcdf4-szip-lzf`, `cfrad1-xsapr-sgp-20110520-ppi-classic`, `cfrad2-xradar-xsapr-sgp-20110520-ppi`
- `provider:smhi` (1): `odim-seang-20260924-2130-qcvol-dataset1-trim`
- `provider:wradlib-data` (2): `odim-bejab-20190606-0000-pvol`, `odim-bewid-20130429-0430-pvol-dbzh-scan1`
- `provider:wrf-model` (4): `wrf-p3-lookup-table-1-v5.4-2momI`, `wrf-p3-lookup-table-1-v5.4-3momI`, `wrf-p3-lookup-table-1-v5.4-2momI-first-block`, `wrf-p3-lookup-table-1-v5.4-3momI-first-block`
- `provider:zenodo` (12): `cfrad1-irene-sr2-20110827-120420-sur-sweeps01`, `cfrad2-radx-irene-sr2-20110827-120420-sur-r30km`, `dorade-noxp-20090501-sweeps-tgz`, `dorade-noxp-20090525-sweeps-tgz`, `dorade-noxp-20090501-190244-ppi`, `dorade-noxp-20090501-190324-ppi`, `dorade-noxp-20090525-203211-sector`, `dorade-dow6-20211230-222139-rhi-head41`, `dorade-noxp-20090610-003210-ppi-head6`, `dorade-noxp-20090610-003222-ppi-head6`, `dorade-noxp-20090610-003226-ppi-head6`, `dorade-noxp-20090610-003210-heads-zip`

#### `quirk:`

- `quirk:720-ray-lowest-sweep` (1): `odim-norst-20170421-0908-pvol`
- `quirk:cfac-corrections` (2): `dorade-n42rf-tm-20181010-123925-air`, `dorade-n42rf-tm-20181010-123925-air-head48`
- `quirk:coarse-azimuth` (2): `dorade-noxp-20090501-190244-ppi`, `dorade-noxp-20090501-190324-ppi`
- `quirk:crosses-midnight` (1): `l3-kbmx-19980416-0006-nvw`
- `quirk:dir-list-size-not-bytes` (1): `polling-ndswc-kxwa-dir-list-20260925`
- `quirk:fractional-elevations` (1): `odim-dkrom-20260820-1130-pvol`
- `quirk:gate-spacing-varies-by-sweep` (1): `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-per-ray-geometry`
- `quirk:repeated-elevations` (2): `jma-n5-20191012-090000-rs47773`, `jma-n5-20260924-210000-rs47937`
- `quirk:rstart-metres` (1): `odim-espdg-20260707-1927-pvol-dbzh-vradh`
- `quirk:seds-edit-history` (2): `dorade-n42rf-ts-20181010-122951-air`, `dorade-n42rf-tm-20181010-123925-air`
- `quirk:staggered-prt` (1): `dorade-cow2-20260521-225514-sur-head24`
- `quirk:transition-rays` (2): `dorade-cow2-20260521-225514-sur-head24`, `dorade-dow6-20211230-222139-rhi-head41`
- `quirk:version-string-h5rd` (4): `odim-imgw-ram-20260711-0015-kdp-max`, `odim-imgw-ram-20260711-0015-phidp-max`, `odim-imgw-ram-20260711-0015-rhohv-max`, `odim-imgw-ram-20260711-0015-zdr-max`
- `quirk:vertical-sweep` (2): `odim-iesha-20260305-0115-pvol`, `cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32`
- `quirk:vrad-not-vradh` (2): `odim-dkrom-20260820-1130-pvol`, `odim-bejab-20260612-1450-vrad`
- `quirk:vradh-fill-offset` (1): `odim-espdg-20260707-1927-pvol-dbzh-vradh`
- `quirk:what-on-dataset` (4): `odim-imgw-ram-20260711-0015-kdp-max`, `odim-imgw-ram-20260711-0015-phidp-max`, `odim-imgw-ram-20260711-0015-rhohv-max`, `odim-imgw-ram-20260711-0015-zdr-max`
- `quirk:wmo-only-source` (4): `odim-imgw-ram-20260711-0015-kdp-max`, `odim-imgw-ram-20260711-0015-phidp-max`, `odim-imgw-ram-20260711-0015-rhohv-max`, `odim-imgw-ram-20260711-0015-zdr-max`
- `quirk:zero-site-coords` (2): `dorade-noxp-20090501-190244-ppi`, `dorade-noxp-20090501-190324-ppi`
- `quirk:zip-local-member-stream` (1): `odim-au24-20260610-000300-nci-zip-member`

#### `rad-block:`

- `rad-block:20` (10): `l2-kvwx-20080415-235337`, `l2-kpah-20080415-235014`, `l2-kdmx-20080525-205148`, `l2-kvnx-20110315-000203`, `l2-ktlx-20130520-201643`, `l2-kgwx-20130601-235640`, `l2-tstl-20230331-230314`, `l2-kdmx-20080525-205148-trim`, `l2-ktlx-20130520-201643-trim`, `l2-tstl-20230331-230314-trim`
- `rad-block:28` (29): `l2-koax-20140616-205305`, `l2-kewx-20160413-022531`, `l2-kdvn-20200810-175718`, `l2-kdvn-20200810-180401`, `l2-kdvn-20200810-181043`, `l2-kdvn-20200810-181724`, `l2-klix-20210829-180425`, `l2-klix-20210829-173117`, `l2-klix-20210829-175748`, `l2-kbox-20220129-150537`, `l2-tjua-20220918-190621`, `l2-kdgx-20230325-010651`, `l2-kmaf-20230331-230843`, `l2-pgua-20230524-030945`, `l2-kmtx-20240301-212827`, `l2-ktlx-20240315-000217`, `l2-ktlx-20240515-000014`, `l2-pahg-20250909-212549`, `l2-kilx-20260418-013553`, `l2-kiwa-20260917-003629`, `l2-koax-20140616-205305-trim`, `l2-kewx-20160413-022531-trim`, `l2-kdvn-20200810-180401-trim`, `l2-klix-20210829-180425-trim`, `l2-kbox-20220129-150537-trim`, `l2-pgua-20230524-030945-trim`, `l2-kmtx-20240301-212827-trim`, `l2-ktlx-20240315-000217-trim`, `l2-kilx-20260418-013553-trim`

#### `radar:`

- `radar:nexrad` (234): `l3-abr-ftm-20110428-1331`, `l3-akc-nc1-20210730-055033`, `l3-akq-018-19940810-0835`, `l3-bmx-029-19940914-1621`, `l3-byx-n0q-20150124-2106`, `l3-cae-053-19940629-1906`, `l3-cae-n0r-19940629-1906`, `l3-cys-021-19941114-1107`, `l3-ddc-gsm-20200817-1000`, `l3-ddc-n0q-20200817-0501`, `l3-ddc-n0q-20200817-0503`, `l3-eax-gsm-20200817-0933`, `l3-eax-n0q-20200817-0401`, `l3-eax-n0q-20200817-0405`, `l3-ffc-n0q-20140407-1805`, `l3-ftg-022-19940601-1917`, `l3-ftg-026-19940930-0536`, `l3-ftg-043-19940930-1849`, `l3-ftg-044-19940930-1849`, `l3-ftg-045-19940930-1849`, `l3-ftg-046-19940930-1849`, `l3-ftg-100-19940930-0102`, `l3-ftg-102-19940930-1932`, `l3-ftg-104-19940930-0850`, `l3-ftg-107-19940930-0154`, `l3-ftg-109-19940930-0102`, `l3-ftg-n0b-20220304-1820`, `l3-fws-dpa-19950517-2304`, `l3-fws-n0r-19950517-2304`, `l3-fws-n1p-19950517-2304`, `l3-fws-ncz-19950517-2304`, `l3-fws-nhi-19950517-1323`, `l3-fws-nme-19950517-2316`, `l3-fws-now-19950517-2304`, `l3-fws-nss-19950517-2304`, `l3-fws-nst-19950517-2304`, `l3-fws-ntp-19950517-2304`, `l3-fws-nvw-19950517-2322`, `l3-fws-nwp-19950517-2304`, `l3-fws-rcm-19950517-2310`, `l3-fws-sup-19950517-2304`, `l3-gjx-n0f-20200817-0551`, `l3-gjx-naf-20200817-0551`, `l3-gjx-nbf-20200817-0551`, `l3-gjx-nxf-20200817-0600`, `l3-gjx-nyq-20220503-005356`, `l3-grr-039-20011011-0631`, `l3-ilx-irm-19960419-2309`, `l3-ilx-ncz-19960419-2320`, `l3-ilx-nhi-19960419-2303`, `l3-ilx-nme-19960419-2303`, `l3-ilx-nst-19960419-2303`, `l3-ilx-ntv-19960419-2303`, `l3-ilx-rcm-19960419-2309`, `l3-ind-016-19940910-0647`, `l3-ind-017-19940910-1104`, `l3-ind-035-19941031-2138`, `l3-ind-042-19940910-1642`, `l3-ind-073-19940910-1555`, `l3-jan-024-19951003-1312`, `l3-lot-050-19941031-1358`, `l3-lot-053-19941106-0246`, `l3-lot-055-19941031-1137`, `l3-lot-084-19931120-0721`, `l3-lot-101-19930824-0005`, `l3-lot-108-19930824-0005`, `l3-lot-irm-19941031-0503`, `l3-lot-n0r-19941106-0246`, `l3-lot-nvw-19931120-0721`, `l3-lzk-h0c-20200814-0417`, `l3-lzk-h0v-20200812-1309`, `l3-lzk-h0w-20200812-1305`, `l3-lzk-h0z-20200812-1318`, `l3-lzk-ncz-19970301-1912`, `l3-lzk-nme-19970301-2027`, `l3-lzk-ntv-19970301-2027`, `l3-mlb-051-19941116-0335`, `l3-rax-dta-20200818-0454`, `l3-rax-nll-20220510-155126`, `l3-rax-nyf-20200818-0001`, `l3-sgf-nme-20030504-2332`, `l3-sgf-ntv-20030504-2352`, `l3-shv-nzq-20220503-005452`, `l3-tlx-063-19940308-1930`, `l3-tlx-064-19940308-1930`, `l3-tlx-087-19940308-1939`, `l3-tlx-101-20010503-0007`, `l3-tlx-102-19990504-0052`, `l3-tlx-103-20010503-0007`, `l3-tlx-104-20010503-2355`, `l3-tlx-daa-20130520-2016`, `l3-tlx-daa-20260622-080623`, `l3-tlx-dhr-20130520-2016`, `l3-tlx-dhr-20260622-080623`, `l3-tlx-dod-20130520-2016`, `l3-tlx-dod-20220503-005231`, `l3-tlx-dpa-20130520-2016`, `l3-tlx-dpa-20260629-173638`, `l3-tlx-dpr-20130520-2016`, `l3-tlx-dpr-20260622-080623`, `l3-tlx-dsd-20130520-2016`, `l3-tlx-dsd-20220503-005231`, `l3-tlx-dsp-20130520-2016`, `l3-tlx-dsp-20260629-173638`, `l3-tlx-dta-20130520-2016`, `l3-tlx-dta-20260622-080623`, `l3-tlx-du3-20130520-2008`, `l3-tlx-du3-20260622-080623`, `l3-tlx-du6-20260622-120608`, `l3-tlx-dvl-20130520-2016`, `l3-tlx-dvl-20260622-080623`, `l3-tlx-eet-20130520-2016`, `l3-tlx-eet-20260622-080623`, `l3-tlx-gsm-20130520-2100`, `l3-tlx-hhc-20130520-2016`, `l3-tlx-hhc-20260622-080623`, `l3-tlx-irm-19940308-1115`, `l3-tlx-n0b-20260622-080623`, `l3-tlx-n0c-20130520-2016`, `l3-tlx-n0c-20260622-080623`, `l3-tlx-n0f-20220502-235926`, `l3-tlx-n0g-20260622-080623`, `l3-tlx-n0h-20130520-2016`, `l3-tlx-n0h-20260622-080623`, `l3-tlx-n0k-20130520-2016`, `l3-tlx-n0k-20260622-080623`, `l3-tlx-n0m-20130520-2016`, `l3-tlx-n0m-20260622-080623`, `l3-tlx-n0q-20130520-2016`, `l3-tlx-n0q-20220503-005231`, `l3-tlx-n0r-20130520-2016`, `l3-tlx-n0r-20220908-131957`, `l3-tlx-n0s-20130520-2016`, `l3-tlx-n0s-20260622-080623`, `l3-tlx-n0u-20130520-2016`, `l3-tlx-n0u-20220503-005231`, `l3-tlx-n0v-20130520-2016`, `l3-tlx-n0v-20220908-131957`, `l3-tlx-n0x-20130520-2016`, `l3-tlx-n0x-20260622-080623`, `l3-tlx-n0z-20130520-2016`, `l3-tlx-n0z-20220908-131957`, `l3-tlx-n1c-20130520-2016`, `l3-tlx-n1h-20130520-2016`, `l3-tlx-n1k-20130520-2016`, `l3-tlx-n1m-20130520-2016`, `l3-tlx-n1p-20130520-2016`, `l3-tlx-n1p-20260629-173638`, `l3-tlx-n1q-20130520-2016`, `l3-tlx-n1s-20130520-2016`, `l3-tlx-n1u-20130520-2016`, `l3-tlx-n1x-20130520-2016`, `l3-tlx-n2c-20130520-2016`, `l3-tlx-n2h-20130520-2016`, `l3-tlx-n2k-20130520-2016`, `l3-tlx-n2m-20130520-2016`, `l3-tlx-n2q-20130520-2016`, `l3-tlx-n2s-20130520-2016`, `l3-tlx-n2u-20130520-2016`, `l3-tlx-n2x-20130520-2016`, `l3-tlx-n3c-20130520-2016`, `l3-tlx-n3h-20130520-2016`, `l3-tlx-n3k-20130520-2016`, `l3-tlx-n3m-20130520-2016`, `l3-tlx-n3p-20130520-2012`, `l3-tlx-n3p-20220503-011226`, `l3-tlx-n3q-20130520-2016`, `l3-tlx-n3s-20130520-2016`, `l3-tlx-n3u-20130520-2016`, `l3-tlx-n3x-20130520-2016`, `l3-tlx-nac-20130520-2016`, `l3-tlx-nah-20130520-2016`, `l3-tlx-nak-20130520-2016`, `l3-tlx-nam-20130520-2016`, `l3-tlx-naq-20130520-2016`, `l3-tlx-nau-20130520-2016`, `l3-tlx-nax-20130520-2016`, `l3-tlx-nbc-20130520-2016`, `l3-tlx-nbh-20130520-2016`, `l3-tlx-nbk-20130520-2016`, `l3-tlx-nbm-20130520-2016`, `l3-tlx-nbq-20130520-2016`, `l3-tlx-nbu-20130520-2016`, `l3-tlx-nbx-20130520-2016`, `l3-tlx-nc1-20130520-2354`, `l3-tlx-nc2-20130520-2354`, `l3-tlx-nc3-20130520-2354`, `l3-tlx-nc4-20130520-2354`, `l3-tlx-nc5-20130520-2354`, `l3-tlx-nco-20130520-1816`, `l3-tlx-ncr-20130520-2016`, `l3-tlx-ncr-20260622-080623`, `l3-tlx-ncz-19990503-2316`, `l3-tlx-ncz-20130520-2016`, `l3-tlx-ncz-20220503-005231`, `l3-tlx-net-20130520-2016`, `l3-tlx-net-20220503-005231`, `l3-tlx-nhi-20130520-2016`, `l3-tlx-nhi-20220503-005231`, `l3-tlx-nhl-20130520-2016`, `l3-tlx-nhl-20220503-005231`, `l3-tlx-nla-20130520-2016`, `l3-tlx-nla-20220503-005231`, `l3-tlx-nll-20130520-2016`, `l3-tlx-nmd-20130520-2016`, `l3-tlx-nmd-20260622-080623`, `l3-tlx-nml-20130520-2016`, `l3-tlx-nml-20220503-005231`, `l3-tlx-nrr-20260622-080623`, `l3-tlx-nsp-20130520-2016`, `l3-tlx-nss-20130520-2016`, `l3-tlx-nss-20220503-005231`, `l3-tlx-nst-20130520-2016`, `l3-tlx-nst-20260622-080623`, `l3-tlx-nsw-20130520-2016`, `l3-tlx-nsw-20220503-005231`, `l3-tlx-ntp-20130520-2016`, `l3-tlx-ntp-20260629-173638`, `l3-tlx-ntv-20130520-2016`, `l3-tlx-ntv-20220503-005231`, `l3-tlx-nvl-20130520-2012`, `l3-tlx-nvl-20260622-080623`, `l3-tlx-nvw-20130520-2016`, `l3-tlx-nvw-20260622-080623`, `l3-tlx-oha-20130520-2016`, `l3-tlx-oha-20260622-080623`, `l3-tlx-pta-20130520-2016`, `l3-tlx-pta-20200501-000023`, `l3-tlx-rcm-20130520-2016`, `l3-tlx-rcm-20220503-004553`, `l3-tlx-rsl-20130520-2358`, `l3-tlx-rsl-20220502-235926`, `l3-tlx-spd-20130520-2016`, `l3-tlx-spd-20220503-005231`
- `radar:tdwr` (43): `l2-tstl-20230331-230314`, `l2-tbwi-20230601-175101-stub`, `l2chunk-tlas-998-20260917-012843-001-s`, `l2chunk-tlas-999-20260917-013443-001-s`, `l2chunk-tlas-3-20260917-015242-001-s`, `l2chunk-tlas-3-20260917-015242-002-i`, `l2chunk-tlas-3-20260917-015242-003-i`, `l2-tstl-20230331-230314-trim`, `l3-den-tz0-20200804-2226`, `l3-den-tz1-20200804-2226`, `l3-den-tz2-20200804-2227`, `l3-jfk-tr0-20210120-154051`, `l3-mci-dhr-20160526-2154`, `l3-mci-dpa-20160526-2154`, `l3-mci-dsp-20160526-2154`, `l3-mci-n1p-20160526-2154`, `l3-mci-ncr-20160526-2154`, `l3-mci-net-20160526-2154`, `l3-mci-nmd-20160526-2154`, `l3-mci-nst-20160526-2154`, `l3-mci-ntp-20160526-2154`, `l3-mci-nvl-20160526-2154`, `l3-mci-nvw-20160526-2154`, `l3-mci-tr0-20160526-2154`, `l3-mci-tr1-20160526-2154`, `l3-mci-tr2-20160526-2154`, `l3-mci-tv0-20160526-2154`, `l3-mci-tv1-20160526-2154`, `l3-mci-tv2-20160526-2154`, `l3-mci-tzl-20160526-2154`, `l3-okc-ncr-20260622-080623`, `l3-okc-net-20220503-005210`, `l3-okc-nhi-20220503-005210`, `l3-okc-nmd-20260622-080640`, `l3-okc-nst-20260622-080640`, `l3-okc-ntv-20220503-005210`, `l3-okc-nvl-20260622-080623`, `l3-okc-nvw-20260622-080623`, `l3-okc-rsl-20220517-085551`, `l3-okc-tv0-20260622-080547`, `l3-okc-tz0-20260622-080547`, `l3-okc-tzl-20260622-080623`, `l3-slc-tv0-20160516-2359`
- `radar:wsr-88d` (117): `l2-ktlx-19910605-162126`, `l2-ktlx-19990503-230052`, `l2-ktlx-19990504-002218`, `l2-ktlx-20030508-221041`, `l2-klix-20050829-130035`, `l2-kvwx-20080415-235337`, `l2-kpah-20080415-235014`, `l2-kdmx-20080525-205148`, `l2-kvnx-20110315-000203`, `l2-ktlx-20130520-201643`, `l2-kgwx-20130601-235640`, `l2-koax-20140616-205305`, `l2-kewx-20160413-022531`, `l2-kdvn-20200810-175718`, `l2-kdvn-20200810-180401`, `l2-kdvn-20200810-181043`, `l2-kdvn-20200810-181724`, `l2-klix-20210829-180425`, `l2-klix-20210829-173117`, `l2-klix-20210829-175748`, `l2-klix-20210829-175748-mdm`, `l2-kbox-20220129-150537`, `l2-tjua-20220918-190621`, `l2-kdgx-20230325-010651`, `l2-kmaf-20230331-230843`, `l2-pgua-20230524-030945`, `l2-kmtx-20240301-212827`, `l2-ktlx-20240315-000217`, `l2-ktlx-20240515-000014`, `l2-pahg-20250909-212549`, `l2-kilx-20260418-013553`, `l2-kiwa-20260917-003629`, `l2chunk-kiwa-307-20260917-003629-001-s`, `l2chunk-kiwa-307-20260917-003629-{002..069}-i`, `l2chunk-kiwa-307-20260917-003629-070-e`, `l2-ktlx-19910605-162126-trim`, `l2-ktlx-19990504-002218-trim`, `l2-ktlx-20030508-221041-trim`, `l2-klix-20050829-130035-trim`, `l2-kdmx-20080525-205148-trim`, `l2-ktlx-20130520-201643-trim`, `l2-koax-20140616-205305-trim`, `l2-kewx-20160413-022531-trim`, `l2-kdvn-20200810-180401-trim`, `l2-klix-20210829-180425-trim`, `l2-kbox-20220129-150537-trim`, `l2-pgua-20230524-030945-trim`, `l2-kmtx-20240301-212827-trim`, `l2-ktlx-20240315-000217-trim`, `l2-kilx-20260418-013553-trim`

#### `radial-status:`

- `radial-status:3` (2): `l2chunk-kiwa-307-20260917-003629-002-i`, `l2chunk-tlas-3-20260917-015242-002-i`
- `radial-status:4` (1): `l2chunk-kiwa-307-20260917-003629-070-e`
- `radial-status:5` (1): `l2chunk-kiwa-307-20260917-003629-068-i`

#### `records:`

- `records:raw` (13): `l2-ktlx-19910605-162126`, `l2-ktlx-19990503-230052`, `l2-ktlx-19990504-002218`, `l2-ktlx-20030508-221041`, `l2-klix-20050829-130035`, `l2-kvwx-20080415-235337`, `l2-kpah-20080415-235014`, `l2-kdmx-20080525-205148`, `l2-kvnx-20110315-000203`, `l2-ktlx-20130520-201643`, `l2-kgwx-20130601-235640`, `l2-koax-20140616-205305`, `l2-kewx-20160413-022531`

#### `regime:`

- `regime:clear-air` (5): `l2-kvwx-20080415-235337`, `l2-kpah-20080415-235014`, `l2-kvnx-20110315-000203`, `l2-kmaf-20230331-230843`, `l2-ktlx-20240515-000014`
- `regime:complex-terrain` (2): `l2-kmtx-20240301-212827`, `l2-kmtx-20240301-212827-trim`
- `regime:convective` (13): `l2-ktlx-19910605-162126`, `l2-kdmx-20080525-205148`, `l2-kgwx-20130601-235640`, `l2-tstl-20230331-230314`, `l2-ktlx-20240315-000217`, `l2-kilx-20260418-013553`, `l2-kiwa-20260917-003629`, `l2-ktlx-19910605-162126-trim`, `l2-kdmx-20080525-205148-trim`, `l2-kewx-20160413-022531-trim`, `l2-tstl-20230331-230314-trim`, `l2-ktlx-20240315-000217-trim`, `l2-kilx-20260418-013553-trim`
- `regime:derecho` (9): `l2-kdvn-20200810-175718`, `l2-kdvn-20200810-180401`, `l2-kdvn-20200810-181043`, `l2-kdvn-20200810-181724`, `l2-kdvn-20200810-180401-trim`, `l3-kdvn-20200810-1757-nst`, `l3-kdvn-20200810-1804-nst`, `l3-kdvn-20200810-1810-nst`, `l3-kdvn-20200810-1817-nst`
- `regime:hail` (1): `l2-kewx-20160413-022531`
- `regime:hurricane` (11): `l2-klix-20050829-130035`, `l2-klix-20210829-180425`, `l2-klix-20210829-173117`, `l2-klix-20210829-175748`, `l2-tjua-20220918-190621`, `l2-klix-20050829-130035-trim`, `cfrad1-irene-sr2-20110827-120420-sur-sweeps01`, `dorade-n42rf-ts-20181010-122951-air`, `dorade-n42rf-ts-20181010-122951-air-head24`, `dorade-n42rf-tm-20181010-123925-air`, `dorade-n42rf-tm-20181010-123925-air-head48`
- `regime:non-conus` (4): `l2-tjua-20220918-190621`, `l2-pgua-20230524-030945`, `l2-pahg-20250909-212549`, `l2-pgua-20230524-030945-trim`
- `regime:snow` (2): `l2-kmtx-20240301-212827`, `l2-kmtx-20240301-212827-trim`
- `regime:stratiform` (1): `l2-pahg-20250909-212549`
- `regime:tornado` (101): `l2-ktlx-19990504-002218`, `l2-ktlx-20030508-221041`, `l2-ktlx-20130520-201643`, `l2-koax-20140616-205305`, `l2-kdgx-20230325-010651`, `l2-ktlx-19990504-002218-trim`, `l2-ktlx-20030508-221041-trim`, `l2-ktlx-20130520-201643-trim`, `l2-koax-20140616-205305-trim`, `l3-tlx-daa-20130520-2016`, `l3-tlx-dhr-20130520-2016`, `l3-tlx-dod-20130520-2016`, `l3-tlx-dpa-20130520-2016`, `l3-tlx-dpr-20130520-2016`, `l3-tlx-dsd-20130520-2016`, `l3-tlx-dsp-20130520-2016`, `l3-tlx-dta-20130520-2016`, `l3-tlx-du3-20130520-2008`, `l3-tlx-dvl-20130520-2016`, `l3-tlx-eet-20130520-2016`, `l3-tlx-gsm-20130520-2100`, `l3-tlx-hhc-20130520-2016`, `l3-tlx-n0c-20130520-2016`, `l3-tlx-n0h-20130520-2016`, `l3-tlx-n0k-20130520-2016`, `l3-tlx-n0m-20130520-2016`, `l3-tlx-n0q-20130520-2016`, `l3-tlx-n0r-20130520-2016`, `l3-tlx-n0s-20130520-2016`, `l3-tlx-n0u-20130520-2016`, `l3-tlx-n0v-20130520-2016`, `l3-tlx-n0x-20130520-2016`, `l3-tlx-n0z-20130520-2016`, `l3-tlx-n1c-20130520-2016`, `l3-tlx-n1h-20130520-2016`, `l3-tlx-n1k-20130520-2016`, `l3-tlx-n1m-20130520-2016`, `l3-tlx-n1p-20130520-2016`, `l3-tlx-n1q-20130520-2016`, `l3-tlx-n1s-20130520-2016`, `l3-tlx-n1u-20130520-2016`, `l3-tlx-n1x-20130520-2016`, `l3-tlx-n2c-20130520-2016`, `l3-tlx-n2h-20130520-2016`, `l3-tlx-n2k-20130520-2016`, `l3-tlx-n2m-20130520-2016`, `l3-tlx-n2q-20130520-2016`, `l3-tlx-n2s-20130520-2016`, `l3-tlx-n2u-20130520-2016`, `l3-tlx-n2x-20130520-2016`, `l3-tlx-n3c-20130520-2016`, `l3-tlx-n3h-20130520-2016`, `l3-tlx-n3k-20130520-2016`, `l3-tlx-n3m-20130520-2016`, `l3-tlx-n3p-20130520-2012`, `l3-tlx-n3q-20130520-2016`, `l3-tlx-n3s-20130520-2016`, `l3-tlx-n3u-20130520-2016`, `l3-tlx-n3x-20130520-2016`, `l3-tlx-nac-20130520-2016`, `l3-tlx-nah-20130520-2016`, `l3-tlx-nak-20130520-2016`, `l3-tlx-nam-20130520-2016`, `l3-tlx-naq-20130520-2016`, `l3-tlx-nau-20130520-2016`, `l3-tlx-nax-20130520-2016`, `l3-tlx-nbc-20130520-2016`, `l3-tlx-nbh-20130520-2016`, `l3-tlx-nbk-20130520-2016`, `l3-tlx-nbm-20130520-2016`, `l3-tlx-nbq-20130520-2016`, `l3-tlx-nbu-20130520-2016`, `l3-tlx-nbx-20130520-2016`, `l3-tlx-nc1-20130520-2354`, `l3-tlx-nc2-20130520-2354`, `l3-tlx-nc3-20130520-2354`, `l3-tlx-nc4-20130520-2354`, `l3-tlx-nc5-20130520-2354`, `l3-tlx-nco-20130520-1816`, `l3-tlx-ncr-20130520-2016`, `l3-tlx-ncz-20130520-2016`, `l3-tlx-net-20130520-2016`, `l3-tlx-nhi-20130520-2016`, `l3-tlx-nhl-20130520-2016`, `l3-tlx-nla-20130520-2016`, `l3-tlx-nll-20130520-2016`, `l3-tlx-nmd-20130520-2016`, `l3-tlx-nml-20130520-2016`, `l3-tlx-nsp-20130520-2016`, `l3-tlx-nss-20130520-2016`, `l3-tlx-nst-20130520-2016`, `l3-tlx-nsw-20130520-2016`, `l3-tlx-ntp-20130520-2016`, `l3-tlx-ntv-20130520-2016`, `l3-tlx-nvl-20130520-2012`, `l3-tlx-nvw-20130520-2016`, `l3-tlx-oha-20130520-2016`, `l3-tlx-pta-20130520-2016`, `l3-tlx-rcm-20130520-2016`, `l3-tlx-rsl-20130520-2358`, `l3-tlx-spd-20130520-2016`
- `regime:typhoon` (5): `l2-pgua-20230524-030945`, `jma-n5-20191012-090000`, `jma-n6-20191012-090000`, `jma-n5-20191012-090000-rs47773`, `jma-n6-20191012-090000-rs47773`
- `regime:wildfire` (1): `dorade-dow6-20211230-222139-rhi-head41`
- `regime:winter-storm` (2): `l2-kbox-20220129-150537`, `l2-kbox-20220129-150537-trim`

#### `replaces:`

- `replaces:cfrad_synth` (1): `cfrad1-irene-sr2-20110827-120420-sur-sweeps01`
- `replaces:odim_pvol_synth` (2): `odim-iesha-20260305-0115-pvol`, `odim-dkrom-20260820-1130-pvol`

#### `res:`

- `res:legacy` (14): `l2-ktlx-19910605-162126`, `l2-ktlx-19990503-230052`, `l2-ktlx-19990504-002218`, `l2-ktlx-20030508-221041`, `l2-klix-20050829-130035`, `l2-kvwx-20080415-235337`, `l2-kpah-20080415-235014`, `l2-kgwx-20130601-235640`, `l2-tstl-20230331-230314`, `l2-ktlx-19910605-162126-trim`, `l2-ktlx-19990504-002218-trim`, `l2-ktlx-20030508-221041-trim`, `l2-klix-20050829-130035-trim`, `l2-tstl-20230331-230314-trim`
- `res:super` (34): `l2-kdmx-20080525-205148`, `l2-kvnx-20110315-000203`, `l2-ktlx-20130520-201643`, `l2-koax-20140616-205305`, `l2-kewx-20160413-022531`, `l2-kdvn-20200810-175718`, `l2-kdvn-20200810-180401`, `l2-kdvn-20200810-181043`, `l2-kdvn-20200810-181724`, `l2-klix-20210829-180425`, `l2-klix-20210829-173117`, `l2-klix-20210829-175748`, `l2-kbox-20220129-150537`, `l2-tjua-20220918-190621`, `l2-kdgx-20230325-010651`, `l2-kmaf-20230331-230843`, `l2-pgua-20230524-030945`, `l2-kmtx-20240301-212827`, `l2-ktlx-20240315-000217`, `l2-ktlx-20240515-000014`, `l2-pahg-20250909-212549`, `l2-kilx-20260418-013553`, `l2-kiwa-20260917-003629`, `l2-kdmx-20080525-205148-trim`, `l2-ktlx-20130520-201643-trim`, `l2-koax-20140616-205305-trim`, `l2-kewx-20160413-022531-trim`, `l2-kdvn-20200810-180401-trim`, `l2-klix-20210829-180425-trim`, `l2-kbox-20220129-150537-trim`, `l2-pgua-20230524-030945-trim`, `l2-kmtx-20240301-212827-trim`, `l2-ktlx-20240315-000217-trim`, `l2-kilx-20260418-013553-trim`

#### `scan:`

- `scan:air` (4): `dorade-n42rf-ts-20181010-122951-air`, `dorade-n42rf-ts-20181010-122951-air-head24`, `dorade-n42rf-tm-20181010-123925-air`, `dorade-n42rf-tm-20181010-123925-air-head48`
- `scan:ppi` (36): `odim-bejab-20190606-0000-pvol`, `odim-bewid-20130429-0430-pvol-dbzh-scan1`, `odim-norst-20170421-0908-pvol`, `odim-espdg-20260707-1927-pvol-dbzh-vradh`, `odim-iesha-20260305-0115-pvol`, `odim-dkrom-20260820-1130-pvol`, `odim-dkrom-20260820-1130-pvol-h5latest-trim`, `odim-dkrom-20260820-1130-pvol-h5edge-paged-ea`, `odim-dkrom-20260820-1130-pvol-h5edge-len4`, `odim-seang-20260924-2130-qcvol-dataset1-trim`, `odim-fianj-20260924-2130-pvol-dataset1-trim`, `odim-deboo-20260924-2130-sweep-th-00`, `odim-itdes-20260924-2135-pvol-class`, `cfrad1-xsapr-sgp-20110520-ppi-netcdf4`, `cfrad1-xsapr-sgp-20110520-ppi-netcdf4-user-types`, `cfrad1-xsapr-sgp-20110520-ppi-netcdf4-szip-lzf`, `cfrad1-xsapr-sgp-20110520-ppi-classic`, `cfrad1-irene-sr2-20110827-120420-sur-sweeps01`, `cfrad1-spol-20080604-002217-sur`, `cfrad2-spol-20080604-002217-sur`, `cfrad2-radx-irene-sr2-20110827-120420-sur-r30km`, `cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32`, `cfrad2-xradar-xsapr-sgp-20110520-ppi`, `dorade-noxp-20090501-190244-ppi`, `dorade-noxp-20090501-190324-ppi`, `dorade-noxp-20090610-003210-ppi-head6`, `dorade-noxp-20090610-003222-ppi-head6`, `dorade-noxp-20090610-003226-ppi-head6`, `odim-bejab-20260612-1450-dbzh`, `odim-bejab-20260612-1450-vrad`, `odim-nohur-20260612-1445-dbzh`, `odim-nohur-20260612-1445-th`, `odim-nohur-20260612-1446-vradh`, `odim-au02-20260921-0000-pvol-subset`, `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-per-ray-geometry`, `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-finest-geometry-netcdf4`
- `scan:rhi` (4): `cfrad1-dow8-20211011-223602-rhi`, `cfrad1-dow8-20211011-223602-rhi-trim3-classic`, `cfrad2-xradar-dow8-20211011-223602-rhi-r300`, `dorade-dow6-20211230-222139-rhi-head41`
- `scan:sails` (1): `l3-ktlx-20260622-080806-n0b-sails`
- `scan:sector` (1): `dorade-noxp-20090525-203211-sector`
- `scan:sur` (1): `dorade-cow2-20260521-225514-sur-head24`
- `scan:vertical-pointing` (3): `odim-nohur-20260612-1445-dbzh`, `odim-nohur-20260612-1445-th`, `odim-nohur-20260612-1446-vradh`

#### `segmented:`

- `segmented:13` (2): `l2-klix-20050829-130035`, `l2-klix-20050829-130035-trim`
- `segmented:15` (3): `l2-klix-20050829-130035`, `l2-kpah-20080415-235014`, `l2-klix-20050829-130035-trim`

#### `sequence:`

- `sequence:kdvn-20200810` (8): `l2-kdvn-20200810-175718`, `l2-kdvn-20200810-180401`, `l2-kdvn-20200810-181043`, `l2-kdvn-20200810-181724`, `l3-kdvn-20200810-1757-nst`, `l3-kdvn-20200810-1804-nst`, `l3-kdvn-20200810-1810-nst`, `l3-kdvn-20200810-1817-nst`

#### `site:`

- `site:au02` (1): `odim-au02-20260921-0000-pvol-subset`
- `site:au24` (1): `odim-au24-20260610-000300-nci-zip-member`
- `site:bejab` (3): `odim-bejab-20190606-0000-pvol`, `odim-bejab-20260612-1450-dbzh`, `odim-bejab-20260612-1450-vrad`
- `site:bewid` (1): `odim-bewid-20130429-0430-pvol-dbzh-scan1`
- `site:cow2` (1): `dorade-cow2-20260521-225514-sur-head24`
- `site:deboo` (1): `odim-deboo-20260924-2130-sweep-th-00`
- `site:dkrom` (4): `odim-dkrom-20260820-1130-pvol`, `odim-dkrom-20260820-1130-pvol-h5latest-trim`, `odim-dkrom-20260820-1130-pvol-h5edge-paged-ea`, `odim-dkrom-20260820-1130-pvol-h5edge-len4`
- `site:dow6` (1): `dorade-dow6-20211230-222139-rhi-head41`
- `site:dow8` (3): `cfrad1-dow8-20211011-223602-rhi`, `cfrad1-dow8-20211011-223602-rhi-trim3-classic`, `cfrad2-xradar-dow8-20211011-223602-rhi-r300`
- `site:espdg` (1): `odim-espdg-20260707-1927-pvol-dbzh-vradh`
- `site:fianj` (3): `odim-fianj-20260924-2130-pvol-dataset1-trim`, `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-per-ray-geometry`, `cfrad1-radx-fianj-20260924-2130-sweeps1-2-7-finest-geometry-netcdf4`
- `site:iesha` (2): `odim-iesha-20260305-0115-pvol`, `cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32`
- `site:itdes` (1): `odim-itdes-20260924-2135-pvol-class`
- `site:itok` (2): `jma-n5-20260924-210000-rs47937`, `jma-n6-20260924-210000-rs47937`
- `site:kbmx` (2): `l3-kbmx-19980416-archive-tarz`, `l3-kbmx-19980416-0006-nvw`
- `site:kbox` (2): `l2-kbox-20220129-150537`, `l2-kbox-20220129-150537-trim`
- `site:kdgx` (1): `l2-kdgx-20230325-010651`
- `site:kdmx` (2): `l2-kdmx-20080525-205148`, `l2-kdmx-20080525-205148-trim`
- `site:kdvn` (9): `l2-kdvn-20200810-175718`, `l2-kdvn-20200810-180401`, `l2-kdvn-20200810-181043`, `l2-kdvn-20200810-181724`, `l2-kdvn-20200810-180401-trim`, `l3-kdvn-20200810-1757-nst`, `l3-kdvn-20200810-1804-nst`, `l3-kdvn-20200810-1810-nst`, `l3-kdvn-20200810-1817-nst`
- `site:kewx` (2): `l2-kewx-20160413-022531`, `l2-kewx-20160413-022531-trim`
- `site:kgwx` (1): `l2-kgwx-20130601-235640`
- `site:kilx` (2): `l2-kilx-20260418-013553`, `l2-kilx-20260418-013553-trim`
- `site:kiwa` (71): `l2-kiwa-20260917-003629`, `l2chunk-kiwa-307-20260917-003629-001-s`, `l2chunk-kiwa-307-20260917-003629-{002..069}-i`, `l2chunk-kiwa-307-20260917-003629-070-e`
- `site:klix` (7): `l2-klix-20050829-130035`, `l2-klix-20210829-180425`, `l2-klix-20210829-173117`, `l2-klix-20210829-175748`, `l2-klix-20210829-175748-mdm`, `l2-klix-20050829-130035-trim`, `l2-klix-20210829-180425-trim`
- `site:kmaf` (1): `l2-kmaf-20230331-230843`
- `site:kmtx` (2): `l2-kmtx-20240301-212827`, `l2-kmtx-20240301-212827-trim`
- `site:knqa` (2): `l3-knqa-20080205-archive-tarz`, `l3-knqa-20080205-0018-rob`
- `site:koax` (2): `l2-koax-20140616-205305`, `l2-koax-20140616-205305-trim`
- `site:kpah` (1): `l2-kpah-20080415-235014`
- `site:kslc` (1): `wmo-text-kslc-20251012-0424-hmlslc`
- `site:ktlx` (13): `l2-ktlx-19910605-162126`, `l2-ktlx-19990503-230052`, `l2-ktlx-19990504-002218`, `l2-ktlx-20030508-221041`, `l2-ktlx-20130520-201643`, `l2-ktlx-20240315-000217`, `l2-ktlx-20240515-000014`, `l2-ktlx-19910605-162126-trim`, `l2-ktlx-19990504-002218-trim`, `l2-ktlx-20030508-221041-trim`, `l2-ktlx-20130520-201643-trim`, `l2-ktlx-20240315-000217-trim`, `l3-ktlx-20260622-080806-n0b-sails`
- `site:kvnx` (1): `l2-kvnx-20110315-000203`
- `site:kvwx` (1): `l2-kvwx-20080415-235337`
- `site:kxwa` (3): `ndswc-kxwa-20260924-214316`, `ndswc-kxwa-20260924-214316-head41`, `polling-ndswc-kxwa-dir-list-20260925`
- `site:lare` (1): `polling-ewr-laredo-grlevel2-cfg-20260925`
- `site:n42rf` (4): `dorade-n42rf-ts-20181010-122951-air`, `dorade-n42rf-ts-20181010-122951-air-head24`, `dorade-n42rf-tm-20181010-123925-air`, `dorade-n42rf-tm-20181010-123925-air-head48`
- `site:nohur` (3): `odim-nohur-20260612-1445-dbzh`, `odim-nohur-20260612-1445-th`, `odim-nohur-20260612-1446-vradh`
- `site:norst` (1): `odim-norst-20170421-0908-pvol`
- `site:noxp` (9): `dorade-noxp-20090501-sweeps-tgz`, `dorade-noxp-20090525-sweeps-tgz`, `dorade-noxp-20090501-190244-ppi`, `dorade-noxp-20090501-190324-ppi`, `dorade-noxp-20090525-203211-sector`, `dorade-noxp-20090610-003210-ppi-head6`, `dorade-noxp-20090610-003222-ppi-head6`, `dorade-noxp-20090610-003226-ppi-head6`, `dorade-noxp-20090610-003210-heads-zip`
- `site:pahg` (1): `l2-pahg-20250909-212549`
- `site:pgua` (2): `l2-pgua-20230524-030945`, `l2-pgua-20230524-030945-trim`
- `site:ram` (4): `odim-imgw-ram-20260711-0015-kdp-max`, `odim-imgw-ram-20260711-0015-phidp-max`, `odim-imgw-ram-20260711-0015-rhohv-max`, `odim-imgw-ram-20260711-0015-zdr-max`
- `site:seang` (1): `odim-seang-20260924-2130-qcvol-dataset1-trim`
- `site:smart-r2` (2): `cfrad1-irene-sr2-20110827-120420-sur-sweeps01`, `cfrad2-radx-irene-sr2-20110827-120420-sur-r30km`
- `site:spol` (2): `cfrad1-spol-20080604-002217-sur`, `cfrad2-spol-20080604-002217-sur`
- `site:taka` (2): `jma-n5-20191012-090000-rs47773`, `jma-n6-20191012-090000-rs47773`
- `site:tbwi` (1): `l2-tbwi-20230601-175101-stub`
- `site:tjua` (1): `l2-tjua-20220918-190621`
- `site:tlas` (5): `l2chunk-tlas-998-20260917-012843-001-s`, `l2chunk-tlas-999-20260917-013443-001-s`, `l2chunk-tlas-3-20260917-015242-001-s`, `l2chunk-tlas-3-20260917-015242-002-i`, `l2chunk-tlas-3-20260917-015242-003-i`
- `site:tstl` (2): `l2-tstl-20230331-230314`, `l2-tstl-20230331-230314-trim`
- `site:xsapr-sgp` (5): `cfrad1-xsapr-sgp-20110520-ppi-netcdf4`, `cfrad1-xsapr-sgp-20110520-ppi-netcdf4-user-types`, `cfrad1-xsapr-sgp-20110520-ppi-netcdf4-szip-lzf`, `cfrad1-xsapr-sgp-20110520-ppi-classic`, `cfrad2-xradar-xsapr-sgp-20110520-ppi`

#### `split-scan:`

- `split-scan:bejab-20260612-1450-doppler` (2): `odim-bejab-20260612-1450-dbzh`, `odim-bejab-20260612-1450-vrad`
- `split-scan:nohur-20260612-1445` (3): `odim-nohur-20260612-1445-dbzh`, `odim-nohur-20260612-1445-th`, `odim-nohur-20260612-1446-vradh`

#### `status:`

- `status:research-only-unvalidated` (5): `tmatrix-lut-rain-sband-pytmatrix-0.3.3`, `tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3`, `tmatrix-lut-property-dry-oblate-sband-trim`, `tmatrix-lut-property-wet-oblate-sband-trim`, `tmatrix-lut-property-rain-sband-trim`

#### `sweepset:`

- `sweepset:noxp-20090501` (2): `dorade-noxp-20090501-190244-ppi`, `dorade-noxp-20090501-190324-ppi`
- `sweepset:noxp-20090610-003210` (4): `dorade-noxp-20090610-003210-ppi-head6`, `dorade-noxp-20090610-003222-ppi-head6`, `dorade-noxp-20090610-003226-ppi-head6`, `dorade-noxp-20090610-003210-heads-zip`

#### `table:`

- `table:conventional-dry-ice-spheroids` (3): `tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3`, `tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3-config`, `tmatrix-lut-dry-ice-sband-pytmatrix-0.3.3-manifest`
- `table:conventional-liquid-rain` (3): `tmatrix-lut-rain-sband-pytmatrix-0.3.3`, `tmatrix-lut-rain-sband-pytmatrix-0.3.3-config`, `tmatrix-lut-rain-sband-pytmatrix-0.3.3-manifest`
- `table:property-p3-ishmael-dry-oblate` (3): `tmatrix-lut-property-dry-oblate-sband-trim`, `tmatrix-lut-property-dry-oblate-sband-trim-config`, `tmatrix-lut-property-dry-oblate-sband-trim-trim`
- `table:property-p3-ishmael-wet-oblate` (3): `tmatrix-lut-property-wet-oblate-sband-trim`, `tmatrix-lut-property-wet-oblate-sband-trim-config`, `tmatrix-lut-property-wet-oblate-sband-trim-trim`
- `table:property-rain` (3): `tmatrix-lut-property-rain-sband-trim`, `tmatrix-lut-property-rain-sband-trim-config`, `tmatrix-lut-property-rain-sband-trim-trim`

#### `validation:`

- `validation:held-out-interpolation` (1): `tmatrix-held-out-interpolation-report-v10`
- `validation:held-out-nodes` (1): `tmatrix-held-out-nodes-v10`

#### `vcp:`

- `vcp:11` (15): `l2-ktlx-19990503-230052`, `l2-ktlx-19990504-002218`, `l2-ktlx-20030508-221041`, `l2-ktlx-19990504-002218-trim`, `l2-ktlx-20030508-221041-trim`, `l3-lzk-nme-19970301-2027`, `l3-lzk-ntv-19970301-2027`, `l3-mlb-051-19941116-0335`, `l3-sgf-nme-20030504-2332`, `l3-sgf-ntv-20030504-2352`, `l3-tlx-101-20010503-0007`, `l3-tlx-102-19990504-0052`, `l3-tlx-103-20010503-0007`, `l3-tlx-104-20010503-2355`, `l3-tlx-ncz-19990503-2316`
- `vcp:12` (87): `l2-ktlx-20130520-201643`, `l2-ktlx-20130520-201643-trim`, `l3-tlx-daa-20130520-2016`, `l3-tlx-dhr-20130520-2016`, `l3-tlx-dod-20130520-2016`, `l3-tlx-dpa-20130520-2016`, `l3-tlx-dpr-20130520-2016`, `l3-tlx-dsd-20130520-2016`, `l3-tlx-dsp-20130520-2016`, `l3-tlx-dta-20130520-2016`, `l3-tlx-du3-20130520-2008`, `l3-tlx-dvl-20130520-2016`, `l3-tlx-eet-20130520-2016`, `l3-tlx-hhc-20130520-2016`, `l3-tlx-n0c-20130520-2016`, `l3-tlx-n0h-20130520-2016`, `l3-tlx-n0k-20130520-2016`, `l3-tlx-n0m-20130520-2016`, `l3-tlx-n0q-20130520-2016`, `l3-tlx-n0r-20130520-2016`, `l3-tlx-n0s-20130520-2016`, `l3-tlx-n0u-20130520-2016`, `l3-tlx-n0v-20130520-2016`, `l3-tlx-n0x-20130520-2016`, `l3-tlx-n0z-20130520-2016`, `l3-tlx-n1c-20130520-2016`, `l3-tlx-n1h-20130520-2016`, `l3-tlx-n1k-20130520-2016`, `l3-tlx-n1m-20130520-2016`, `l3-tlx-n1p-20130520-2016`, `l3-tlx-n1q-20130520-2016`, `l3-tlx-n1s-20130520-2016`, `l3-tlx-n1u-20130520-2016`, `l3-tlx-n1x-20130520-2016`, `l3-tlx-n2c-20130520-2016`, `l3-tlx-n2h-20130520-2016`, `l3-tlx-n2k-20130520-2016`, `l3-tlx-n2m-20130520-2016`, `l3-tlx-n2q-20130520-2016`, `l3-tlx-n2s-20130520-2016`, `l3-tlx-n2u-20130520-2016`, `l3-tlx-n2x-20130520-2016`, `l3-tlx-n3c-20130520-2016`, `l3-tlx-n3h-20130520-2016`, `l3-tlx-n3k-20130520-2016`, `l3-tlx-n3m-20130520-2016`, `l3-tlx-n3p-20130520-2012`, `l3-tlx-n3q-20130520-2016`, `l3-tlx-n3s-20130520-2016`, `l3-tlx-n3u-20130520-2016`, `l3-tlx-n3x-20130520-2016`, `l3-tlx-nac-20130520-2016`, `l3-tlx-nah-20130520-2016`, `l3-tlx-nak-20130520-2016`, `l3-tlx-nam-20130520-2016`, `l3-tlx-naq-20130520-2016`, `l3-tlx-nau-20130520-2016`, `l3-tlx-nax-20130520-2016`, `l3-tlx-nbc-20130520-2016`, `l3-tlx-nbh-20130520-2016`, `l3-tlx-nbk-20130520-2016`, `l3-tlx-nbm-20130520-2016`, `l3-tlx-nbq-20130520-2016`, `l3-tlx-nbu-20130520-2016`, `l3-tlx-nbx-20130520-2016`, `l3-tlx-ncr-20130520-2016`, `l3-tlx-ncz-20130520-2016`, `l3-tlx-net-20130520-2016`, `l3-tlx-nhi-20130520-2016`, `l3-tlx-nhl-20130520-2016`, `l3-tlx-nla-20130520-2016`, `l3-tlx-nll-20130520-2016`, `l3-tlx-nmd-20130520-2016`, `l3-tlx-nml-20130520-2016`, `l3-tlx-nsp-20130520-2016`, `l3-tlx-nss-20130520-2016`, `l3-tlx-nst-20130520-2016`, `l3-tlx-nsw-20130520-2016`, `l3-tlx-ntp-20130520-2016`, `l3-tlx-ntv-20130520-2016`, `l3-tlx-nvl-20130520-2012`, `l3-tlx-nvw-20130520-2016`, `l3-tlx-oha-20130520-2016`, `l3-tlx-pta-20130520-2016`, `l3-tlx-rcm-20130520-2016`, `l3-tlx-rsl-20130520-2358`, `l3-tlx-spd-20130520-2016`
- `vcp:21` (56): `l2-ktlx-19910605-162126`, `l2-ktlx-19910605-162126-trim`, `l3-cae-053-19940629-1906`, `l3-cae-n0r-19940629-1906`, `l3-cys-021-19941114-1107`, `l3-ffc-n0q-20140407-1805`, `l3-ftg-026-19940930-0536`, `l3-ftg-043-19940930-1849`, `l3-ftg-044-19940930-1849`, `l3-ftg-045-19940930-1849`, `l3-ftg-046-19940930-1849`, `l3-ftg-100-19940930-0102`, `l3-ftg-102-19940930-1932`, `l3-ftg-104-19940930-0850`, `l3-ftg-107-19940930-0154`, `l3-ftg-109-19940930-0102`, `l3-fws-dpa-19950517-2304`, `l3-fws-n0r-19950517-2304`, `l3-fws-n1p-19950517-2304`, `l3-fws-ncz-19950517-2304`, `l3-fws-nhi-19950517-1323`, `l3-fws-nme-19950517-2316`, `l3-fws-now-19950517-2304`, `l3-fws-nss-19950517-2304`, `l3-fws-nst-19950517-2304`, `l3-fws-ntp-19950517-2304`, `l3-fws-nvw-19950517-2322`, `l3-fws-nwp-19950517-2304`, `l3-fws-rcm-19950517-2310`, `l3-fws-sup-19950517-2304`, `l3-grr-039-20011011-0631`, `l3-ilx-irm-19960419-2309`, `l3-ilx-ncz-19960419-2320`, `l3-ilx-nhi-19960419-2303`, `l3-ilx-nme-19960419-2303`, `l3-ilx-nst-19960419-2303`, `l3-ilx-ntv-19960419-2303`, `l3-ilx-rcm-19960419-2309`, `l3-ind-016-19940910-0647`, `l3-ind-017-19940910-1104`, `l3-ind-035-19941031-2138`, `l3-ind-042-19940910-1642`, `l3-ind-073-19940910-1555`, `l3-jan-024-19951003-1312`, `l3-lot-050-19941031-1358`, `l3-lot-053-19941106-0246`, `l3-lot-055-19941031-1137`, `l3-lot-101-19930824-0005`, `l3-lot-108-19930824-0005`, `l3-lot-irm-19941031-0503`, `l3-lot-n0r-19941106-0246`, `l3-lzk-ncz-19970301-1912`, `l3-tlx-063-19940308-1930`, `l3-tlx-064-19940308-1930`, `l3-tlx-087-19940308-1939`, `l3-tlx-irm-19940308-1115`
- `vcp:31` (2): `l2-kmaf-20230331-230843`, `l3-akq-018-19940810-0835`
- `vcp:32` (9): `l2-kvwx-20080415-235337`, `l2-kpah-20080415-235014`, `l2-kvnx-20110315-000203`, `l3-bmx-029-19940914-1621`, `l3-ftg-022-19940601-1917`, `l3-lot-084-19931120-0721`, `l3-lot-nvw-19931120-0721`, `l3-tlx-nco-20130520-1816`, `l3-tlx-pta-20200501-000023`
- `vcp:35` (15): `l2-ktlx-20240515-000014`, `l3-akc-nc1-20210730-055033`, `l3-gjx-n0f-20200817-0551`, `l3-gjx-naf-20200817-0551`, `l3-gjx-nbf-20200817-0551`, `l3-gjx-nxf-20200817-0600`, `l3-lzk-h0c-20200814-0417`, `l3-shv-nzq-20220503-005452`, `l3-tlx-dpa-20260629-173638`, `l3-tlx-dsp-20260629-173638`, `l3-tlx-n0r-20220908-131957`, `l3-tlx-n0v-20220908-131957`, `l3-tlx-n0z-20220908-131957`, `l3-tlx-n1p-20260629-173638`, `l3-tlx-ntp-20260629-173638`
- `vcp:80` (36): `l2-tstl-20230331-230314`, `l2-tstl-20230331-230314-trim`, `l3-den-tz0-20200804-2226`, `l3-den-tz1-20200804-2226`, `l3-den-tz2-20200804-2227`, `l3-mci-dhr-20160526-2154`, `l3-mci-dpa-20160526-2154`, `l3-mci-dsp-20160526-2154`, `l3-mci-n1p-20160526-2154`, `l3-mci-ncr-20160526-2154`, `l3-mci-net-20160526-2154`, `l3-mci-nmd-20160526-2154`, `l3-mci-nst-20160526-2154`, `l3-mci-ntp-20160526-2154`, `l3-mci-nvl-20160526-2154`, `l3-mci-nvw-20160526-2154`, `l3-mci-tr0-20160526-2154`, `l3-mci-tr1-20160526-2154`, `l3-mci-tr2-20160526-2154`, `l3-mci-tv0-20160526-2154`, `l3-mci-tv1-20160526-2154`, `l3-mci-tv2-20160526-2154`, `l3-mci-tzl-20160526-2154`, `l3-okc-ncr-20260622-080623`, `l3-okc-net-20220503-005210`, `l3-okc-nhi-20220503-005210`, `l3-okc-nmd-20260622-080640`, `l3-okc-nst-20260622-080640`, `l3-okc-ntv-20220503-005210`, `l3-okc-nvl-20260622-080623`, `l3-okc-nvw-20260622-080623`, `l3-okc-rsl-20220517-085551`, `l3-okc-tv0-20260622-080547`, `l3-okc-tz0-20260622-080547`, `l3-okc-tzl-20260622-080623`, `l3-slc-tv0-20160516-2359`
- `vcp:90` (6): `l2chunk-tlas-998-20260917-012843-001-s`, `l2chunk-tlas-999-20260917-013443-001-s`, `l2chunk-tlas-3-20260917-015242-001-s`, `l2chunk-tlas-3-20260917-015242-002-i`, `l2chunk-tlas-3-20260917-015242-003-i`, `l3-jfk-tr0-20210120-154051`
- `vcp:112` (4): `l2-klix-20210829-180425`, `l2-klix-20210829-173117`, `l2-klix-20210829-175748`, `l2-klix-20210829-180425-trim`
- `vcp:121` (2): `l2-klix-20050829-130035`, `l2-klix-20050829-130035-trim`
- `vcp:212` (75): `l2-kdmx-20080525-205148`, `l2-kgwx-20130601-235640`, `l2-koax-20140616-205305`, `l2-kewx-20160413-022531`, `l2-kdvn-20200810-175718`, `l2-kdvn-20200810-180401`, `l2-kdvn-20200810-181043`, `l2-kdvn-20200810-181724`, `l2-kdgx-20230325-010651`, `l2-pgua-20230524-030945`, `l2-ktlx-20240315-000217`, `l2-kilx-20260418-013553`, `l2-kdmx-20080525-205148-trim`, `l2-koax-20140616-205305-trim`, `l2-kewx-20160413-022531-trim`, `l2-kdvn-20200810-180401-trim`, `l2-pgua-20230524-030945-trim`, `l2-ktlx-20240315-000217-trim`, `l2-kilx-20260418-013553-trim`, `l3-byx-n0q-20150124-2106`, `l3-ddc-n0q-20200817-0501`, `l3-ddc-n0q-20200817-0503`, `l3-lzk-h0v-20200812-1309`, `l3-lzk-h0w-20200812-1305`, `l3-lzk-h0z-20200812-1318`, `l3-rax-nll-20220510-155126`, `l3-rax-nyf-20200818-0001`, `l3-tlx-daa-20260622-080623`, `l3-tlx-dhr-20260622-080623`, `l3-tlx-dod-20220503-005231`, `l3-tlx-dpr-20260622-080623`, `l3-tlx-dsd-20220503-005231`, `l3-tlx-dta-20260622-080623`, `l3-tlx-du3-20260622-080623`, `l3-tlx-du6-20260622-120608`, `l3-tlx-dvl-20260622-080623`, `l3-tlx-eet-20260622-080623`, `l3-tlx-hhc-20260622-080623`, `l3-tlx-n0b-20260622-080623`, `l3-tlx-n0c-20260622-080623`, `l3-tlx-n0f-20220502-235926`, `l3-tlx-n0g-20260622-080623`, `l3-tlx-n0h-20260622-080623`, `l3-tlx-n0k-20260622-080623`, `l3-tlx-n0m-20260622-080623`, `l3-tlx-n0q-20220503-005231`, `l3-tlx-n0s-20260622-080623`, `l3-tlx-n0u-20220503-005231`, `l3-tlx-n0x-20260622-080623`, `l3-tlx-n3p-20220503-011226`, `l3-tlx-nc1-20130520-2354`, `l3-tlx-nc2-20130520-2354`, `l3-tlx-nc3-20130520-2354`, `l3-tlx-nc4-20130520-2354`, `l3-tlx-nc5-20130520-2354`, `l3-tlx-ncr-20260622-080623`, `l3-tlx-ncz-20220503-005231`, `l3-tlx-net-20220503-005231`, `l3-tlx-nhi-20220503-005231`, `l3-tlx-nhl-20220503-005231`, `l3-tlx-nla-20220503-005231`, `l3-tlx-nmd-20260622-080623`, `l3-tlx-nml-20220503-005231`, `l3-tlx-nrr-20260622-080623`, `l3-tlx-nss-20220503-005231`, `l3-tlx-nst-20260622-080623`, `l3-tlx-nsw-20220503-005231`, `l3-tlx-ntv-20220503-005231`, `l3-tlx-nvl-20260622-080623`, `l3-tlx-nvw-20260622-080623`, `l3-tlx-oha-20260622-080623`, `l3-tlx-rcm-20220503-004553`, `l3-tlx-rsl-20220502-235926`, `l3-tlx-spd-20220503-005231`, `l3-ktlx-20260622-080806-n0b-sails`
- `vcp:215` (82): `l2-kbox-20220129-150537`, `l2-tjua-20220918-190621`, `l2-kmtx-20240301-212827`, `l2-pahg-20250909-212549`, `l2-kiwa-20260917-003629`, `l2chunk-kiwa-307-20260917-003629-001-s`, `l2chunk-kiwa-307-20260917-003629-{002..069}-i`, `l2chunk-kiwa-307-20260917-003629-070-e`, `l2-kbox-20220129-150537-trim`, `l2-kmtx-20240301-212827-trim`, `l3-eax-n0q-20200817-0401`, `l3-eax-n0q-20200817-0405`, `l3-ftg-n0b-20220304-1820`, `l3-gjx-nyq-20220503-005356`, `l3-rax-dta-20200818-0454`

#### `version:`

- `version:0` (197): `l3-akq-018-19940810-0835`, `l3-bmx-029-19940914-1621`, `l3-byx-n0q-20150124-2106`, `l3-cae-053-19940629-1906`, `l3-cae-n0r-19940629-1906`, `l3-cys-021-19941114-1107`, `l3-ddc-n0q-20200817-0501`, `l3-ddc-n0q-20200817-0503`, `l3-den-tz0-20200804-2226`, `l3-den-tz1-20200804-2226`, `l3-den-tz2-20200804-2227`, `l3-eax-n0q-20200817-0401`, `l3-eax-n0q-20200817-0405`, `l3-ffc-n0q-20140407-1805`, `l3-ftg-022-19940601-1917`, `l3-ftg-026-19940930-0536`, `l3-ftg-043-19940930-1849`, `l3-ftg-044-19940930-1849`, `l3-ftg-045-19940930-1849`, `l3-ftg-046-19940930-1849`, `l3-ftg-100-19940930-0102`, `l3-ftg-102-19940930-1932`, `l3-ftg-104-19940930-0850`, `l3-ftg-107-19940930-0154`, `l3-ftg-109-19940930-0102`, `l3-ftg-n0b-20220304-1820`, `l3-fws-dpa-19950517-2304`, `l3-fws-n0r-19950517-2304`, `l3-fws-n1p-19950517-2304`, `l3-fws-ncz-19950517-2304`, `l3-fws-nhi-19950517-1323`, `l3-fws-nme-19950517-2316`, `l3-fws-now-19950517-2304`, `l3-fws-nss-19950517-2304`, `l3-fws-nst-19950517-2304`, `l3-fws-ntp-19950517-2304`, `l3-fws-nvw-19950517-2322`, `l3-fws-nwp-19950517-2304`, `l3-fws-rcm-19950517-2310`, `l3-fws-sup-19950517-2304`, `l3-gjx-nyq-20220503-005356`, `l3-grr-039-20011011-0631`, `l3-ilx-irm-19960419-2309`, `l3-ilx-ncz-19960419-2320`, `l3-ilx-nhi-19960419-2303`, `l3-ilx-nme-19960419-2303`, `l3-ilx-nst-19960419-2303`, `l3-ilx-ntv-19960419-2303`, `l3-ilx-rcm-19960419-2309`, `l3-ind-016-19940910-0647`, `l3-ind-017-19940910-1104`, `l3-ind-035-19941031-2138`, `l3-ind-042-19940910-1642`, `l3-ind-073-19940910-1555`, `l3-jan-024-19951003-1312`, `l3-jfk-tr0-20210120-154051`, `l3-lot-050-19941031-1358`, `l3-lot-053-19941106-0246`, `l3-lot-055-19941031-1137`, `l3-lot-084-19931120-0721`, `l3-lot-101-19930824-0005`, `l3-lot-108-19930824-0005`, `l3-lot-irm-19941031-0503`, `l3-lot-n0r-19941106-0246`, `l3-lot-nvw-19931120-0721`, `l3-lzk-h0c-20200814-0417`, `l3-lzk-h0v-20200812-1309`, `l3-lzk-h0w-20200812-1305`, `l3-lzk-h0z-20200812-1318`, `l3-lzk-ncz-19970301-1912`, `l3-lzk-nme-19970301-2027`, `l3-lzk-ntv-19970301-2027`, `l3-mci-net-20160526-2154`, `l3-mci-nvl-20160526-2154`, `l3-mci-nvw-20160526-2154`, `l3-mci-tr0-20160526-2154`, `l3-mci-tr1-20160526-2154`, `l3-mci-tr2-20160526-2154`, `l3-mci-tv0-20160526-2154`, `l3-mci-tv1-20160526-2154`, `l3-mci-tv2-20160526-2154`, `l3-mci-tzl-20160526-2154`, `l3-mlb-051-19941116-0335`, `l3-okc-net-20220503-005210`, `l3-okc-nvl-20260622-080623`, `l3-okc-nvw-20260622-080623`, `l3-okc-rsl-20220517-085551`, `l3-okc-tv0-20260622-080547`, `l3-okc-tz0-20260622-080547`, `l3-okc-tzl-20260622-080623`, `l3-rax-nll-20220510-155126`, `l3-sgf-nme-20030504-2332`, `l3-shv-nzq-20220503-005452`, `l3-slc-tv0-20160516-2359`, `l3-tlx-063-19940308-1930`, `l3-tlx-064-19940308-1930`, `l3-tlx-087-19940308-1939`, `l3-tlx-103-20010503-0007`, `l3-tlx-daa-20130520-2016`, `l3-tlx-daa-20260622-080623`, `l3-tlx-dod-20130520-2016`, `l3-tlx-dod-20220503-005231`, `l3-tlx-dpr-20130520-2016`, `l3-tlx-dpr-20260622-080623`, `l3-tlx-dsd-20130520-2016`, `l3-tlx-dsd-20220503-005231`, `l3-tlx-dta-20130520-2016`, `l3-tlx-du3-20130520-2008`, `l3-tlx-du3-20260622-080623`, `l3-tlx-du6-20260622-120608`, `l3-tlx-eet-20130520-2016`, `l3-tlx-eet-20260622-080623`, `l3-tlx-hhc-20130520-2016`, `l3-tlx-hhc-20260622-080623`, `l3-tlx-irm-19940308-1115`, `l3-tlx-n0b-20260622-080623`, `l3-tlx-n0c-20130520-2016`, `l3-tlx-n0g-20260622-080623`, `l3-tlx-n0h-20130520-2016`, `l3-tlx-n0k-20130520-2016`, `l3-tlx-n0m-20130520-2016`, `l3-tlx-n0m-20260622-080623`, `l3-tlx-n0q-20130520-2016`, `l3-tlx-n0q-20220503-005231`, `l3-tlx-n0r-20130520-2016`, `l3-tlx-n0r-20220908-131957`, `l3-tlx-n0s-20130520-2016`, `l3-tlx-n0s-20260622-080623`, `l3-tlx-n0u-20130520-2016`, `l3-tlx-n0u-20220503-005231`, `l3-tlx-n0v-20130520-2016`, `l3-tlx-n0v-20220908-131957`, `l3-tlx-n0x-20130520-2016`, `l3-tlx-n0z-20130520-2016`, `l3-tlx-n0z-20220908-131957`, `l3-tlx-n1c-20130520-2016`, `l3-tlx-n1h-20130520-2016`, `l3-tlx-n1k-20130520-2016`, `l3-tlx-n1m-20130520-2016`, `l3-tlx-n1q-20130520-2016`, `l3-tlx-n1s-20130520-2016`, `l3-tlx-n1u-20130520-2016`, `l3-tlx-n1x-20130520-2016`, `l3-tlx-n2c-20130520-2016`, `l3-tlx-n2h-20130520-2016`, `l3-tlx-n2k-20130520-2016`, `l3-tlx-n2m-20130520-2016`, `l3-tlx-n2q-20130520-2016`, `l3-tlx-n2s-20130520-2016`, `l3-tlx-n2u-20130520-2016`, `l3-tlx-n2x-20130520-2016`, `l3-tlx-n3c-20130520-2016`, `l3-tlx-n3h-20130520-2016`, `l3-tlx-n3k-20130520-2016`, `l3-tlx-n3m-20130520-2016`, `l3-tlx-n3q-20130520-2016`, `l3-tlx-n3s-20130520-2016`, `l3-tlx-n3u-20130520-2016`, `l3-tlx-n3x-20130520-2016`, `l3-tlx-nac-20130520-2016`, `l3-tlx-nah-20130520-2016`, `l3-tlx-nak-20130520-2016`, `l3-tlx-nam-20130520-2016`, `l3-tlx-naq-20130520-2016`, `l3-tlx-nau-20130520-2016`, `l3-tlx-nax-20130520-2016`, `l3-tlx-nbc-20130520-2016`, `l3-tlx-nbh-20130520-2016`, `l3-tlx-nbk-20130520-2016`, `l3-tlx-nbm-20130520-2016`, `l3-tlx-nbq-20130520-2016`, `l3-tlx-nbu-20130520-2016`, `l3-tlx-nbx-20130520-2016`, `l3-tlx-ncz-19990503-2316`, `l3-tlx-net-20130520-2016`, `l3-tlx-net-20220503-005231`, `l3-tlx-nhl-20130520-2016`, `l3-tlx-nhl-20220503-005231`, `l3-tlx-nll-20130520-2016`, `l3-tlx-nml-20130520-2016`, `l3-tlx-nml-20220503-005231`, `l3-tlx-nrr-20260622-080623`, `l3-tlx-nsp-20130520-2016`, `l3-tlx-nsw-20130520-2016`, `l3-tlx-nsw-20220503-005231`, `l3-tlx-nvl-20130520-2012`, `l3-tlx-nvl-20260622-080623`, `l3-tlx-nvw-20130520-2016`, `l3-tlx-nvw-20260622-080623`, `l3-tlx-oha-20130520-2016`, `l3-tlx-oha-20260622-080623`, `l3-tlx-pta-20130520-2016`, `l3-tlx-pta-20200501-000023`, `l3-tlx-rcm-20130520-2016`, `l3-tlx-rcm-20220503-004553`, `l3-tlx-rsl-20130520-2358`, `l3-tlx-rsl-20220502-235926`
- `version:1` (57): `l3-akc-nc1-20210730-055033`, `l3-gjx-n0f-20200817-0551`, `l3-gjx-naf-20200817-0551`, `l3-gjx-nbf-20200817-0551`, `l3-gjx-nxf-20200817-0600`, `l3-mci-n1p-20160526-2154`, `l3-mci-ncr-20160526-2154`, `l3-mci-nmd-20160526-2154`, `l3-mci-nst-20160526-2154`, `l3-mci-ntp-20160526-2154`, `l3-okc-ncr-20260622-080623`, `l3-okc-nhi-20220503-005210`, `l3-okc-nmd-20260622-080640`, `l3-okc-nst-20260622-080640`, `l3-okc-ntv-20220503-005210`, `l3-rax-nyf-20200818-0001`, `l3-sgf-ntv-20030504-2352`, `l3-tlx-101-20010503-0007`, `l3-tlx-102-19990504-0052`, `l3-tlx-104-20010503-2355`, `l3-tlx-dvl-20130520-2016`, `l3-tlx-dvl-20260622-080623`, `l3-tlx-n0c-20260622-080623`, `l3-tlx-n0f-20220502-235926`, `l3-tlx-n0h-20260622-080623`, `l3-tlx-n0k-20260622-080623`, `l3-tlx-n0x-20260622-080623`, `l3-tlx-n1p-20130520-2016`, `l3-tlx-n1p-20260629-173638`, `l3-tlx-n3p-20130520-2012`, `l3-tlx-n3p-20220503-011226`, `l3-tlx-nc1-20130520-2354`, `l3-tlx-nc2-20130520-2354`, `l3-tlx-nc3-20130520-2354`, `l3-tlx-nc4-20130520-2354`, `l3-tlx-nc5-20130520-2354`, `l3-tlx-nco-20130520-1816`, `l3-tlx-ncr-20130520-2016`, `l3-tlx-ncr-20260622-080623`, `l3-tlx-ncz-20130520-2016`, `l3-tlx-ncz-20220503-005231`, `l3-tlx-nhi-20130520-2016`, `l3-tlx-nhi-20220503-005231`, `l3-tlx-nla-20130520-2016`, `l3-tlx-nla-20220503-005231`, `l3-tlx-nmd-20130520-2016`, `l3-tlx-nmd-20260622-080623`, `l3-tlx-nss-20130520-2016`, `l3-tlx-nss-20220503-005231`, `l3-tlx-nst-20130520-2016`, `l3-tlx-nst-20260622-080623`, `l3-tlx-ntp-20130520-2016`, `l3-tlx-ntp-20260629-173638`, `l3-tlx-ntv-20130520-2016`, `l3-tlx-ntv-20220503-005231`, `l3-tlx-spd-20130520-2016`, `l3-tlx-spd-20220503-005231`
- `version:2` (10): `l3-mci-dhr-20160526-2154`, `l3-mci-dpa-20160526-2154`, `l3-mci-dsp-20160526-2154`, `l3-rax-dta-20200818-0454`, `l3-tlx-dhr-20130520-2016`, `l3-tlx-dhr-20260622-080623`, `l3-tlx-dpa-20130520-2016`, `l3-tlx-dpa-20260629-173638`, `l3-tlx-dsp-20130520-2016`, `l3-tlx-dsp-20260629-173638`
- `version:3` (1): `l3-tlx-dta-20260622-080623`

#### `vol-block:`

- `vol-block:44` (23): `l2-kvwx-20080415-235337`, `l2-kpah-20080415-235014`, `l2-kdmx-20080525-205148`, `l2-kvnx-20110315-000203`, `l2-ktlx-20130520-201643`, `l2-kgwx-20130601-235640`, `l2-koax-20140616-205305`, `l2-kewx-20160413-022531`, `l2-kdvn-20200810-175718`, `l2-kdvn-20200810-180401`, `l2-kdvn-20200810-181043`, `l2-kdvn-20200810-181724`, `l2-klix-20210829-180425`, `l2-klix-20210829-173117`, `l2-klix-20210829-175748`, `l2-tstl-20230331-230314`, `l2-kdmx-20080525-205148-trim`, `l2-ktlx-20130520-201643-trim`, `l2-koax-20140616-205305-trim`, `l2-kewx-20160413-022531-trim`, `l2-kdvn-20200810-180401-trim`, `l2-klix-20210829-180425-trim`, `l2-tstl-20230331-230314-trim`
- `vol-block:52` (16): `l2-kbox-20220129-150537`, `l2-tjua-20220918-190621`, `l2-kdgx-20230325-010651`, `l2-kmaf-20230331-230843`, `l2-pgua-20230524-030945`, `l2-kmtx-20240301-212827`, `l2-ktlx-20240315-000217`, `l2-ktlx-20240515-000014`, `l2-pahg-20250909-212549`, `l2-kilx-20260418-013553`, `l2-kiwa-20260917-003629`, `l2-kbox-20220129-150537-trim`, `l2-pgua-20230524-030945-trim`, `l2-kmtx-20240301-212827-trim`, `l2-ktlx-20240315-000217-trim`, `l2-kilx-20260418-013553-trim`

#### `writer:`

- `writer:radx` (8): `cfrad1-irene-sr2-20110827-120420-sur-sweeps01`, `cfrad2-radx-irene-sr2-20110827-120420-sur-r30km`, `cfrad2-radx-iesha-20260305-0115-sweeps7-10-int32`, `dorade-cow2-20260521-225514-sur-head24`, `dorade-n42rf-ts-20181010-122951-air`, `dorade-n42rf-ts-20181010-122951-air-head24`, `dorade-n42rf-tm-20181010-123925-air`, `dorade-n42rf-tm-20181010-123925-air-head48`
- `writer:sigmet_dorade` (6): `dorade-noxp-20090501-190244-ppi`, `dorade-noxp-20090501-190324-ppi`, `dorade-noxp-20090525-203211-sector`, `dorade-noxp-20090610-003210-ppi-head6`, `dorade-noxp-20090610-003222-ppi-head6`, `dorade-noxp-20090610-003226-ppi-head6`
- `writer:xradar` (2): `cfrad2-xradar-xsapr-sgp-20110520-ppi`, `cfrad2-xradar-dow8-20211011-223602-rhi-r300`

#### `zdr:`

- `zdr:8bit` (13): `l2-kvnx-20110315-000203`, `l2-ktlx-20130520-201643`, `l2-kgwx-20130601-235640`, `l2-koax-20140616-205305`, `l2-kewx-20160413-022531`, `l2-kdvn-20200810-175718`, `l2-kdvn-20200810-180401`, `l2-kdvn-20200810-181043`, `l2-kdvn-20200810-181724`, `l2-ktlx-20130520-201643-trim`, `l2-koax-20140616-205305-trim`, `l2-kewx-20160413-022531-trim`, `l2-kdvn-20200810-180401-trim`
- `zdr:16bit` (20): `l2-klix-20210829-180425`, `l2-klix-20210829-173117`, `l2-klix-20210829-175748`, `l2-kbox-20220129-150537`, `l2-tjua-20220918-190621`, `l2-kdgx-20230325-010651`, `l2-kmaf-20230331-230843`, `l2-pgua-20230524-030945`, `l2-kmtx-20240301-212827`, `l2-ktlx-20240315-000217`, `l2-ktlx-20240515-000014`, `l2-pahg-20250909-212549`, `l2-kilx-20260418-013553`, `l2-kiwa-20260917-003629`, `l2-klix-20210829-180425-trim`, `l2-kbox-20220129-150537-trim`, `l2-pgua-20230524-030945-trim`, `l2-kmtx-20240301-212827-trim`, `l2-ktlx-20240315-000217-trim`, `l2-kilx-20260418-013553-trim`

<!-- END GENERATED -->
