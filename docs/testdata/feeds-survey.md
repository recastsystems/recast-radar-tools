# Real-feed survey

This page records a survey of every provider in `recast-radar-data`, run on 2026-09-24 between 21:15 and
22:15 UTC, with fix passes on 2026-09-25 (and one more capture on 2026-09-26, section 1). It is the survey
part of gap G12. Two questions were asked:

1. For each source `recast-radar-data` fetches radar data from, does our reader decode what it publishes now?
2. Where an independent reader reads the same file, do the two agree?

## Summary

- **Upstream**: 29 frames (122 files) from all 14 international providers in recast-radar-data decode, and
  so do the first part of the newest frame of three more ORD sites (`frlep`, `frmcl`, `frtra`, fetched
  later to check their site-table entries) and a DWD frame planned with the new filtered-DBZH option (30
  files). IMGW's POLRAD CMAX images, the one grid product in the crate that our decoders read, decode
  through recast-radar-io-odim's Cartesian decoder (2 files; the volume router refuses ODIM `IMAGE`
  objects, as designed). 2 AWS NEXRAD sites (archive and real-time chunks) and 8 of 9 community-feed files
  decode; the other 10 of the 19 community feeds had no file to fetch (stale listings, missing `dir.list`,
  and the Laredo EWR feed, whose site directory has been empty since 2025-10-14). A direct h5py read of
  every ODIM dataset of the 153 ODIM volume files agrees with our decode on gate counts, spacing,
  valid-gate counts and value ranges; the only difference is AEMET's `where/rstart`, which our decoder
  reads as metres on purpose. The rest of recast-radar-data (Italy DPC and Taiwan CWA composites, GDEX,
  tropical cyclones, catalogs) publishes no radar data that recast-radar-io reads, and is listed as left
  out in section 2.2.
- **Decoder failures** (committed as known failures in `testdata/feeds/manifest.toml` and, for 2,
  `testdata/other/manifest.toml`; checked by `crates/recast-radar-data/tests/feeds_known_failures.rs`):
  1. `level2-ldm-block-limit`: a real volume with one radial per LDM record (North Dakota SWC's KXWA,
     4,760 records) exceeds the decoder's 4,096-record limit; Py-ART reads it. This is the one failure
     without a committed fixture: no part of the volume under the 2 MB cap reaches the limit (its radial
     records are 3,074 to 4,693 bytes, so any 4,097 of its records take at least 17.6 MB), and the
     smallest prefix that does (17.8 MB) does not fit the 60 MB total (section 4). Its check, and the
     check of the committed head against its source, run only where the shared testdata cache holds the
     whole file, so CI never runs them. Committing the prefix is an owner decision (section 4).
  2. `jma-lowest-level-valid`: the JMA decoder gives level 1 of the reflectivity level table, which
     JMA's format description of the product defines as "No Echo" (value 0.00), as a valid 0.0 dBZ, so no
     gate is ever below threshold (section 3.2). The tag is on the committed TAKA member of the Hagibis
     N5 tar.
- **recast-radar-data changes**: a reader for GR2Analyst polling directories (`polling`: `dir.list`,
  `config.cfg`; listed names that are not plain file names are refused; a root whose `grlevel2.cfg` names
  one site stands for that site; the same API as the `frontends` branch's module, so the two merge into
  this one, and a `polling_listing` fuzz target), six live ORD radars added to the ORD site table
  (Iceland's mobile `isx2`, which gets no advertised position, and five more),
  `DwdProvider::filtered_reflectivity(true)` (DWD's clutter-filtered DBZH; tested on real Borkum
  listings), `OrdProvider::velocity_scan_only(true)` (a site's velocity scan alone: for Belgium's `bejab`
  the Doppler scan's DBZH and VRAD), `OrdProvider::complete_cycles(true)`, provider options that chain in
  any order, and the survey programs `feeds_plans` and `feeds_survey`. The Python side of the survey
  (frame decoding, the h5py cross-check, the independent-reader goldens and the testdata budget) is in
  `tools/feeds_survey/`.

## 1. Method

Rust programs (in `crates/recast-radar-data/examples/`):

- `feeds_plans plan|fetch|imgw|poll`: asks a provider for its newest frame plan (`IntlProvider::latest`)
  and downloads the parts with `fetch_volume_bytes` (recording the plan in `frame.json`), lists and
  downloads IMGW's newest CMAX cycle, or reads a polling directory with `polling::latest_volume`.
  It starts each provider `latest` call, each IMGW cycle listing, each poll and each download at least
  1.5 s after the one before. The requests inside one call go back to back: a DWD frame's station,
  `hdf5/` and sweep listings (8 for one frame with the filtered DBZH), an ORD plan's hour listings, and
  up to three for `latest_volume_or_single_site` (a root's `dir.list`, its `grlevel2.cfg` and the site's
  `dir.list`).
- `feeds_survey`: decodes files through `recast_radar_io::read_supported_volume_with_metadata` (JMA tars
  through `read_jma_tar_volumes`, every station or one), or merges one frame's parts with
  `merge_volumes` (`--merge`), and prints one JSON line per volume: site, location, times, sweeps, each
  field's valid-gate count and value range, and for Level II the archive layout from
  `recast_radar_io_nexrad::messages` (volume header, LDM records and control words, message types per
  record, Message 5 and 18 headers, per-cut Message 31 blocks and moments). An ODIM Cartesian `IMAGE`
  (IMGW) gets an `odim_image` grid summary from the Cartesian decoder.
- `live_decode` (existing): AWS archive plus real-time chunks for one NEXRAD site.

Python programs (in `tools/feeds_survey/`, standard library plus the reference venv; section 5 gives the
order): the decode of the cached upstream frames (`run_upstream_survey.py`), the h5py cross-check
(`odim_crosscheck.py`), Py-ART on the KXWA files for the known-failure goldens (`fixture_goldens.py`) and
the testdata budget (`testdata_budget.py`). All of them read local files only.

Upstream files are cached under `~/radar-corpus/feeds/<provider>/<site>/`, with `frame.json`
giving each frame's parts in plan order. Nothing was fetched in parallel from one host. Besides the first
survey's frames, the later passes made these requests, each at least 1.2 s after the one before on the
same host:

- ORD: the first part of the newest frame of `frlep`, `frmcl` and `frtra` (2026-09-25 00:09Z), to check
  their new site-table entries; `ltlau`'s `SCAN` hour listed five times, at least 20 s apart, until the
  03:45 cycle was arriving (03:44-03:53Z; the listings of 03:44:56Z and 03:46:39Z are kept under
  `feeds/ord/ltlau/listings/`), and `feeds_plans plan ord:ltlau ord-complete:ltlau` once (listing
  requests only); `feeds_plans plan ord:bejab ord-vscan:bejab` once (10:32:39-10:32:48Z, listing
  requests only; section 4).
- EUMETNET's OPERA radar database (the OPERA page, `OPERA_RADARS_DB.json` and `OPERA_RADARS_ARH_DB.json`,
  cached under `feeds/opera/`), for the status of the new ORD site-table entries.
- IMGW: one listing and two files (the CMAX images of section 2.1).
- North Dakota SWC (`level2.swc.nd.gov`), at least 2 s apart: the KXWA `dir.list` and the `/raw/` index
  (2026-09-25 01:55-02:00Z), KXWA's `dir.list` once more (03:18Z, cached as
  `feeds/polling/KXWA/dir.list.20260925` and committed as a polling capture), and `HEAD` requests for the
  surveyed KXWA volume (01:58Z, 09:03Z, 09:37:45Z, 10:30:34Z and 11:05Z: each 200, with the same
  `Last-Modified` and size) and the newest one.
- Laredo EWR (`LARE`, section 2.1): six requests by hand, at least 2 s apart (the root's `grlevel2.cfg`,
  `dir.list` and index, `LARE/dir.list` and `LARE/` index, `_READ_ME.txt`; 03:01Z), cached under
  `feeds/polling/LARE/`, then `feeds_plans poll` on the root (three requests).
- DWD: boo's filtered DBZH of the 21:30Z cycle (the `hdf5/` index, one `HEAD` and the 10 sweeps, into
  `feeds/dwd/boo-dbzh/`), one newest frame with the new DBZH option (8 listing requests and 30 sweeps,
  into `feeds/dwd-dbzh/boo/`), and 5 listings of Borkum for the filtered-DBZH test (08:56Z, 4.1 MB, under
  `feeds/dwd-dbzh/boo/listings/`).
- JMA's format description of the per-radar polar reflectivity product, once, from `www.mri-jma.go.jp`
  (10:17Z, section 3.2; a copy under `feeds/jma/docs/`).
- The Iowa Environmental Mesonet's polling root `config.cfg`, once (2026-09-26 02:13:28Z, when the stream
  was merged; cached under `feeds/polling/iem-root/` and committed as a polling capture).

Independent readers (Python venv `~/radar-ref-venv`): Py-ART 2.3.0 (`read_nexrad_archive`),
h5py 3.16.0. The local steps (sections 2.1 and 3.1) were rerun with the committed scripts on the cached
files and gave the numbers below.

## 2. Sources

### 2.1 Sources with a recast-radar-data provider

Each row is one frame from the provider's `latest` (plan and download through the crate), decoded part by
part and merged as the poller does. All parts decode; the h5py cross-check (section 3.1) agrees with every
ODIM part.

| Source | Provider (site) | Frame | Files, bytes | Decoded (merged) |
|---|---|---|---|---|
| DWD open data | `dwd` (`boo`) | 21:30:35-21:34:02Z | 20 (TH + VRADH per sweep), 2.46 MB | 10 sweeps, TH VRADH. In the third fix pass `DwdProvider::new()` with the filtered-DBZH option (now `filtered_reflectivity(true)`; `feeds_plans fetch ... dwd-dbzh:boo`) planned the 2026-09-25 03:20:57-03:24:02Z cycle as 30 parts (TH, VRADH, DBZH), which merge to 10 sweeps of TH, VRADH and DBZH |
| CHMI | `chmi` (`ska`) | 21:29:17-21:35:25Z | 21, 5.04 MB | 14 sweeps, DBZH VRAD WRAD TH ZDR RHOHV PHIDP |
| SHMU | `shmu` (`skjav`) | 21:30:03-21:34:00Z | 8, 4.12 MB | 12 sweeps, + KDP |
| SMHI | `smhi` (`angelholm`) | 21:30:04-21:34:17Z | 1 (qcvol), 16.5 MB | 10 sweeps, 15 fields |
| FMI | `fmi` (`fianj`) | 21:30:05-21:34:42Z | 1, 27.8 MB | 13 sweeps, 17 fields |
| DMI | `dmi` (`06177` Stevns) | 21:25:01-21:27:23Z | 1, 1.06 MB | 10 sweeps, 8 fields |
| ANM Romania | `meteoromania` (`BUC`) | 21:30:02-21:34:05Z | 5 (one per moment), 3.77 MB | 12 sweeps, DBZH VRADH ZDR KDP RHOHV |
| KAIA Estonia | `kaia` (`eehar`) | 21:30:02-21:34:28Z | 1, 38.6 MB | 15 sweeps, 16 fields |
| ARPA Piemonte | `arpa-piemonte` (`bric`) | 21:35:02-21:39:11Z | 1, 5.59 MB | 11 sweeps |
| ARPA Lombardia | `arpa-lombardia` (`des`) | 21:35:45-21:38:19Z | 9 gzip-wrapped ODIM, 1.45 MB | 8 sweeps, 9 fields |
| GeoSphere Austria | `geosphere` (`hochficht`) | 21:35:02-21:39:39Z | 1, 2.25 MB | 12 sweeps |
| EUMETNET ORD | `ord`: `bejab`, `chalb`, `hrbil`, `iedub`, `isska`, `ltlau`, `mtgud`, `nlhrw`, `nohur`, `plleg`, `eslid`, `frtre`, `eesur`, then `isx2`, `hrgol`, `frale`; in the fix pass the first part of `frlep`, `frmcl`, `frtra` | 21:35-21:42Z, `isx2`/`hrgol`/`frale` 22:09-22:10Z, `frlep`/`frmcl`/`frtra` 2026-09-25 00:09Z | 53 files, 44.9 MB | all decode; each of the six sites added to the site table has the table's coordinates in `where/lat,lon` and the table's `PVOL`/`SCAN` type; `silis` had no file in the last 6 h |
| JMA (NICT mirror) | `jma` (`ITOK`) | stamp 21:40Z | N5 + N6 tars, 6.05 MB (all 20 stations) | ITOK: 35 sweeps, DBZH VRADH |
| BoM via NCI | `australia-nci` (`2`, Melbourne) | 2026-09-21 23:40Z (NCI runs about 3 days behind) | 1 ZIP member, 4.86 MB | 14 sweeps, 12 fields |
| IMGW-PIB POLRAD CMAX (`grid_products::imgw`) | `imgw_polrad_latest_cycle` (`leg`, Legionowo; `feeds_plans imgw`) | 2026-09-25 00:10:07Z | ZDR and KDP of the cycle's three files, 93 KB | ODIM_H5 2.3 `IMAGE`, `MAX` product: the router refuses it (`ODIM_H5 object 'IMAGE' unsupported (PVOL and SCAN only)`, by design); `recast_radar_io_odim::odim_cartesian::decode_odim_h5_cartesian_max` reads both, 500 x 500 cells of 1002 x 998 m, ZDR 17,368 valid cells -7.92..11.76 dB, KDP 4,474 cells -1.02..1.21 deg/km, the same as h5py |

NEXRAD on AWS (library functions, `live_decode`): KMKX archive volume 21:41:25Z (VCP 35, 12 cuts, 6480
radials) and real-time volume 21:48:13Z followed chunk by chunk to its end (55 chunks; a partial decode
at 23 chunks). KTLX's newest archive volume was 13:55:11Z, 7.8 hours old, with 5 cuts; it decoded, and
the real-time bucket held the same volume.

Community feeds (all 19 of `community_feeds`, read with the new `polling` module):

| Feed | Result |
|---|---|
| IEM FWLX, FUSA, GAWX, WILU, MZZU | newest file downloaded and decoded (uncompressed, with the volume header's date in the high halfword) |
| IEM KULM | decoded, but its newest listed file is from 2024-05-26 |
| IEM FOP1 | decoded (LDM bzip2, VCP 215, 2 cuts) |
| IEM DAN1, DOP1, NOP3, NOP4, ROP3, ROP4, KCRI | the listing's newest file answers 404: stale listings |
| IEM OP5R, ND SWC K08D | `dir.list` answers 404 |
| ND SWC KBPP | decoded: `ARCHIVE2` header with a NUL ICAO, Message 1 only (2880 radials), VCP 31; the volume's instrument name is empty |
| Laredo EWR LARE (`http://offsitevpn.ewradar.com/Laredo/archive2.trans`) | no file (probed 2026-09-25 03:01Z): the root has no `dir.list` (404); its `grlevel2.cfg` (`Site: LARE`, last modified 2022-07-27) names one site, and `polling::latest_volume_or_single_site` follows it to `LARE/`, whose `dir.list` answers 404: the directory is empty, last modified 2025-10-14. The host's `_READ_ME.txt` says files arrive under names like `E70230324160559.RAWLFRP` (Vaisala IRIS's RAW product naming) and a script renames them `LARE_23032416_0559` and writes `dir.list`; if they are IRIS RAW, recast-radar-io has no decoder for them |
| ND SWC KXWA | **fails**: one radial per LDM record, 4,760 records, over the decoder's 4,096 (**known failure 1**); Py-ART reads it. On 2026-09-25 at 01:58Z the volume was still served (`Last-Modified` 21:48:37Z) and `dir.list` listed 1,143 volumes back to 2026-09-21 10:58Z (about 3.6 days); its sizes are not bytes (40,288 for this 20,616,906-byte volume, 30,496 for the 15,605,729-byte one of 01:49Z: about 512 bytes per unit, like a count of 512-byte disk blocks) |

### 2.2 recast-radar-data modules left out

Everything in the crate that fetches radar data was surveyed: the 14 `IntlProvider`s (2.1), AWS NEXRAD
archive and real-time chunks (`realtime`, `live_decode`), all 19 `community_feeds` with the new `polling`
module (Laredo EWR only in the third fix pass; the first survey missed it), and IMGW's POLRAD CMAX
(`grid_products::imgw`). The other modules were left out, because what they fetch is not radar data
recast-radar-io reads:

| Module | What it fetches | Why left out |
|---|---|---|
| `grid_products` Italy DPC (`italy_dpc_*`) | national products (VMI, SRI, SRT1, CUM3-CUM24, plus IR 10.8 and temperature layers) as GeoTIFF from the DPC API, and WMTS PNG tiles | Cartesian composites of many radars (and non-radar layers) in GeoTIFF/PNG; recast-radar-io has no GeoTIFF or image reader |
| `grid_products` Taiwan CWA (`taiwan_cwa_*`) | the O-A0059-001 composite as a JSON grid | a composite grid parsed by the crate itself (`parse_taiwan_cwa_latest_json`), not a radar file |
| `grid_products` catalog (`grid_products()`, `grid_product_providers()`) | nothing: a static catalog of ORD composites, MRMS, UK Met Office, KNMI, MeteoSwiss, DWD, AEMET, IPMA and Meteoalarm products | catalog entries without fetchers |
| `gdex` | NSF NCAR GDEX THREDDS catalogs and NCSS subsets (reanalysis grids such as ERA-20C, classic netCDF) | gridded model and reanalysis data, not radar |
| `tropical` | NHC `CurrentStorms.json`, GDACS events, JTWC warning text | storm tracks and warnings, not radar |
| `sites`, `fetch_weather_gov_radar_sites`, `fetch_level2_radar_sites`, `fetch_mping_reports_geojson` | NEXRAD site catalogs and mPING reports | catalogs and reports, not radar data |

## 3. Cross-checks

### 3.1 Our ODIM decode against h5py

For every ODIM part of every upstream frame (153 files, 1,866 datasets, the NCI ZIP member unwrapped;
`tools/feeds_survey/odim_crosscheck.py`), each `datasetN/dataM` was read with h5py (gain and offset
applied, `nodata` and `undetect` masked) and matched to our sweep and field by quantity, elevation and ray
count. Gate counts, gate spacing, valid-gate counts and minimum and maximum agree everywhere, and so does
each first gate centre with `rstart + rscale / 2`, except in one case: AEMET's first gate (eslid). Its
`where/rstart` is 125, which ODIM defines in km; our decoder reads it as metres on purpose (manifest tag
`quirk:rstart-metres`), so our first gate centre is 375 or 625 m where the literal reading gives
125.25 or 125.5 km.

### 3.2 Decodes but suspicious

In our decoders (for the decoder streams to judge):

- **JMA** (`recast-radar-io-jma`; **known failure 2**, `jma-lowest-level-valid`, tagged on the committed
  TAKA member `jma-n5-20191012-090000-rs47773`): every gate of every ITOK sweep of the 21:40Z tar decodes
  as valid, minimum 0.0 dBZ. JMA's format description of this product ("レーダー毎極座標レーダーエコー強度
  GPVフォーマット (GRIB2形式 Ver.2.00)", 2007-05-17, table ※3; published for JMA's weather business
  consortium at <https://www.mri-jma.go.jp/Project/cons/data/SitePolar.pdf>, fetched 2026-09-25, sha256
  `1ac4650797a942ee67f5af068f2d5d32b9e9a9ea3e8811823aa52b12032c9bfc`, a copy under
  `feeds/jma/docs/`) defines the levels: 0 is outside the observed range or missing, 1 is "No Echo"
  (value 0), 2 is below 0.32 dBZ (0.16; negative dBZ included), and each level after it a 0.32 dB class
  with its centre as the value, up to 252 (80 dBZ and over, 80.16). The files carry that table (GRIB2
  template 5.200, decimal scale 2): ITOK's member of the 21:40Z tar lists 252 levels, 0, 16, 48, 80, ...,
  8016 in 0.01 dBZ, and the TAKA member starts the same way (the check reads it). The decoder treats
  only level 0 as missing, so "No Echo" becomes a valid 0.0 dBZ: in the TAKA member all 7,526,400 DBZH
  gates are valid and 5,905,836 of them (78%) are 0.0 dBZ. No independent reader decodes JMA's polar GRIB2 (ecCodes 2.48.0, installed in the
  reference venv for this, has no definition of the local grid template 3.50120), so the check pins the
  file's own level table, read from its bytes, and fails once level 1 stops decoding as a valid 0.0 dBZ.
  All rays carry the tar stamp as their time, the Nyquist velocity is unset and the first gate centre is
  0 m; the crate documents these three choices.
- **DMI ODIM**: our decode matches h5py, but the file's RHOHV gain (0.00278, so at most 0.709) and PHIDP
  in radians (gain 0.0247, offset -3.166) break the ODIM conventions (RHOHV to 1, PHIDP in degrees). A
  source quirk for G7 to decide on.
- **KBPP** (ND SWC): Message 1 with a NUL ICAO decodes with an empty instrument name, although the file
  name says KBPP.

In the feeds:

- **ORD per-sweep countries** (LT, and any SCAN country mid-cycle): `OrdProvider::latest` anchors on the
  newest stamp, so while a cycle is still being uploaded it returns a partial frame (ltlau at 21:41: 2 of 8
  sweeps). The identity counts parts, so the poller picks up the rest on later ticks; a caller wanting one
  complete volume needs the previous cycle, which the new opt-in `OrdProvider::complete_cycles(true)`
  plans: a `SCAN` cycle whose sweep files are a strict subset of the previous cycle's gives way to that
  cycle when it is at most 20 minutes older. Its test uses a real `ltlau` listing captured at 03:46:39Z
  on 2026-09-25 with 1 of the 03:45 cycle's 8 sweeps listed (the default plans that one sweep, the option
  the 8 sweeps of 03:40). Live at 03:52:22Z, `feeds_plans plan ord:ltlau ord-complete:ltlau` planned
  the 03:50 cycle's 2 listed sweeps by default and the 03:45 cycle's 8 with the option. The default is
  unchanged; which one `intl_providers()` uses is for the owner (section 4).

## 4. Known failures, fixes and open items

Fixtures (`testdata/feeds/manifest.toml`, 5 entries, every one tagged `license:unknown` for the license
review before publishing): the KXWA volume, checksum-pinned and not committed, and its first 41 LDM
records, committed (181,437 bytes), which are the one `derived_from` prefix; and three polling-directory
captures, committed (43 KB: ND SWC's KXWA `dir.list`, the Iowa Environmental Mesonet's root `config.cfg`
and the Laredo EWR root's `grlevel2.cfg`), which recast-radar-data's polling tests and the
`polling_listing` fuzz seeds read. Known failure 2 is a tag on the TAKA member of
`testdata/other/manifest.toml`. `crates/recast-radar-data/tests/feeds_known_failures.rs` checks each
failure. For KXWA it pins what an independent reader makes of the same bytes (its `golden` module:
Py-ART 2.3.0's rays per sweep of the whole volume and of the committed head, printed by
`tools/feeds_survey/fixture_goldens.py`) and compares the raw bytes with it, so a fixed decoder's own
test has its expected values; for JMA it pins the file's own level table. It also checks that the
committed head is the start of its source where the shared testdata cache holds the source; the check
reads the cache only and never downloads. Two of its checks therefore run only on a machine whose
testdata cache holds the whole KXWA volume (the survey machine's does): known failure 1, and the prefix
check. Elsewhere, CI included, they print `skipping ...` and `checked 0 of 1 committed prefix` and pass.

| # | Tag | Fixture | Failure | Where to fix |
|---|---|---|---|---|
| 1 | `level2-ldm-block-limit` | `ndswc-kxwa-20260924-214316` (the whole 20.6 MB volume, checksum-pinned, not committed: the check reads it from the shared testdata cache and skips where the cache lacks it; its URL still answered on 2026-09-25 11:05Z, `Last-Modified` 2026-09-24 21:48:37Z, and should roll off around 2026-09-28); `ndswc-kxwa-20260924-214316-head41` (41 of 4,760 records, committed) samples the layout | real volumes may have more than 4,096 LDM records | `recast-radar-io-nexrad` `MAX_BZIP_BLOCKS`; the fix's regression test needs the whole volume or its first 4,097 records (17,819,782 bytes, sha256 `bf30ddcf67b75904c1278d0ee44d743b91dc56a389198f10210b45204c6c066b`), both over the 2 MB cap: every radial carries SW, ZDR and RHO noise on all 3,218 gates, about 4.3 KB per record, though REF and VEL are nearly empty. The prefix does not fit the 60 MB total either (open items). Committing it is the owner's decision |
| 2 | `jma-lowest-level-valid` | `jma-n5-20191012-090000-rs47773` (TAKA member of the Hagibis N5 tar, committed, `testdata/other/manifest.toml`) | level 1 of the reflectivity level table, "No Echo" in JMA's format description (value 0.00), decodes as a valid 0.0 dBZ: all 7,526,400 DBZH gates valid, 5,905,836 of them 0.0 dBZ (3.2) | `recast-radar-io-jma` level table (level 1 as below threshold, a gate state the model keeps, distinct from level 0's missing) |

Changes in `recast-radar-data` (this stream):

- `polling`: `dir.list` and `config.cfg`/`grlevel2.cfg` parsing. By server root and site id, with the
  same names and signatures as the `frontends` branch's module (its CLI, Python package and fuzz target
  call them): `parse_dir_list` and `parse_site_config` return the entries and site ids a client can use,
  `dir_list_url`, `site_file_url` and `site_config_url` build URLs, and with `net`
  `fetch_dir_list(root, site)` and `fetch_site_config(root)` fetch. By site directory:
  `DirList::parse` and `SiteConfig::parse` keep every line as listed, newest-volume selection skips
  `.tmp`, `.part` and state files, and with `net` `latest_volume` and `latest_volume_or_single_site`,
  which follows a root's `grlevel2.cfg` to its one site when the root has no `dir.list` (the convention
  `community_feeds` documents; Laredo EWR). Limits on entries, sites and line length, each an error
  rather than a shortened list (the client parsers then return nothing, the fetches the error), and
  duplicate `Site:` lines found with a hash set (a 16 MiB configuration of 10,000 sites and then
  duplicates took 38 s with the first version's linear scan, 0.1 s now); a line that is not
  `<size> <name>` is kept in `DirList::skipped` and the rest of the listing is read (a name with a space
  is kept as listed, then refused as a URL component); names and site ids are used only when they are
  plain file names (RFC 3986 unreserved characters, no leading or trailing dot, not a Windows device
  name such as `CON` or `nul.ar2v`), and the client views keep each name once whatever its letter case,
  so a hostile `dir.list` cannot steer a request or a local file name with `\`, `/`, `?`, `#`, `%` or
  `:`, open a Windows device, or make two listed names one local file; a byte-order mark is ignored in a
  site configuration as in a listing. Tests on the committed ND SWC, IEM and Laredo captures
  (`testdata/files/other/polling/`); a `polling_listing` fuzz target (same name and wrapper as
  `frontends`'; it also checks that no two usable names differ only in case), whose seeds are those three
  captures.
- Provider options are chainable setters that take a `bool` and apply in any order
  (`DwdProvider::new().filtered_reflectivity(true).dual_pol(true)`); `DwdProvider::with_dual_pol()`
  stays as the shorthand it was.
- `OrdProvider::complete_cycles(true)`: plan the previous `SCAN` cycle while the newest is still
  arriving (its sweep files a strict subset of the previous cycle's, at most 20 minutes apart); the
  default provider is unchanged. `feeds_plans` plans it as `ord-complete`.
- `OrdProvider::velocity_scan_only(true)`: for a site that publishes its reflectivity and velocity scans
  as separate `PVOL` volumes, plan only the volumes on the velocity's elevation set (Belgium's `bejab`:
  the 9-cut DBZH and VRAD, where the default takes the 11-cut DBZH; AEMET: the Doppler volume alone). It
  never trades reflectivity away: with no reflectivity on that elevation set (Norway's VRADH covers 8 of
  the DBZH's 10 cuts), with velocity on several sets, and for `SCAN`, the plan is the default one. Tested
  on the recorded `bejab`, `esatn`, `nohur`, `iedub`, `nlhrw`, `mtgud`, `frtou` and `ltlau` listings;
  `feeds_plans` plans it as `ord-vscan`.
- `DwdProvider::filtered_reflectivity(true)`: DWD's clutter-filtered DBZH
  (`sweep_vol_z/<site>/hdf5/filter_polarimetric/`, else `filter_simple/`) as an optional product beside
  TH, ten more parts per frame; the default provider is unchanged. Its test plans three cycles from real
  Borkum listings captured together (TH, VRADH and DBZH sweep listings of 2026-09-25 08:56Z, trimmed to
  their quantity's lines of the three newest cycles) and checks that each frame is the default frame
  followed by the ten DBZH sweeps of the TH cycle, stamp for stamp, under an identity of its own.
- `tests/feeds_known_failures.rs`: a check per known failure.
- ORD site table: `frale`, `frlep`, `frmcl`, `frtra`, `hrgol`, `isx2` added from the ORD catalog (their
  OPERA status is not all 1; see the table's doc comment). `isx2` is a mobile radar: its row keeps where
  it stood on 2026-09-24, it is left out of the static catalog (which promises a fixed position), and the
  live listing names it without a position.
- `examples/feeds_plans.rs`, `examples/feeds_survey.rs`: the survey programs; `tools/feeds_survey/`: the
  Python side, with `testdata_budget.py`, which recounts the committed testdata of every stream branch
  from git (read only).

Open, for later steps or the owner:

- Known failure 1 has no committed fixture, so neither its check nor the decoder fix's regression test
  can run on CI. The smallest real file that reproduces it is the KXWA volume's first 4,097 records,
  17,819,782 bytes: about 9 times the 2 MB per-file cap, and over the 60 MB total cap
  (`COMMITTED_TOTAL_CAP_BYTES = 60_000_000` in `recast-radar-testdata/tests/trim.rs`). Committed bytes
  (the committed entries' sizes in every manifest, each committed path once) on the integration branch
  after this stream merged: 48,625,428; with the prefix 66,445,210, 6,445,210 over the cap. The streams
  merged after it add more, so recount with `tools/feeds_survey/testdata_budget.py` before deciding.
  The choices:
  1. Commit the prefix, make it an exception to the 2 MB per-file cap, and raise the total cap to at
     least the recounted total plus 17,819,782 bytes (`--cap N` prints what a cap of N leaves). The
     known-failure check then runs everywhere.
  2. Keep it checksum-pinned and cache-only, as now: the check and the fix's regression test run only on
     a machine whose testdata cache holds the volume. ND SWC still served it on 2026-09-25 at 11:05Z
     (`Last-Modified` 2026-09-24 21:48:37Z) and should drop it around 2026-09-28; after that the only
     copies are that cache and `~/radar-corpus/feeds/polling/KXWA/`.
  For option 1 the prefix is ready at
  `~/radar-corpus/feeds/polling/KXWA/KXWA20260924_214316_V06.head4097.ar2v` (the hash
  above, rechecked on 2026-09-25); the backlog entry lists the manifest and cap changes.
  The survey found no smaller real reproducer. Within this volume none exists: its radial records are
  3,074 to 4,693 bytes, so even the 4,096 smallest with the Message 5 record and the volume header take
  17,643,383 bytes. KXWA's `dir.list` of 2026-09-25 03:18Z names 1,161 volumes back to 2026-09-21
  10:58Z, listed at 27,392-66,880 units (about 14-34 MB at the surveyed volume's 512 bytes per unit),
  and no other surveyed feed writes one radial per LDM record: among the community feeds FOP1 writes 120
  radials per record and the rest are uncompressed.
- ORD's `bejab` comes as two scans per cycle. `OrdProvider::new()` plans the reflectivity scan's DBZH
  beside the Doppler scan's VRAD and leaves the Doppler scan's DBZH out, by design (its test
  `stamp_ties_prefer_the_volume_with_more_elevations`); the opt-in `OrdProvider::velocity_scan_only(true)`
  plans the Doppler scan's DBZH and VRAD as one volume. The default is unchanged; whether it should change
  (leaving out the reflectivity scan's 0.3-3.8 deg cuts, or merging both DBZH files into one volume) is
  the owner's call.
- `DwdProvider::new()`, the provider `intl_providers()` returns and the survey used, plans unfiltered TH
  and VRADH; `dual_pol(true)` (or `DwdProvider::with_dual_pol()`) adds ZDR, RHOHV and PHIDP, and the new
  `filtered_reflectivity(true)` adds the clutter-filtered DBZH, each ten more sweeps per frame. Whether
  `intl_providers()` should turn either on by default (more bandwidth per poll, and new frame identities)
  is left for a decision.
- `OrdProvider::latest` can return a cycle that is still arriving (3.2). `OrdProvider::complete_cycles(true)`
  plans the last complete cycle instead; whether `intl_providers()` should build ORD with it (whole
  volumes, one cycle later) or keep progressive frames (the default) is the owner's call, as is whether
  it should plan velocity scans only (`velocity_scan_only(true)`).

## 5. Reproducing

The Rust programs:

```sh
cargo build --release -p recast-radar-data --examples
# site tables and newest plans (no volume downloads)
cargo run --release -p recast-radar-data --example feeds_plans -- sites
cargo run --release -p recast-radar-data --example feeds_plans -- plan dwd:boo ord:isx2 jma:ITOK
# ORD's two plan policies, run while an ltlau cycle is uploading (about x1:15 to x4:15 past each 5 minutes)
cargo run --release -p recast-radar-data --example feeds_plans -- plan ord:ltlau ord-complete:ltlau
# bejab's default frame and its velocity scan alone
cargo run --release -p recast-radar-data --example feeds_plans -- plan ord:bejab ord-vscan:bejab
# download one frame per source into the upstream cache (writes frame.json), then decode
cargo run --release -p recast-radar-data --example feeds_plans -- fetch ~/radar-corpus/feeds dwd:boo chmi:ska
cargo run --release -p recast-radar-data --example feeds_plans -- fetch ~/radar-corpus/feeds dwd-dbzh:boo
cargo run --release -p recast-radar-data --example feeds_plans -- fetch ~/radar-corpus/feeds --parts 1 ord:frlep
cargo run --release -p recast-radar-data --example feeds_plans -- imgw ~/radar-corpus/feeds leg
cargo run --release -p recast-radar-data --example feeds_survey -- --merge dwd:boo ~/radar-corpus/feeds/dwd/boo/*
# a polling directory, into the upstream cache
cargo run --release -p recast-radar-data --example feeds_plans -- poll ~/radar-corpus/feeds/polling https://mesonet-nexrad.agron.iastate.edu/level2/raw/FWLX
cargo run --release -p recast-radar-data --example feeds_plans -- poll ~/radar-corpus/feeds/polling http://offsitevpn.ewradar.com/Laredo/archive2.trans
# Level II structure of a downloaded volume
cargo run --release -p recast-radar-data --example feeds_survey -- ~/radar-corpus/feeds/polling/FWLX/*
# the known failures
cargo test --release -p recast-radar-data --test feeds_known_failures
# the polling parsers' fuzz target, from fuzz/ (Linux or nexbench, nightly, cargo-fuzz; seeds from `fuzz-tools seeds`)
cargo +nightly fuzz run polling_listing seeds/polling_listing -- -max_total_time=300
```

The Python side (`tools/feeds_survey/`, run with `~/radar-ref-venv`; `common.py` lists the
environment variables for the upstream cache root, the output directory, default `target/feeds-survey/`,
and the `feeds_survey` binary). Every step reads local files:

```sh
python tools/feeds_survey/run_upstream_survey.py  # 1. upstream frames                      (section 2.1)
python tools/feeds_survey/odim_crosscheck.py      # 2. ODIM against h5py                    (section 3.1)
python tools/feeds_survey/fixture_goldens.py      #    Py-ART on the KXWA files (section 4, the tests' goldens)
python tools/feeds_survey/testdata_budget.py --cap 70000000  # committed testdata across the stream branches (section 4)
```

Steps 1 and 2 on the cached files reproduce this page's numbers (step 1 decodes only frame directories,
those with a `frame.json`, and IMGW's images, and includes the `dwd-dbzh` frame, which is why step 2
counts 153 ODIM files). Be a light client of every server: fetch one frame per source, wait at least a
second between requests, and cache.
