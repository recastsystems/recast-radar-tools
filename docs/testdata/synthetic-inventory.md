# Synthetic test inputs

Tests in recast-radar-tools must read real radar files (spec section 5, plan stream C). This page lists
every test, helper and data file in the workspace that still feeds synthetic input, and proposes a real
replacement for each one: a corpus file (manifest id) and an independent source for the expected values.

**State after C.3 (merge of the eight conversion branches and `main`): the allowlist is empty.** The
detector's output on branch `real-tests` at C.1 (from `main` at `c279db3`) was **376 entries** (274
tests, 98 helpers and 4 data files), every one allowlisted as `pending` under the conversion group (plan
task C.2) that owned it. C.2 converted all 376 to real inputs (the group sections below record each
test's real input and assertion source) and kept no exception. C.3 then merged `main` (safety, Level II
completeness, level3-polish, packaging, data-access, l2-fixes and perf), whose new tests raised 76
findings; the section [C.3: findings in tests merged from `main`](#c3-findings-in-tests-merged-from-main)
lists how each was converted. `cargo test -p recast-radar-testdata --test no_synthetic` passes with
zero findings and zero entries.

## Enforcement

`crates/recast-radar-testdata/tests/no_synthetic.rs` runs the detector in
`crates/recast-radar-testdata/src/synthetic/` over the workspace and fails when:

- a finding is not in `testdata/synthetic-allowlist.toml` (the failure prints ready-to-paste tables),
- an allowlist entry no longer matches a finding (delete it after converting),
- an entry sits under the wrong group, is duplicated, or has an empty justification.

To list every current finding as allowlist tables:

```text
RECAST_RADAR_SYNTHETIC_REPORT=findings.toml cargo test -p recast-radar-testdata --test no_synthetic
```

### What is scanned

- Rust test code: every item in `crates/*/tests/*.rs` (and modules they declare), and `#[cfg(test)]` items,
  `#[test]` functions and out-of-line test modules under `crates/*/src/`. Library code, examples and
  benches are not test code. `fuzz/` is outside `crates/` and is not scanned.
- Data files under `crates/` and `testdata/`.

A finding is one item or one data file. Items are named by their inline module path within the file
(`tests::helper`, `tests::Synth::build`); nested functions and closures belong to the enclosing item.
Allowlist entries do not record line numbers, so edits elsewhere in a file do not invalidate them.

### Rules

| rule | flags | suppressed by real-data evidence |
|---|---|---|
| `synthetic-name` | a helper, constant, type or nested function named as a fabricator: contains `synth`, `fake`, `fabricat`, `mock`, `dummy`, `handcraft`, or is a `build_/make_/write_/encode_/gen_..._<radar noun>` builder (`write_zip`, `make_volume`). `#[test]` function names are not checked | no |
| `byte-encoding` | `to_be_bytes`/`to_le_bytes`/`to_ne_bytes` in an item that writes into a buffer (`extend_from_slice`, `copy_from_slice`, `push`, `write_all`, `collect`, ...), or a byte-string literal or `.as_bytes()` written straight into a buffer | yes |
| `magic-literal` | a byte-string literal starting with a format signature: HDF5 (`\x89HDF`, `TREE`, `OHDR`, ...), netCDF (`CDF\x01`), Archive II (`AR2V`, `ARCHIVE2`, `RVOL`, `DREF`, ...), Level III (`SDUS`), DORADE (`SSWB`, `VOLD`, `RADD`, `RDAT`, ...), GRIB (`GRIB`, `7777`), JMA members, `ustar`, zip, gzip, bzip2 | yes |
| `model-construction` | hand construction of radar model values: a `RadarVolume`, `ElevationCut`, `Radial`, `MomentGrid` or `RayInstrumentMetadata` built with `::new*`, `::default`, `::empty` or a struct literal; `.push_cut`, `.find_or_insert_cut`, `.push_row`, `.push_u8_row_slice`, `.push_u16_be_row_bytes`; and, inside their own crates, `TiltField` (correct), `Field` (bench), `PolarVelocityField` (retrieve), `StormCell` and `TdsGate` (track), `GroupableSweep` (io-dorade), `VwpProduct` (io-nexrad). Patterns (`Radial { a, .. } =`), return types and `Type::from_*` conversions are not flagged | no |
| `gate-field` | a polar field allocation `vec![<float>; rows * gates]` (dimension names such as `rows`, `rays`, `radials`, `azimuths`, `gates`, `bins`) | yes |
| `synthetic-data-file` | `include_bytes!`/`include_str!` of a flagged data file | no |
| `uses-synthetic` | an item that calls or names a flagged item of the same file (or test crate), transitively | no |
| `synthetic-file-name` | a data file named `*synth*`, `*fake*`, `*mock*`, ... | no |
| `unmanifested-binary` | a binary file in a `tests`, `data`, `fixtures` or `testdata` directory whose sha256 is not in any `testdata/**/manifest.toml` | no |
| `synthetic-generator` | a script under a `tests`/`data`/`fixtures` directory that says it generates synthetic data or is named as a generator | no |

Real-data evidence is `recast_radar_testdata`, `require_file!`, `include_bytes!`/`include_str!` of an
unflagged file, `fs::read*`, `File::open` or a `*_from_path(..)` call, in the item or in a helper it uses.
It marks byte edits as mutations of real bytes, so corruption tests built on real files pass without an
entry.

When the model types are renamed (stream F, FM301), add the new names to `MODEL_TYPES` in
`crates/recast-radar-testdata/src/synthetic/rules.rs`; otherwise the renamed constructors stop matching
and their entries fail as stale.

## Converting an entry

1. Load the real input with `recast_radar_testdata::require_file!("<id>")` (or `bytes`/`path`). Committed
   ids (`files/...` in the manifest) work offline; download ids skip the test when offline.
2. Take expected values from an independent source, never from the crate under test: golden values
   produced by a committed script under `tools/` (as `tools/validate_trimmed.py` does) with the Python
   venv (Py-ART 2.2.5, MetPy 1.7.1, xradar 0.12, h5py, netCDF4, scipy), values read directly from the
   file, or a published reference.
3. Edge and corruption cases mutate real bytes, and the test shows the mutation.
4. Delete the test's allowlist entry, then the entries (and code) of helpers nothing uses any more, then
   synthetic data files nothing includes.
5. "Needs corpus addition" means the input is not in the corpus yet: add it to a manifest first
   (docs/testdata/corpus.md, "Adding or changing an entry").
6. An entry may stay only as `status = "exception"` with a justification: a pure math or geometry helper
   that does not consume radar data, a fuzz regression input, or a corruption helper that starts from real
   bytes.

Names used for assertion sources below:

- **MetPy**: `metpy.io.Level2File` on the same file (headers, VOL block, moment data headers, raw codes).
- **Py-ART**: `pyart.io.read_nexrad_archive` / `read_cfradial` field arrays; Py-ART algorithms by name
  (`pyart.correct.dealias_region_based`, `pyart.retrieve.kdp_vulpiani`, `vad_browning`,
  `composite_reflectivity`, `compute_cdr`, `storm_relative_velocity`, `pyart.filters.GateFilter`).
- **xradar**: `xradar.io.open_odim_datatree` / `open_cfradial1_datatree`.
- **Python walker**: a small reader written for the golden script, independent of the Rust decoder (DORADE
  blocks as in `crates/recast-radar-io-dorade/tests/dorade_real.rs`; GRIB2 sections for JMA, because the
  venv has no eccodes, cfgrib or pygrib).
- **Py-ART region-based**: `pyart.correct.dealias_region_based` on the same sweep, compared modulo one
  global 2N offset.

Where a row quotes how many gates Py-ART region-based unfolds, or where a sweep's strongest echo is, the
number was measured for this page with Py-ART 2.2.5 on the file (a gate counts as unfolded when its value
changed by more than the sweep's median Nyquist velocity). Trimmed fixtures keep only part of the circle,
so a row names the full volume when the feature is outside the trimmed sector.

## Counts by group

C.1 entries per group, all converted in C.2 (branch `real-tests-<group>`), and the C.3 findings in the
tests `main` added meanwhile (all converted in C.3). The crates `main` added after C.1 belong to the
group of the crates they serve (`GROUPS` in `crates/recast-radar-testdata/src/synthetic/mod.rs`):
`recast-radar-bzip2` and `recast-radar-io-level3` to io-nexrad, the `recast-radar-tools` facade to
io-formats.

| group | crates | C.1 entries (tests / helpers / data files) | remaining | C.3 findings from `main` |
|---|---|---:|---:|---:|
| io-nexrad | `recast-radar-io-nexrad`, `recast-radar-io-level3`, `recast-radar-bzip2` | 32 (21 / 11 / 0) | 0 | 46 |
| io-formats | `recast-radar-io-odim`, `recast-radar-io-cfradial`, `recast-radar-io-dorade`, `recast-radar-io-jma`, `recast-radar-io`, `recast-radar-tools` | 79 (45 / 30 / 4) | 0 | 22 |
| correct | `recast-radar-correct` | 32 (28 / 4 / 0) | 0 | 0 |
| filters-map | `recast-radar-filters`, `recast-radar-map` | 36 (28 / 8 / 0) | 0 | 0 |
| retrieve | `recast-radar-retrieve` | 48 (35 / 13 / 0) | 0 | 0 |
| track | `recast-radar-track` | 30 (22 / 8 / 0) | 0 | 0 |
| render-bench | `recast-radar-render`, `recast-radar-bench` | 32 (27 / 5 / 0) | 0 | 0 |
| core-data-scattering | `recast-radar-core`, `recast-radar-data`, `recast-radar-scattering`, `recast-radar-testdata` | 87 (68 / 19 / 0) | 3 (FM301 model: 2 pending, 1 exception; see below) | 8 |
| **all** | | **376 (274 / 98 / 4)** | **3** | **76** |

## io-nexrad

### `crates/recast-radar-io-nexrad/src/lib.rs`

**Converted (C.2, branch `real-tests-io-nexrad`); no allowlist entries remain.** The hand-assembled
Archive II volumes and all 11 helpers are deleted. The tests in `tests` read real corpus files and compare
the decode with `testdata/level2/golden/decode/<name>.json`, written by `tools/level2_decode_golden.py`
(a byte walker, MetPy 1.7.1 `Level2File` and Py-ART 2.2.5 `NEXRADLevel2File`; the script exits with an
error unless the three agree, and `--check` reproduces the committed JSON). Each golden holds the volume
header, LDM record framing, message headers before the first radial, the first radial's header and block
pointers, site and VCP, and per sweep the radial count, statuses, angles, Nyquist velocities and, per
moment, the gate layout, scaling, count and sum of valid raw codes, scaled sum, minimum, maximum and
sampled gates.

| test (new name) | real input | what is compared |
|---|---|---|
| `tests::parses_archive_volume_header` | `l2-ktlx-20240315-000217-trim`, `l2-ktlx-19910605-162126-trim` (blank ICAO), `l2-ktlx-19990504-002218-trim` (NUL ICAO) | version, ICAO and time against the header bytes and MetPy |
| `tests::parses_message_header` | `l2-ktlx-20240315-000217-trim`, `l2-ktlx-20130520-201643-trim`, `l2-ktlx-19910605-162126-trim` | every non-empty message header up to the first radial (walker); a header cut off by the end of the data is an error |
| `tests::parses_message_31_header` | `l2-ktlx-20240315-000217-trim` (72-byte header), `l2-ktlx-20130520-201643-trim` (68-byte header) | first radial header and the ten pointer words (walker, MetPy) |
| `tests::decodes_message_31_volume` (was `decodes_synthetic_message_31_volume`) | `l2-ktlx-20240315-000217-trim` | whole volume against the golden |
| `tests::decodes_legacy_message_1_reflectivity_and_velocity` | `l2-ktlx-19990504-002218-trim` | whole volume; Nyquist 26.1 m/s on the Doppler cut |
| `tests::decodes_legacy_message_1_spectrum_width_with_velocity_offset` | `l2-ktlx-19990504-002218-trim` | SW = (code - 129) / 2 on sampled gates and the full SW summary |
| `tests::decodes_16_bit_moments` (was `decodes_synthetic_16_bit_moment`) | `l2-ktlx-20130520-201643-trim` (PHI 16-bit, ZDR 8-bit), `l2-ktlx-20240315-000217-trim` (both 16-bit) | word sizes and PHI/ZDR summaries |
| `tests::decodes_every_trimmed_fixture` (replaces the environment-gated `decodes_real_public_level2_file_from_env`) | the 16 `trimmed` fixtures | whole volume against the golden |
| `tests::decodes_gzip_stream_without_normalized_buffer` | `l2-kpah-20080415-235014` (gzip; see note) | streaming reader equals the buffered decode; whole volume against the golden |
| `tests::gzip_preview_waits_for_complete_displayable_cut` | `l2-ktlx-19990503-230052` (ends 68 radials into its first cut) | no preview; 1 cut of 68 radials |
| `tests::gzip_preview_returns_completed_displayable_cut` | `l2-kpah-20080415-235014` | preview is the first cut (360 radials, ends with status 2); none when more radials are required |
| `tests::gzip_preview_callback_continues_to_full_volume` | `l2-kpah-20080415-235014` | one callback with sweep 0; full volume (2520 radials) against the golden |
| `tests::decodes_bzip_blocks_without_concatenated_normalized_buffer` | `l2-ktlx-20240315-000217-trim` | whole volume against the golden |
| `tests::bzip_preview_waits_for_complete_displayable_cut` | `l2chunk-kiwa-307-20260917-003629-001-s` + `-002-i` | no preview; 120 radials against the golden |
| `tests::bzip_preview_returns_completed_displayable_cut` | `l2-ktlx-20240315-000217-trim`; `l2-ktlx-20240315-000217` (downloaded) | trim: preview when sweep 2 starts; full volume: preview at the end-of-elevation radial (720) |
| `tests::bzip_preview_full_decode_reuses_path_and_returns_full_volume` | `l2-ktlx-20240315-000217-trim` | one callback (480 radials); result equals the plain decode |
| `tests::multi_block_bzip_decode_matches_uncompressed_reference` | `l2-ktlx-20240315-000217-trim` | LDM decode equals the decode of the records decompressed in the test and framed uncompressed, and of the same stream re-blocked every 1,000,003 bytes (records and messages split across blocks) |
| `tests::bzip_preview_fires_past_legacy_block_window` | `l2-ktlx-20240315-000217-trim` with record 1 split into 20 records of 6 radials (28 records) | preview fires once (480 radials); decode equals the original file's |
| `tests::corrupt_trailing_bzip_block_yields_partial_volume` | `l2-ktlx-20240315-000217-trim` with the last record's bzip2 data zeroed after its magic | 480 + 360 radials, as MetPy reads the file without that record; one more skipped message |
| `tests::corrupt_first_bzip_block_is_a_hard_error` | `l2-ktlx-20240315-000217-trim` with the metadata record's bzip2 data zeroed | compression error |
| `tests::pipelined_decode_works_on_single_thread_rayon_pool` | `l2-ktlx-20240315-000217-trim` | 1-thread decode equals the default decode and the golden |
| `tests::decodes_gr2_style_variable_framed_msg31_records` | `l2-ktlx-20240315-000217-trim` cut to the GR2 layout: volume header, the Message 2 and 5 records, then every Message 31 back to back (see note) | 480 + 480 radials as MetPy reads that layout; cuts equal the original decode; with the header date zeroed the time comes from the first radial |

Notes:

- The conversion found that Message 1 radials took the Nyquist velocity from bytes 46-47 (spare) instead
  of halfword 31 (bytes 60-61, ICD 2620002, as MetPy and Py-ART read it); the synthetic test had encoded the
  wrong offset. `parse_message_1` now reads bytes 60-61. Message 31 decoding is unchanged, and the bench
  checksums are identical.
- `l2-kvwx-20080415-235337`, proposed for the gzip tests, does not decode: its Message 31 radials carry a
  blank radar id, and `parse_message_31_header` rejects it ("empty message 31 id"), while MetPy and Py-ART
  read 2500 radials. The gzip tests use `l2-kpah-20080415-235014` (gzip, Message 31, 7 sweeps) instead;
  the KVWX decode failure is left to the Level II stream.
- There is still no real GR2 `.msg31` export in the corpus; the GR2 test uses real Message 31 bytes in the
  GR2 layout.


## io-formats

Converted in plan task C.2 (branch `real-tests-io-formats`): all 79 entries are gone from
`testdata/synthetic-allowlist.toml`, with no exceptions. The synthetic data files `cfrad_synth.nc`,
`gen_cfradial_fixture.py`, `odim_pvol_synth.h5` and `gen_odim_fixture.py` and every builder helper
(`tiny_cdf1`, `Synth`, `put_i16/i32/f32`, `base_block`, `synth_rays`, `synthetic_sweep`, `write_zip`,
`synthetic_jma_grib2*`, `tar_member_blocks`, `tar_archive`, `two_station_tar`, `push_u16/u32`,
`section`, `float_grid`, `copied_sentinel_cut`, `synthetic_archive_ii`, `synthetic_message_31_body`,
`push_volume_block`, `push_radial_block`, `push_u8_moment`, `set_pointer`, the `FIXTURE`,
`ODIM_SYNTH` and `CFRADIAL_SYNTH` includes) are deleted. Expected values come from
`tools/golden_io_formats.py` (sections `cfradial`, `odim`, `dorade`, `jma`, `router`); the key is named
in each test's comments.

Corpus additions made for this group (`testdata/other/manifest.toml`, recipes in
docs/testdata/corpus.md): `dorade-noxp-20090610-{003210,003222,003226}-ppi-head6` (three sweeps of one
multi-elevation NOXP volume), `dorade-noxp-20090610-003210-heads-zip` (that archive directory as a zip)
and `odim-au24-20260610-000300-nci-zip-member` (an unmodified NCI THREDDS response).

| file | test (renamed from) | real input | assertion source |
|---|---|---|---|
| `recast-radar-io-cfradial/src/netcdf3.rs` | `tests::magic_sniffer_accepts_classic_versions` | xsapr classic and Irene bytes; xsapr version byte set to 2, 5, 3, 0; xsapr netCDF-4 | netCDF classic specification |
| | `tests::parses_real_classic_cdf1_header_and_record_variables` (`parses_handcrafted_cdf1`) | `cfrad1-xsapr-sgp-20110520-ppi-classic` (UNLIMITED `time`, 40 records) | netCDF4-python: dims, attributes, raw record-variable values |
| | `tests::parses_real_packed_int8_fixed_dimension_variables` (new) | `cfrad1-irene-sr2-20110827-120420-sur-sweeps01` | netCDF4-python: int8 DBZ attributes and raw codes |
| | `tests::cdf5_is_rejected_with_guidance` | xsapr classic with the version byte set to 5 | error text |
| | `tests::rejects_absurd_header_counts_before_allocating` | xsapr classic with the dimension count at offset 12 set to `u32::MAX` | header layout checked in the test (NC_DIMENSION tag, count 4) |
| `recast-radar-io-cfradial/tests/cfradial_real.rs` | `decodes_real_irene_cfradial1_volume` (`decodes_synthetic_cfradial1_volume`) | Irene | Py-ART `read_cfradial` (site, sweeps, gates, masked values, valid counts), xradar sweep sizes and fixed angles, netCDF4 per-ray variables |
| | `record_variable_ray_metadata_decodes_from_unlimited_time` (new) | xsapr classic | netCDF4 per-ray `prt`, `unambiguous_range`, `nyquist_velocity`, fill gate |
| | `level2_decoder_is_not_fooled_by_netcdf_magic` | Irene and xsapr classic bytes | manifest format `cfradial1` |
| `recast-radar-io-dorade/src/dorade.rs` | `tests::decodes_big_endian_real_cow2_sweep` (`decodes_big_endian_synthetic_sweep`) | `dorade-cow2-20260521-225514-sur-head24` | Python DORADE walker: RADD, SSWB, SWIB, CSFD, ray status/azimuth/time, gate values and bad counts |
| | `tests::decodes_little_endian_rle_sweep` | `dorade-dow6-20211230-222139-rhi-head41` | Python walker (HRD RLE decode) |
| | `tests::decodes_little_endian_uncompressed_sweep` (new) | `dorade-noxp-20090525-203211-sector` | Python walker |
| | `tests::rejects_extended_parm_with_absurd_gate_count` | COW2 PARM at offset 1080 with `number_cells` set to `i32::MAX` | error text; block layout from the walker |
| | `tests::peek_reads_grouping_metadata_without_rays` | COW2, DOW6 RHI, NOXP 2009-06-10 0.5 deg head (descriptor bytes only) | Python walker |
| | `tests::multi_sweep_volume_sorts_cuts_by_elevation` | the three `dorade-noxp-20090610-*-ppi-head6` sweeps, passed out of order | Python walker fixed angles, times, CSFD, gate values |
| | `tests::mismatched_instruments_are_rejected` | COW2 with NOXP | RADD names |
| | `tests::rhi_scan_mode_is_detected_from_radd` | DOW6 RHI | Python walker: RADD scan mode 3, per-ray elevations and azimuths |
| | `tests::u16_grids_preserve_dorade_scaling` | COW2 | PARM scale, bias, bad value |
| `recast-radar-io-dorade/src/mobile_archive.rs` | `tests::groups_zip_members_into_ascending_elevation_runs_per_instrument` | `dorade-noxp-20090610-003210-heads-zip`; the three heads with COW2 in a folder | walker start times and fixed angles; member names |
| | `tests::same_elevation_sequences_become_one_volume_per_sweep` | `dorade-noxp-20090501-190244-ppi`, `-190324-ppi` in a folder | walker times and angles |
| | `tests::long_time_gap_splits_an_ascending_run` | NOXP 2009-05-01 0.5 deg with the 2009-06-10 1.0 deg head (and, as control, the 2009-06-10 0.5 and 1.0 deg heads) | walker times and angles |
| | `tests::rejects_archive_without_radar_members` | the NOXP zip with its three sweep names changed from `swp.` to `swp_` | error text |
| | `tests::loose_sweepfile_groups_directory_siblings_from_same_run` | three 2009-06-10 heads and the 2009-05-01 sweep in a folder | walker times and angles |
| | `tests::zip_sniffers_match_magic_and_extension` | the NOXP zip bytes and its end-of-central-directory record; COW2 and JMA paths | PKWARE APPNOTE signatures |
| `recast-radar-io-jma/src/lib.rs` | `tests::grid_axis_limits_reject_pathological_radial_tables` | section 3 of `jma-n5-20191012-090000-rs47773` (offset 37) with 2049 radials | Python GRIB2 walker |
| | `tests::oversized_tar_member_is_rejected_from_its_header` | TAKA N5 tar with its size field set over the member limit | ustar header |
| | `tests::sniffs_jma_tar_bytes` | TAKA N5 and N6 tars; the N5 header renamed; ODIM and Level II bytes | manifest formats |
| | `tests::cuts_sort_lowest_elevation_first_across_members` | the TAKA N5 member followed by the TAKA N6 tar | Python walker elevations (stable sort) |
| | `tests::decodes_single_station_member_with_real_gate_values` (new) | TAKA N5 and N6 tars | Python walker: site, geometry, per-ray elevations, run-length gate values, 547,108 valid N6 gates |
| | `tests::decodes_every_station_in_archive_order` | `jma-n6-20191012-090000` (full) | ustar member order, PDT 4.51022 station fields |
| | `tests::site_filter_selects_one_station_by_id_or_number` | `jma-n5-20191012-090000` (full) | Python walker |
| | `tests::first_station_decode_takes_the_first_member_only` | full N5 (MURO first) and N6 (AKIT first) | ustar member order |
| | `tests::station_headers_skip_gate_data_and_dedupe` | TAKA N5 + N6; full N5 members followed by the full N6 tar | Python walker |
| | `tests::repeated_station_members_merge_into_one_volume` | TAKA N5 + N6 | Python walker sweep counts |
| | `tests::corrupt_member_is_skipped_but_alone_is_an_error` | TAKA N5 with its GRIB indicator zeroed, renamed, and before the N6 member | error text; 13 surviving velocity cuts |
| | `tests::corrupt_station_in_full_archive_is_skipped` (split out) | full N6 with MURO's GRIB indicator zeroed | 19 stations in member order |
| | `tests::truncated_tar_member_is_an_error_not_a_panic` | TAKA N5 cut inside the member | error text |
| `recast-radar-io-odim/src/hdf5lite.rs` | `tests::magic_sniffer_matches_signature_only` | `odim-bejab-20190606-0000-pvol` (and 7-byte prefix), xsapr netCDF-4 and classic, a Level II trim | HDF5 superblock signature |
| | `tests::v1_object_header_rejects_continuation_cycle` | bejab root object header (address 96) with its continuation pointed at its own first block | h5py `h5o.get_info` address and message counts; v1 header layout from the HDF5 specification |
| | `tests::btree_walks_reject_self_references` | bejab root group B-tree (136) and `dataset1/data1/data` chunk B-tree (3440), each raised to level 1 with child 0 set to itself | h5py chunk offset and size, 14 root children |
| `recast-radar-io-odim/src/odim.rs` | `tests::copied_whatgroup_recovery_masks_only_no_echo_offset_gates` | `odim-espdg-20260707-1927-pvol-dbzh-vradh`, recovery applied to the edited file's unrecovered 0.5 deg plane with the file's real `what` sentinels | h5py raw planes: fill and genuine-zero gate counts and index sums |
| | `tests::distinct_velocity_sentinels_are_never_reflectivity_gated` | espdg with dataset2 VRADH `what/nodata` rewritten to -9999 and its v2 object-header checksum recomputed (libhdf5 reads the edited file) | h5py on the edited file |
| `recast-radar-io-odim/tests/odim_real.rs` | `decodes_real_iesha_pvol` (`decodes_synthetic_odim_pvol`) | `odim-iesha-20260305-0115-pvol` | h5py attributes and raw planes; xradar sweep sizes, range and azimuth |
| | `non_odim_hdf5_is_rejected_with_guidance` | xsapr netCDF-4 (superblock 2); `odim-imgw-ram-20260711-0015-kdp-max` (IMAGE) | error text |
| `recast-radar-io/src/lib.rs` | `tests::sniffs_supported_volume_formats_in_router_order` | committed DORADE, ODIM, netCDF-4, CfRadial, JMA, Level II and NCI zip files; mutated CDF-3 and renamed JMA header | manifest formats |
| | `tests::sniffs_gzip_archive_and_generic_tar_as_level2_fallthrough` (new) | `l2-kvwx-20080415-235337` (gzip); first tar header of `dorade-noxp-20090501-sweeps-tgz` | gzip and ustar signatures |
| | `tests::unwraps_zip_local_member_stream_without_central_directory` | `odim-au24-20260610-000300-nci-zip-member` | `struct` + `zlib` unwrap, CRC-32, member sha256, h5py |
| `recast-radar-io/tests/router_real_files.rs` | `router_matches_direct_odim_decoder_on_real_pvols` | bejab, bewid, norst, espdg, iesha, dkrom | routed equals direct; NOD site ids |
| | `router_matches_direct_cfradial_decoder_on_classic_netcdf` | xsapr classic, DOW8 trim3, Irene | routed equals direct |
| | `image_decoder_and_volume_router_remain_separate` | IMGW KDP IMAGE; iesha PVOL | error texts |
| | `router_decodes_real_archive_ii_same_as_direct_decoder` (`..._synthetic_...`) | `l2-ktlx-20240315-000217-trim`, `l2-ktlx-19990504-002218-trim`, `l2-kpah-20080415-235014` (gzip) | routed equals direct; Py-ART and MetPy radial counts per sweep |
| | `router_matches_direct_archive_ii_decoder_on_real_volumes` (`..._synthetic_volume`) | same | MetPy station ids |

Deviations from the proposals above them in C.1:

- `l2-kvwx-20080415-235337` was not decodable by `recast_radar_io_nexrad::decode_volume_from_bytes`
  (now `read_volume_from_bytes`) when C.1 ran ("empty message 31 id"; decodable since `09c8d1e`),
  so the gzip Archive II leg of the router tests uses `l2-kpah-20080415-235014`; KVWX is still used
  for gzip sniffing.
- An unfiltered `read_jma_tar_volumes` of the full N5 tar is refused (150,528,000 grid points against
  the 67,108,864-point `MAX_POINTS_PER_DECODE`), so the all-station and corrupt-member archive tests use
  the full N6 tar (44,032,000 points); N5 is exercised with a site filter and header-only paths.
- `odim-dkrom-20260820-1130-pvol` cannot stand for distinct velocity sentinels: its VRAD `what`
  sentinels (255/0) equal DBZH's. The distinct case edits one espdg attribute instead.
- `v1_object_header_rejects_continuation_cycle` and `btree_walks_reject_self_references` take their
  offsets from h5py addresses plus a version-1 header reader in the golden script, not from h5py alone.


## correct

Converted in C.2 (branch `real-tests-correct`): all 28 tests read real Level II volumes and the 4
helpers (`velocity_cut`, `wind_cut`, `tilt_with_uniform_wind`, `test_velocity_grid_rows`) are deleted;
the group has no allowlist entries. One test was added (`v4_passes_nyquist_less_tdwr_through`) and
three were renamed because their real-data assertions no longer match the synthetic names.

Shared pieces:

- `crates/recast-radar-correct/src/real_data.rs` (test-only): decoded corpus volumes, the velocity
  sweep as the solvers see it, and the goldens.
- `tools/correct_golden.py` writes `crates/recast-radar-correct/tests/golden/<case>.txt` (13 sweeps,
  about 515 KB of text): Py-ART 2.2.5 `dealias_region_based` folds per gate, run-length encoded, plus
  per-ray valid-gate count, gate-position sum and raw-velocity sum. Every test first checks those
  per-ray sums against its own decoded rows, so the folds are compared gate for gate. For sweeps with
  an HRRR/RAP fixture (`crates/recast-radar-bench/fixtures/dealias/`), `env_offset` is the global fold
  that puts Py-ART's output closest to the projected model wind; Py-ART's folds plus that offset are
  the absolute-branch reference. Regenerating the goldens reproduces them byte for byte.
- Corpus additions: `l2-klix-20210829-175748` (the volume before `l2-klix-20210829-180425`) and
  `l2-klix-20210829-173117` (33 min before), for the temporal-prior tests.
- `l2-klix-20050829-130035-trim` (Katrina) is not used: on `real-tests`, `recast-radar-io-nexrad` reads
  the Message 1 Nyquist velocity at body offset 46 (a spare field, always 0) instead of offset 60, so
  every Message 1 radial decodes with `nyquist_velocity_mps = None` and every engine passes legacy
  velocity through (Py-ART and MetPy read 32.1 m/s; offset 60 holds 3210 on all 362 Doppler radials of
  the file). Branch `real-tests-io-nexrad` (d075340) moves the read to offset 60; after C.3 the Katrina
  sweep can join the goldens (`tools/correct_golden.py` needs only a new `CASES` entry).

| test | real input | assertion (measured value) |
|---|---|---|
| `lib.rs` `lightweight_velocity_dealias_unfolds_radial_continuity` | `l2-kdvn-20200810-180401-trim`, `l2-kbox-20220129-150537-trim` Doppler cuts | along-ray raw jumps > N that Py-ART makes continuous, also made continuous by the region engine: >= 89% (3,402 of 3,735) and >= 99% (6,672 of 6,713) |
| `lib.rs` `dealias_skip_detection_reports_nyquist_less_feeds` | `l2-tstl-20230331-230314-trim` (Nyquist 0 per Py-ART), `jma-n6-20191012-090000-rs47773` (13 sweeps), KDVN trim as control; Nyquist edits of the decoded TDWR cut | skip reported, output equals input within 0.05 m/s on every gate; JMA valid gates = 547,108 (manifest, GRIB2 walker); control not skipped |
| `lib.rs` `region_dealias_recovers_smooth_folded_ramp` | KBOX trim; `l2-klix-20210829-180425` 0.48 deg cut at 23.2 m/s | fold agreement with Py-ART modulo one global offset >= 99.7% / 99.5% (99.835%, 99.629%); breaks where Py-ART is continuous <= 0.1% / 0.05% of pairs |
| `lib.rs` `region_dealias_does_not_propagate_errors_down_a_radial` | `l2-klix-20210829-180425-trim` (Py-ART moves 13 of 63,544 gates) | gates Py-ART keeps that the engine moves <= 12 (6); longest run along a ray <= 4 (3) |
| `lib.rs` `region_dealias_is_deterministic_across_runs` | KDVN trim | 16 runs byte-identical; > 4,000 gates moved (4,845) |
| `lib.rs` `region_dealias_unfolds_geometrically_supported_fold` | KDVN trim, the 18 Py-ART patches of >= 6 gates enclosed by dominant-branch gates | patch gates >= 97% (97.5%), ring gates >= 97.5% (98.4%) on Py-ART's branch |
| `lib.rs` `external_harmonic_reference_selects_the_absolute_branch` | Ida full volume: reference fitted on the 32.1 m/s cut, applied to the 23.2 m/s cut | absolute agreement >= 99.8% (477,793 of 478,616) and >= 500 gates better than without (476,841) |
| `lib.rs` `velocity_dealias_preserves_supported_adjacent_folds` | KBOX trim, enclosed Py-ART patches spanning >= 3 rays (46) | patch and ring gates >= 99% (100%) |
| `merge.rs` `bridged_pairs_resolve_an_isolated_island_across_a_gap` | KDVN 0.48 deg (18 aliased islands) and Ida 23.2 m/s cut (8) | merge on Py-ART's branch >= 90% (773 of 811, 599 of 605); plain vote graph <= 20% (74, 3) |
| `merge.rs` `aggregation_preserves_an_embedded_shear_couplet` | `l2-ktlx-20130520-201643-trim`, Moore couplet (Py-ART max opposite-sign azimuthal shear 10-40 km: 98.0 m/s) | merge couplet shear >= Py-ART's (120.7 m/s) within 4 rays / 8 gates |
| `merge.rs` `uncorroborated_bridge_welds_without_unwrapping` | KDVN 0.48 deg, 1-2 gate specks whose bridge partners mostly differ by > N (36) | merge keeps >= 95% on a partner's branch (36); Py-ART unwraps >= 75% (33) |
| `merge.rs` `merge_solve_is_deterministic` | KDVN trim | two solves identical |
| `mod.rs` `v4_temporal_reference_recovers_a_topmost_aliased_high_tilt` | Ida 1.80 deg tilt alone, previous volume `l2-klix-20210829-175748` | prior used; absolute agreement >= 99.4% (159,902 of 160,762) and >= 300 gates better than without (159,448) |
| `mod.rs` `v4_lower_current_tilt_can_reference_a_folded_higher_tilt` | Ida cuts up to 1.80 deg vs the 1.80 deg tilt alone | >= 99.4% (160,066) and >= 300 better (159,448) |
| `mod.rs` `v4_current_lower_tilt_fixes_an_isolated_high_tilt_branch` | Ida 1.80 and 2.42 deg tilts, isolated echo of 64-1,000 gates | volume solve >= 94% / 90% (1,821 of 1,903; 980 of 1,071), >= 15 / 5 points above the tilt alone (1,389; 896); no invented gates |
| `mod.rs` `v4_repairs_folded_patches_inside_inbound_flow` (was `..._fused_into_legitimate_inbound`) | KDVN full volume, 0.48 deg cut | agreement >= 93.5% (94.2%), per echo >= 98.5% (98.9%), >= 8 points above the region engine (82.9%); inbound enclosed patches >= 70% (121 of 156) |
| `mod.rs` `v4_stale_temporal_volume_is_ignored` | Ida 1.80 deg tilt with `l2-klix-20210829-173117` (33 min) and `-175748` | stale: prior unused, grid and confidence byte-identical to none; fresh: used, grid differs |
| `mod.rs` `v4_rebranches_onto_the_absolute_branch_only_with_environmental_evidence` (was `v4_weak_edge_subgraph_...`) | `l2-ktlx-20130520-201643` + `env_ktlx.json` (RAP 20Z) | gates the profile rebranches >= 40 (53); with profile >= 85% absolute (49), without <= 15% (4); no-profile confidence never above interior-only |
| `mod.rs` `v4_environment_decides_the_absolute_branch_on_both_tilts` (was `v4_branch_degenerate_...`) | Moore volume + RAP | both lowest Doppler tilts >= 99.8% / 99.7% absolute (99.90%, 99.84%); Py-ART's branch within N of RAP on >= 99% |
| `mod.rs` `v4_stale_environment_profile_is_ignored` | Ida trim + `env_klix_hrrr.json` with `valid_time` moved 4 h earlier | identical to no profile; unedited profile used |
| `mod.rs` `v4_solve_is_deterministic_across_runs` | Ida full volume + HRRR | 19 velocity tilts (Py-ART count); grids, confidence and diagnostics identical |
| `mod.rs` `v4_confidence_grid_reflects_decision_margins` | Ida full volume + HRRR, 4 tilts | gates above interior-only >= 90% of valid and >= 99.5% absolute (100%, 100%, 99.97%, 99.70%), more often right than the rest |
| `mod.rs` `v4_passes_nyquist_less_tdwr_through` (new) | TSTL trim | pass-through within 0.05 m/s |
| `repair.rs` `meso_couplet_survives_untouched` | Moore trim from Py-ART's solution | couplet masked; gates within the 2-gate dilation of the couplet pair unchanged |
| `repair.rs` `patch_repair_closes_its_ring` | Ida 0.48 deg cuts: isolated smooth one-branch regions of 100-1,000 gates with no echo in couplet reach (2), shifted one interval, Py-ART reference with holes (25) | restored >= 99% (269 of 270), holes >= 90% (24), ring closure >= holes filled, boundary pairs only around unrestored gates |
| `repair.rs` `change_cap_aborts_the_patch_module` | KDVN trim, reference = observed + 2N | abort; patch/ring/revert counts 0; every change comes from other modules; changes below the cap |
| `repair.rs` `box_median_ladder_snaps_speckle_and_converges` | `l2-pahg-20250909-212549` 0.48 deg: Py-ART's 1-2 gate enclosed specks reset (159) | >= 93% snapped back (153), confidence demoted; a second pass leaves the folds unchanged |
| `super_regions.rs` `single_contact_edge_is_weak_and_splits_super_regions` | KDVN trim region solve (3,285 regions) | partition = independent union-find over the documented strong rule; >= 1,000 single-contact edges (1,332), all across super-regions; weak edge list as documented |
| `super_regions.rs` `high_support_edge_welds_a_super_region` | KBOX trim region solve | every strong edge (118) welds; its fold vote equals Py-ART's fold difference >= 99% (100%) |


## filters-map

**Converted in C.2** (branch `real-tests-filters-map`): all 36 entries are gone from the allowlist
(the counts table above is the C.1 snapshot). The 28 synthetic tests and 8 helpers were deleted from
`src/`; their replacements are integration tests that decode corpus files with the workspace readers
and compare against JSON goldens under `testdata/golden/filters/` and `testdata/golden/map/`, written
by `tools/filters_map_golden.py`. The script reads the same files with MetPy 1.7.1 (Level II),
Py-ART 2.2.5 (gate filter, region-based dealiasing), netCDF4 1.7.4 (CfRadial) and its own DORADE
block walker, and computes expected outputs with numpy reference implementations of the documented
algorithms (float32 arithmetic where the Rust code uses f32). The pure-math unit tests that stayed in
`src/` (`interpolate.rs` factor policy, `rhi.rs` beam-geometry round trip) were never findings.

### `crates/recast-radar-filters/tests/gate_filter_real.rs`

| test | real input | assertion source |
|---|---|---|
| `keeps_velocity_only_where_reflectivity_clears_the_threshold` | `l2-ktlx-20240315-000217-trim` sweep 2 (REF/VEL/SW, 88,294 velocity gates) at 0, 10 and 20 dBZ | Py-ART `GateFilter.exclude_below('reflectivity', t)` on Py-ART velocity: kept gates and velocity sum per ray |
| `no_reflectivity_moment_blanks_everything` | `l2-ktlx-19990504-002218-trim` sweep 2 (Message 1 Doppler, VEL/SW only) and all 13 sweeps of `jma-n6-20191012-090000-rs47773` | MetPy valid velocity count (111,458); JMA non-missing count from the corpus walker (547,108); every output gate empty |

### `crates/recast-radar-filters/tests/smooth_real.rs`

| test | real input | assertion source |
|---|---|---|
| `uniform_field_is_unchanged` | `l2-kdvn-20200810-180401-trim`, `l2-ktlx-20130520-201643-trim`, `l2-ktlx-19990504-002218-trim` sweep 1 REF | gates whose valid 3x3 neighbours equal the centre (MetPy values) keep it; per-row reference |
| `steps_soften_and_coverage_does_not_grow` | `l2-kdvn-20200810-180401-trim` sweep 1 REF | per-row coverage equals MetPy's valid gates; the 50 steepest steps lie strictly inside their neighbourhood at the numpy reference value |
| `interior_step_blends` | `l2-ktlx-20130520-201643-trim` and `l2-ktlx-19990504-002218-trim` (whole circle, azimuth wrap) | numpy binomial reference on MetPy reflectivity: steepest 50 gates and every row (count and sum) |

### `crates/recast-radar-filters/tests/interpolate_real.rs`

| test | real input | assertion source |
|---|---|---|
| `geometry_subdivides_exactly` | `l2-ktlx-19990504-002218-trim` sweep 1 REF (367 radials, 1 km gates) | MetPy azimuths and gate geometry: 4 x 4, annulus preserved, native rows at their azimuths, row azimuths and parent radials |
| `azimuth_wraps_between_last_and_first_row` | same | sub-rows across north and between the last and first radial (MetPy azimuths) |
| `uniform_field_is_unchanged_and_fine_grids_pass_through` | KTLX 1999 REF, `l2-klix-20050829-130035-trim` VEL, `dorade-noxp-20090525-203211-sector` DZ; `cfrad1-dow8-20211011-223602-rhi-trim3-classic` (passes through) | cells with four equal parents keep the value; netCDF4 azimuths and 125 m gates give identity factors |
| `coverage_does_not_grow` | KTLX 1999 REF, KLIX 2005 VEL, `l2-kgwx-20130601-235640` sweep 1 RHOHV, NOXP sector | numpy float32 reference: per-row coverage and sums; no cell without a valid nearest parent; blocked beam-boundary cells empty |
| `echo_edges_use_nearest_parent_not_partial_blends` | same four | reference edge cells equal the nearest parent's file value |
| `velocity_fold_guard_uses_nearest_parent` | KLIX 2005 sweep 2 VEL (Nyquist 32.1 m/s from MetPy) | guarded cells (parent spread > 30 m/s) equal the nearest parent; blended cells inside the parents' range |
| `cc_guard_never_blends_through_the_melting_layer` | KGWX 2013 sweep 1 RHOHV | guarded cells (a parent below 0.97) equal the nearest parent; blends only between parents >= 0.97 |
| `sector_scan_gap_stays_native` | NOXP sector (100 rays, 200-300 deg) | DORADE walker azimuths: 397 rows, none in the gap |
| `upsample_cost_smoke` | `l2-ktlx-19990504-002218` (all 16 sweeps, REF/VEL/SW) | output dimensions from MetPy geometry and the factor policy |

### `crates/recast-radar-map/tests/rhi_real.rs`

| test | real input | assertion source |
|---|---|---|
| `rhi_section_samples_the_matching_beam` | `cfrad1-dow8-20211011-223602-rhi-trim3-classic` (768 x 320 panel), `dorade-dow6-20211230-222139-rhi-head41` (400 x 200) | 4/3-Earth panel reference on netCDF4 / DORADE walker elevations and values: sampled pixels and per-row counts and sums |
| `rhi_section_is_empty_above_the_top_beam` | DOW8, DOW6 | reference pixels with no beam within 1 degree are empty |
| `rhi_section_is_empty_beyond_gate_coverage` | DOW8 (130 km panel) | reference pixels past the last gate are empty |
| `rhi_heuristic_accepts_elevation_sweeps_and_rejects_ppi` | DOW8 (`sweep_mode` rhi), DOW6 (RADD scan mode 3); trimmed KTLX 2024, KTLX 2013, KEWX 2016 sweeps (Py-ART `scan_type` ppi); all radials of `l2-ktlx-20130520-201643` in one cut; DOW8 thinned to 8 and 7 rays | scan modes from the files; elevation span and azimuth spread from the file angles |
| `rhi_coverage_extents_track_the_sweep` | DOW8, DOW6 | top height and ground range from the file elevations and gate geometry |
| `azimuth_circular_mean_handles_north_wrap` | `l2-ktlx-20130520-201643-trim` and `l2-kewx-20160413-022531-trim`, both sweeps (azimuths cross north) | circular mean of the MetPy azimuths (differs from the arithmetic mean by 4.5 to 159 deg) |
| `real_dow8_rhi_drives_the_rhi_panel_pipeline` (was already real) | DOW8 | now through `require_file!`; DBZHC[37, 316] from netCDF4 |

### `crates/recast-radar-map/tests/volumetric_real.rs`

| test | real input | assertion source |
|---|---|---|
| `composite_takes_column_max` | `l2-kewx-20160413-022531` (19 tilts) | numpy column walk on MetPy reflectivity: per-row counts and sums, 76.5 dBZ maximum location; gates above the base tilt |
| `echo_top_rises_with_higher_tilt` | same | reference echo tops (18.3 dBZ); gates whose top is above the base beam |
| `cross_section_reconstructs_a_reflectivity_column` | `l2-ktlx-20130520-201643` (Moore supercell, and a path across the radar) | MRMS-style reference section, native and path-smoothed, pixel by pixel |
| `velocity_cross_section_reconstructs_velocity` | same (mesocyclone path; a path Py-ART `dealias_region_based` leaves unchanged) | raw velocity reference with the 30 m/s guard; dealiased section equals it where Py-ART unfolds nothing |
| `derived_products_handle_degraded_inputs_without_panicking` | `l2-tbwi-20230601-175101-stub` (no radials), `l2-ktlx-19990503-230052` (68 radials; also with every REF byte set to the no-data code), `jma-n6-20191012-090000-rs47773`, `l2-ktlx-20240315-000217-trim` | None for no data; reference products for the single tilt; empty grids after the mutation |
| `vil_positive_for_deep_reflectivity` | KEWX 2016 | numpy VIL (Greene and Clark 1972, 56 dBZ cap): per-row reference, 57.2 kg/m2 maximum |
| `mehs_flags_deep_intense_cores_only` | KEWX 2016; `l2-ktlx-20240515-000014` (clear air) | numpy SHI/MEHS (Witt et al. 1998, 3.2/6.4 km): 97 mm at the 76.5 dBZ core; no MEHS in clear air |
| `vil_density_is_in_physical_range` | KEWX 2016 | reference VIL / echo top where the top is above 1.5 km; 5.9 g/m3 maximum |

## retrieve

**Converted in C.2** (branch `real-tests-retrieve`): all 48 entries are gone from the allowlist (the
counts table above is the C.1 snapshot). The 35 synthetic tests and 13 helpers were deleted from
`src/`; their replacements are integration tests under `crates/recast-radar-retrieve/tests/` (one
`<module>_real.rs` per module, plus one unit test in `wind.rs` for the private row lookup) that
decode corpus files with the workspace readers and compare against JSON goldens under
`testdata/golden/retrieve/`, written by `tools/retrieve_golden.py`. The script reads the same files
with MetPy 1.7.1 (Level II moments, azimuths, Nyquist), Py-ART 2.2.5 (`read_nexrad_archive`,
`dealias_region_based`, `compute_cdr`, `kdp_vulpiani`, `vad_browning`) and its own DORADE block
walker; takes storm truth from the NHC HURDAT2 best track and the SPC tornado database (rows copied
into the goldens); and computes expected outputs with numpy reference implementations of the
documented algorithms (float32 arithmetic where the Rust code uses f32). The pure-math unit tests
that stayed in `src/` (`detect.rs` Stumpf 1998 worked examples, `sweep.rs` 1-D unwrap/Hampel kernels
and coefficient tables, `wind.rs` convergence-window rows, `availability.rs` threshold and id lookup)
were never findings; the env-gated `gbvtd.rs::gbvtd_on_real_hurricane_volume` was replaced by
`gbvtd_real.rs` and `pgua_frame_moment_audit` (an env-gated cache audit) is unchanged.

Edge cases mutate real decoded data in place and say so: a reflectivity grid truncated to 10 rows,
a RHOHV grid thinned to 500 m gates, velocity blanked or cut to zero gates, reflectivity removed from
a tornadic volume, +30 m/s spikes on every seventh radial, a 10 m/s sin(2 az) harmonic, reflectivity
rows stored in reverse order, a duplicated radial identity.

### `crates/recast-radar-retrieve/tests/availability_real.rs`

| test | real input | assertion source |
|---|---|---|
| `derive_on_demand_admits_a_dual_pol_sweep_the_presence_gate_rejects` | `l2-ktlx-20240315-000217-trim` sweep 1 (REF/ZDR/PHI/RHO/CFP) | MetPy data-block names and row counts; every product except the velocity/SW ones derivable |
| `derive_on_demand_never_admits_kdp` | same sweep (PHI present, no KDP block in MetPy) | KDP not admitted through the derive-on-demand arm |
| `native_moments_route_straight_through_the_presence_gate` | both sweeps of the split cut | presence equals MetPy's block list for all seven native moments |
| `a_partial_sweep_carries_no_sources` | `l2-ktlx-19990503-230052` (68 radials, MetPy) | displayable at the relaxed threshold; not after the REF grid is truncated to 10 rows |
| `unknown_names_that_match_nothing_are_not_derivable` | the split cut (CFP is a real unknown moment) | bogus ids rejected; CFP present but not derived |

### `crates/recast-radar-retrieve/tests/detect_real.rs`

| test | real input | assertion source |
|---|---|---|
| `violent_tornadoes_are_detected_where_the_damage_survey_puts_them` | `l2-kdgx-20230325-010651` (Rolling Fork EF4, 108 km) and `l2-koax-20140616-205305` (Stanton EF4, 103 km) | SPC path (om 622315, 514013) interpolated to the Py-ART ray time: strongest site within 6 km, TVS/meso class, >= 3 tilts, no second site nearby |
| `explicit_rotation_api_never_falls_back_to_an_internal_engine` | Rolling Fork volume | no grids -> nothing; the crate's dealiased grids -> the internal result; raw folded grids -> a different result |
| `single_doppler_tilt_yields_features_but_no_site` | `l2-ktlx-20130520-201643-trim` (one Doppler tilt per MetPy) | 2D features on the tilt, no vertically continuous site |
| `circulations_without_echo_are_rejected` | Rolling Fork volume with every REF grid removed | sites before, none after; zero features per tilt |
| `quiet_volumes_detect_nothing` | `l2-ktlx-20240515-000014` (VCP 35), `l2-kmaf-20230331-230843` (VCP 31) | Py-ART max reflectivity 41.5 / 45.5 dBZ (clutter, biota); no sites |

The Moore 2013-05-20 20:16Z volume (`l2-ktlx-20130520-201643`) is in `detect.json` with its SPC
position (az 264.5 deg, 20.7 km) but is not asserted: `detect_rotation_sites` reports no site
within 10 km of it (the debris region's velocity on the lowest tilts is folded and noisy; the
region dealiaser over-unfolds gates to -73 m/s and the LLSD shear exceeds the 150 m/s/km
plausibility cap), so that volume is a documented gap for the detector, not a test.

### `crates/recast-radar-retrieve/tests/gbvtd_real.rs`

| test | real input | assertion source |
|---|---|---|
| `axisymmetric_retrieval_at_the_best_track_centre_matches_intensity_and_reference_rings` | `l2-klix-20210829-180425` (Ida, sweep 2, Nyquist 32) and `l2-tjua-20220918-190621` (Fiona, sweep 2) | HURDAT2 centre interpolated to the sweep time: VT max within 15 m/s of the best-track wind (64.3 / 38.6 m/s), RMW within a factor 2 of the best-track RMW (18.5 / 46.3 km); numpy ring fits on Py-ART dealiased velocity match VT/VR/rms to 0.5 m/s on >= 2/3 of the rings (19/19 Fiona, 14/19 Ida) |
| `simplex_centre_search_recovers_the_best_track_centre` | both | centre within 20 km of HURDAT2 (15.2 km Ida, 6.3 km Fiona); VT max within 0.6-1.4x the best-track wind |
| `wavenumber_one_asymmetry_matches_the_reference_decomposition` | both | numpy wavenumber-1 terms (cos, sin, amplitude to 0.5 m/s, phase to 10 deg) on agreeing rings; Fiona's 24-40 km eyewall asymmetry 4-9 m/s with a steady phase |

### `crates/recast-radar-retrieve/tests/shear_real.rs`

| test | real input | assertion source |
|---|---|---|
| `moore_couplet_azimuthal_shear_matches_the_llsd_reference` | `l2-ktlx-20130520-201643-trim` sweep 2 | numpy LLSD on MetPy raw velocity, every row and 450 sampled gates to 0.05 x 1e-3/s; Py-ART-dealiased reference on >= 97% of gates (99.3%) |
| `derecho_radial_divergence_matches_the_llsd_reference` | `l2-kdvn-20200810-180401-trim` sweep 2 | numpy LLSD radial derivative on MetPy raw velocity, gate for gate |
| `explicit_derivative_entry_points_never_run_a_second_dealias_pass` | same (Py-ART unfolds 38,656 gates) | explicit entry points equal the raw reference; the internal ones agree better with the Py-ART-dealiased reference (94-95%) than with the raw one (88%) |
| `degraded_velocity_yields_no_data_without_panicking` | `jma-n6-20191012-090000-rs47773` (no Nyquist); the Moore sweep with velocity blanked / cut to zero gates | finite derivatives without Nyquist; all-NaN and empty outputs |

### `crates/recast-radar-retrieve/tests/sweep_real.rs`

| test | real input | assertion source |
|---|---|---|
| `moore_core_kdp_and_filtered_phase_match_the_reference` | `l2-ktlx-20130520-201643-trim` sweep 1 (480 x 1192 PHI) | numpy phase bundle (QC, unwrap, gap fill, Hampel, Huber fit) to 0.01 deg/km on every row and 450 cells; hail-core mean KDP > 1 deg/km and within 3x Py-ART Vulpiani |
| `short_phidp_gaps_feed_the_fit_but_get_no_estimate` | same, 60 one-gate gaps after QC | no KDP/PHIF at the gap; neighbours equal the filled reference, not the unfilled one |
| `native_kdp_is_preserved` | `dorade-noxp-20090525-203211-sector` (native KDP, all bad-data per the walker); the Moore sweep's own derived KDP | skipped_existing, grid unchanged; second pass, `derive_product` and overwrite give the same grid |
| `filtered_phase_survives_where_kdp_is_out_of_bounds` | Moore sweep, 100 of 18,163 gates with slope/2 outside [-2, 14] | KDP NaN, PHIF finite |
| `velocity_range_gradient_uses_the_nyquist_wrapped_difference` | `l2-kdvn-20200810-180401-trim` sweep 2 (Nyquist 21.03, MetPy) | numpy wrapped gradient, gate for gate; 100 fold cells differ from the plain difference |
| `rho_gating_samples_by_physical_range` | Moore sweep with RHO thinned to 500 m gates | numpy reference with the thinned RHO; 100 gates change against the full-RHO result |
| `cdr_matches_pyart_compute_cdr` | Moore sweep ZDR/RHO | Py-ART `compute_cdr`, every row and 450 cells to 0.02 dB |
| `unknown_band_blocks_band_sensitive_products_but_keeps_phif` | Moore sweep with `RadarBand::Unknown` | KDP and RATE_KDP unavailable; PHIF equals the unbounded numpy intercept |

Py-ART's Vulpiani and Maesaka KDP are heavily smoothed and run 2-10x lower than a 3 km windowed
regression on the Moore core's noisy PHIDP (gate correlation near zero), so they serve only as a
magnitude-class check, not a gate reference. The dkrom ODIM volume was tried as a second PHIDP input
and dropped: the file stores PHIDP in radians and RHOHV with a 0.0028 gain (maximum 0.707), so the
0.80 correlation floor gates every PHIDP sample.

### `crates/recast-radar-retrieve/tests/volume_real.rs`

| test | real input | assertion source |
|---|---|---|
| `column_maximum_and_echo_depth_match_the_column_walk_reference` | `l2-kewx-20160413-022531` (19 tilts) | numpy column walk on MetPy reflectivity at 469 sampled cells (300 random + the hail core block): CMAX to 0.01 dB, echo base/top/depth (18.3 dBZ) to 0.5 m, 267 cells whose maximum comes from an upper tilt; the 70.5 dBZ core column |

### `crates/recast-radar-retrieve/tests/vwp_real.rs`

| test | real input | assertion source |
|---|---|---|
| `blizzard_profile_recovers_the_reference_wind` | `l2-kbox-20220129-150537`, 8 levels 0.5-4 km | numpy VAD on Py-ART dealiased velocity: same tilt at >= 6 levels (8), u/v/speed to 1 m/s, direction to 5 deg; Py-ART `vad_browning` median 1.7 m/s, max 5.1 m/s; > 25 m/s jet at 1 km |
| `stratiform_levels_sit_at_four_thirds_earth_beam_height` | `l2-pahg-20250909-212549`, 12 levels 0.5-6 km | same tilt at all 12 levels, winds to 1 m/s; level height equals the 4/3-Earth beam height of the annulus centre gate |
| `robust_refit_removes_convective_outliers` | `l2-kilx-20260418-013553`, 23 levels (>= 4 with > 10% trimmed); KBOX with +30 m/s on every seventh radial | reference winds to 1.5 m/s; the spiked profile keeps u/v within 0.5 m/s of the clean one |
| `sector_scan_is_explicitly_rejected_for_azimuth_coverage` | `dorade-noxp-20090525-203211-sector` (100 rays over 100 deg, walker) | InsufficientAzimuthCoverage with < 8 sectors and a gap > 120 deg |
| `unresolved_second_harmonic_is_rejected_by_residual_qc` | KBOX with a 10 m/s sin(2 az) harmonic added | ResidualTooLarge, rms > 5.2 m/s (clean rms < 2) |
| `missing_height_coverage_is_a_level_rejection_not_a_profile_error` | `l2-ktlx-20240315-000217-trim` (one Doppler tilt, 480 radials) | 1 km: rejected with the reference candidate (13 samples); 20 km: NoBeamCoverage, no candidate |
| `input_contract_and_scan_mode_fail_loudly` | `cfrad1-dow8-20211011-223602-rhi-trim3-classic` (RHI); the KTLX trim with 0 or 1 grids, 20,001 levels, no grids | UnsupportedScanMode, GridCountMismatch, InvalidConfig, NoVelocityGrids |

### `crates/recast-radar-retrieve/src/wind.rs`

| test | real input | assertion source |
|---|---|---|
| `tests::reflectivity_rows_follow_raw_radial_identity_not_row_position` | `l2-ktlx-20130520-201643-trim` sweep 2 REF rows stored in reverse order; a duplicated radial identity | identity lookup inverts the permutation; the gust proxy is identical cell for cell; first row wins |

## track

**Converted in C.2** (branch `real-tests-track`): all 30 entries are gone from the allowlist (the
counts table above is the C.1 snapshot). The 22 synthetic tests and 8 helpers were deleted from
`src/`; the pure-math unit tests that stayed (`tracking.rs` Hungarian solver, `tracks.rs` running
maximum, grid round trip, TDS thresholds and colour ramp) were never findings. The replacements are
integration tests under `crates/recast-radar-track/tests/` that decode corpus files with the
workspace readers and compare against JSON goldens under `testdata/golden/track/`, written by
`tools/track_golden.py`. The script reads the same files with Py-ART 2.2.5 (composite reflectivity,
region-based dealiasing, fields), MetPy 1.7.1 (Level II sweep summaries and volume times; Level III
Storm Tracking Information products) and its own DORADE block walker, and computes the expected
outputs with numpy/scipy (float32 arithmetic where the Rust code uses f32).

Corpus additions for this group: the KDVN volumes before and after `l2-kdvn-20200810-180401`
(`l2-kdvn-20200810-175718`, `-181043`, `-181724`; downloads, tag `sequence:kdvn-20200810`) and the
four Level III STI products for those volumes (`l3-kdvn-20200810-{1757,1804,1810,1817}-nst`,
committed, 57 KB). The STI product is the operational SCIT tracker's output (cell ids, positions,
past/forecast positions, DBZM, forecast movement), the independent reference for the tracking tests.

### `crates/recast-radar-track/tests/cells_real.rs`

| test | real input | assertion source |
|---|---|---|
| `identifies_every_salient_hail_core_of_kewx` | `l2-kewx-20160413-022531` (19 tilts) | every 60 dBZ component of Py-ART's composite with area >= 20 km2 (four: 76.8, 72.5, 67.3, 66.9 dBZ) has its own cell within 3 km of the Z^(4/7)-weighted centroid; the strongest is the first cell; peaks stay in [60, composite max] |
| `clear_air_volume_yields_no_cells` | `l2-ktlx-20240515-000014` | Py-ART composite maximum 40.8 dBZ, largest 30 dBZ patch 0.24 km2 (below the 20 km2 saliency floor): no cells |
| `volume_without_radials_yields_no_cells` | `l2-tbwi-20230601-175101-stub` | MetPy reads 0 sweeps; the decoded volume has no cuts and no cells |
| `watershed_splits_the_derecho_envelope_into_its_cores` | `l2-kdvn-20200810-180401` | one contiguous 40 dBZ envelope of 15,597 km2 on a 1 km Cartesian image of Py-ART's composite holding eight 60 dBZ local maxima 15 km or more apart; the four strongest each get a distinct cell within 5 km, and the envelope holds at least eight cells |

### `crates/recast-radar-track/tests/swath_real.rs`

| test | real input | assertion source |
|---|---|---|
| `max_reflectivity_takes_per_gate_maximum` | NOXP sector sweeps 2009-05-25 20:35:29 and 20:36:59Z from `dorade-noxp-20090525-sweeps-tgz` (171 rays, 1002 x 150 m gates) | numpy per-gate maximum after the documented 0.1-degree nearest-azimuth mapping (f32): 48,243 finite gates, sum, max, min, 48 samples, 24 gates where the earlier sweep wins; reference geometry equals the later sweep's azimuths |
| `swath_covers_union_of_two_positions` | same | union count (48,243), 24 gates lit only in the earlier sweep and 24 only in the later one; each sweep alone covers less |
| `max_magnitude_keeps_sign_of_extreme` | same, velocity | numpy signed extreme per gate: 39,770 finite gates, sum, 48 samples, 24 gates where the winner's sign differs from the loser's, 17,180 inbound gates |
| `empty_when_no_frame_has_the_moment` | `l2-tstl-20230331-230314-trim` (MetPy: REF; REF/VEL/SW) | ZDR and RHOHV swaths and the ZDR base tilt are `None`; the REF swath exists; no frames give `None` |
| `picks_lowest_tilt_carrying_the_moment` | `l2-ktlx-20240315-000217-trim` (0.58 deg REF/ZDR/PHI/RHO/CFP, 0.48 deg REF/VEL/SW), `l2-tstl-20230331-230314-trim` (two 0.26 deg cuts) | lowest first-ray elevation from MetPy among the sweeps carrying each moment (first on ties): REF and VEL resolve to the KTLX Doppler cut, ZDR/RHO/PHI to the surveillance cut; TSTL REF to cut 0, VEL to cut 1 |

### `crates/recast-radar-track/tests/temporal_real.rs`

| test | real input | assertion source |
|---|---|---|
| `difference_and_trend` | NOXP sweeps 20:35:29 and 20:36:59Z (SSWB start times 90 s apart) | numpy f32 difference and per-hour trend over the 37,628 gates both sweeps hold; zero, negative and NaN elapsed give `None`; the 170-ray 20:33:47Z sweep is a geometry mismatch |
| `rate_accumulation_uses_trapezoids` | the first four equal-geometry sweeps (20:35:29-20:40:10Z) | numpy trapezoid accumulation between the SSWB start times (41,478 gates); one frame, reversed or stalled times give `None` |
| `probability_ignores_missing_values` | all eleven equal-geometry sweeps (20:35:29-20:51:27Z) | numpy percentage of sweeps at or above 40 dBZ over the sweeps with data (68,169 gates; 120 at 100 %, 65,973 at 0 %); 24 partially-covered gates carry their valid and exceeding counts |
| `maximum_minimum_mean_and_duration_match_the_reference` (new) | same eleven sweeps; first four for the duration | numpy maximum, minimum, sequential-f32 mean and minutes above 40 dBZ (half credit for one-sided windows) |

### `crates/recast-radar-track/tests/tracking_real.rs`

| test | real input | assertion source |
|---|---|---|
| `co_identified_storms_keep_one_track_id_and_scit_motion` (was `qlcs_line_no_steal`, `crossing_cells_do_not_swap_ids`) | the four consecutive KDVN volumes and their STI products | SCIT storms present in all four volumes with a tracker cell within 3 km at each (M9 and D8 among 13 persistent ids) keep one tracker id, distinct storms distinct ids, one fix per volume, and the fitted motion is within 6 m/s of SCIT's forecast movement (256 deg / 32 kt and 257 deg / 21 kt) |
| `merge_terminates_the_loser_with_a_link` | same | every `merged_into` link joins two fragments of one SCIT storm (both fixes within 8 km of the same storm id at the previous volume) and SCIT has exactly one storm within 8 km of the survivor; at least one merge occurs; tombstones vanish at the next volume |
| `split_links_children_to_parent` | same | every child track's first fix and its parent's fix are within 8 km of the same SCIT storm; the child inherits the parent's motion; at least one split occurs |
| `distant_new_storms_start_fresh_tracks` (was `speed_gate_rejects_a_teleporting_cell`) | same | SCIT storms new in a volume, 20 km or more from every previous SCIT storm and from every tracker fix (C2 at 18:10, S2 at 18:17: over 50 m/s to reach) get a one-fix track without a parent |
| `coast_and_reacquire_keeps_the_id` | same | tracks that miss a volume keep their id, add no fix for the gap, count the miss and reset it on reacquisition; at least one such track is a SCIT storm present throughout (S1) |
| `time_gate_resets_everything` | `l2-ktlx-20130520-201643` then `l2-ktlx-20240315-000217` (MetPy volume times eleven years apart) | every first-volume track is dropped and every second-volume cell starts a one-fix track; a repeated or out-of-order volume time is ignored |

### `crates/recast-radar-track/tests/tracks_real.rs`

| test | real input | assertion source |
|---|---|---|
| `cartesian_frame_paints_couplet_location` | `l2-ktlx-20130520-201643` (Moore EF5, 22 km W) | Py-ART region-based dealiased 0.5 deg velocity: strongest cyclonic azimuthal shear at az 266.8 deg / 22.6 km (gate-to-gate 119 m/s); the frame maximum inside 60 km lies within 2 km of it and reads above the display floor; cells inside 5 km, a no-data cell and a clear-air cell (velocity, no echo) stay off the display; a calm in-echo cell reads finite below the floor |
| `height_cap_bounds_range_coverage` | same | 4/3-Earth beam height in Python: the 0.52 deg beam leaves 2 km at 122.75 km; a 69 dBZ echo at 154 km stays empty, a 56.5 dBZ echo at 110 km accumulates, no finite cell beyond the bound or inside 5 km |
| `tds_gates_require_anchor_proximity_and_criteria` | same, lowest dual-pol sweep | the 218 gates within 5 km of the Py-ART circulation with RHOHV < 0.82 and Z > 30 dBZ in Py-ART's fields, matched one to one by position (50 m), RHOHV and Z; no anchor or a rank-1 anchor flags nothing; the detector's own significant circulations flag only gates within 5 km of themselves |

## render-bench

**Converted in C.2** (branch `real-tests-render-bench`): all 32 entries are gone from the allowlist
(the counts table above is the C.1 snapshot). The 27 synthetic tests and 5 helpers were replaced in
place: the tests stay unit tests in `src/` because they reach private lookup, palette and metric
functions, but they now decode corpus files with `recast-radar-io-nexrad` (render) or the bench's
own byte router (bench) and compare against JSON goldens under `testdata/golden/render/` and
`testdata/golden/bench/`, written by `tools/render_bench_golden.py`. The script reads the same files
with Py-ART 2.2.5 (`NEXRADLevel2File` raw gate codes, azimuths, Nyquist velocities and data-block
scale/offset; `read_nexrad_archive`; `dealias_region_based`; `storm_relative_velocity`) and MetPy
1.7.1 (scaled values, cross-checked against the raw codes), and computes expected outputs with numpy
reference implementations of the documented rules (float32 where the Rust code uses f32). Tests that
never fed synthetic data (viewport option maths, colour-table checks, `CachedSample` packing) are
unchanged.

Two findings from the real data: the COW2 DORADE head trim has no duplicate azimuths (its 3
transition rays are 0.5 deg apart), so the duplicate-row test became a neighbouring-radial test; and
the reflectivity sample cache does not fall through hidden (transparent) palette entries the way the
direct render does, which a four-radial synthetic volume could not show (see
`viewport_sample_cache_matches_direct_moment_render`). The KLIX 2005 seam case proposed in C.1 needs
the Message 1 Nyquist fix from `real-tests-io-nexrad` (the base branch reads it from the wrong
halfword), so the seam test uses the full KDVN 2020 volume until the groups merge.

### `crates/recast-radar-bench/src/dealias_eval.rs`

Fields come from `decode_field` on decoded cuts (the bench's own path); Py-ART's dealiased output is
rebuilt as `raw + 2N·k` from run-length-encoded fold numbers in the golden file.

| test | real input | assertion source |
|---|---|---|
| `tests::boundary_metric_counts_real_fold_boundaries_before_and_after_unfolding` | `l2-kdvn-20200810-180401-trim` sweep 2 (derecho, Nyquist 21.03 m/s, 84,964 finite gates) | numpy boundary pairs of the raw sweep (7,486) and of Py-ART's region-based output (409) |
| `tests::boundary_metric_counts_the_wrap_seam` | `l2-kdvn-20200810-180401` sweep 2 (full circle, 720 radials; download) | numpy counts with and without the last-to-first radial seam (15,224 / 15,193) and speck counts with and without wrap |
| `tests::percent_modified_counts_whole_fold_moves_only` | `l2-kdvn-20200810-180401-trim` sweep 2 | Py-ART unfolds 38,656 of 84,964 gates: 45.4969 %; raw vs raw is 0 |
| `tests::speck_count_finds_isolated_outliers_only` | `l2-pgua-20230524-030945-trim` sweep 2 (Mawar, Nyquist 35.55 m/s) and the KDVN cut | numpy isolated-speck counts: 1,203 (PGUA), 1,212 raw and 164 after Py-ART unfolding (KDVN) |

### `crates/recast-radar-render/src/lib.rs`

| test | real input | assertion source |
|---|---|---|
| `tests::velocity_range_folded_bins_render_table_rf_color` | `l2-ktlx-20240315-000217-trim` sweep 2 VEL | Py-ART raw codes: 342 range-folded gates at listed positions; RF palette colour; rendered pixels centred on interior RF gates carry it |
| `tests::reflectivity_range_folded_bins_render_table_rf_color` | same sweep, REF (the Doppler cut's reflectivity carries the same 342 RF codes) | as above with the reflectivity table |
| `tests::storm_relative_u8_row_palette_matches_pyart_storm_relative_velocity` | same sweep, VEL | `pyart.retrieve.storm_relative_velocity` (225 deg, 18 m/s) at 40 gates: value within 1e-3, palette colour within one channel step of the table colour; no-data and RF codes |
| `tests::custom_color_table_feeds_precomputed_u8_palette` | same sweep, VEL (scale 2, offset 129 from the data-block header) | every code present in the sweep maps to the ramp colour of its physical velocity; exact stops at codes 129, 149, 169 |
| `tests::storm_motion_basis_matches_direct_projection` | same sweep, VEL | `speed · cos(direction − azimuth)` on the file's azimuths |
| `tests::grid_sample_cache_upper_bound_tracks_actual_radar_footprint` | sweep 1 REF (1832 x 250 m from 2125 m: 460.125 km) | pixel count inside the footprint circle, plus at most the row-span padding |
| `tests::viewport_lookup_matches_reference_hypot_formula` | sweep 1 REF (radials 167.3 deg clockwise to 46.7 deg) | reference hypot lookup; 5 of 9 probe pixels resolve (west, south, north), the centre and east do not |
| `tests::viewport_lookup_table_matches_reference_hypot_formula` | same | reference hypot lookup on a 42-pixel probe grid (at least 12 resolve) |
| `tests::viewport_lookup_table_matches_rotated_viewport_lookup` | same | `viewport_lookup` at three rotations over the whole window; over a third of the window resolves |
| `tests::baked_rotation_changes_table_azimuth_bins` | same | 0.35 rad rotation against 0.5 deg radials moves most resolved pixels to another bin |
| `tests::viewport_row_span_covers_reference_samples` | same, 96 px at 10 km/px | reference samples inside the row spans; rows beyond 460 km have none |
| `tests::azimuth_lookup_fills_wider_native_radial_sectors` | `l2-ktlx-19990504-002218-trim` sweep 1 (Message 1, 367 radials at 0.97 deg) | every 0.1 deg bin served; 60 query azimuths resolve to the nearest radial (numpy angular distance) |
| `tests::azimuth_lookup_prefers_neighbour_row_with_longer_valid_extent` | `l2-ktlx-20240315-000217-trim` sweep 1 REF | per-row valid extents from Py-ART raw codes; at the bin between two neighbouring radials the longer row ranks first and a gate only it fills resolves to it (24 pairs) |
| `tests::compact_sample_resolution_keeps_visible_range_folded_candidates` | sweep 2 VEL | per-row valid extents (RF counts as valid); RF gates resolve to their own row |
| `tests::viewport_render_uses_requested_screen_resolution` | sweep 2 (REF and VEL) | buffer dimensions; image, buffer and cache paths byte-identical |
| `tests::viewport_sample_cache_matches_direct_moment_render` | sweep 2 REF | exact equality under an all-opaque ramp; under the default palette differences are only cache-transparent / direct-opaque fall-throughs |
| `tests::viewport_geometry_cache_resolves_across_compatible_products` | sweep 2 REF and VEL (same gate geometry) | geometry-derived and direct sample caches render identically |
| `tests::viewport_sample_cache_matches_direct_storm_relative_render` | sweep 2 VEL | cached equals direct; a different storm motion recolours the same opaque pixels |
| `tests::viewport_sample_cache_rejects_mismatched_cache` | sweep 2 | moment mismatch error |
| `tests::viewport_render_rejects_wrong_sized_reusable_buffer` | sweep 2 | buffer size error with the requested dimensions |
| `tests::viewport_cache_rejects_different_volume` | KTLX 2024 and `l2-ktlx-20130520-201643-trim` | different-volume error; the source volume still renders |
| `tests::viewport_cache_renders_u16_palette_moments` | `l2-ktlx-20130520-201643-trim` sweep 1 PHI (16-bit, codes to 1022, scale 2.8361, offset 2) | 24 MetPy values and Py-ART codes through the u16 palette; cached render equals direct |
| `derived_product_tests::derived_products_render_through_viewport_cache` | `l2-kewx-20160413-022531` (19 sweeps; download) | Py-ART volume maximum 76.5 dBZ at 251.5 deg, 55.9 km (4.0 deg tilt; lowest tilt 70.5 dBZ): composite peak value and location, echo top above that beam, VIL positive, rendered pixel colour |

## core-data-scattering

**Converted in C.2** (branch `real-tests-core-data-scattering`): all 87 entries are gone from the
allowlist and the group keeps no exception (the counts table above is the C.1 snapshot). The 68
synthetic tests and 19 helpers were deleted or rewritten:

- `recast-radar-core`: the model and merge tests moved out of `src/lib.rs` into the integration tests
  `tests/real_model.rs` and `tests/real_merge.rs` (the crate now has dev-dependencies on the io crates
  and the testdata crate; unit tests in `src/` cannot share model types with the io crates through a
  dev-dependency cycle). They decode corpus files with the workspace readers and compare against
  `testdata/golden/core/model.json`, written by `tools/core_golden.py` from Py-ART 2.2.5 (raw moment
  codes, data-block headers), MetPy 1.7.1 (sweep layouts, azimuths, elevations), h5py 3.16.0 (ODIM
  parts), netCDF4 1.7.4 (per-ray instrument variables) and a GRIB2 section walker (JMA). The expected
  outcomes of `merge_radar_volumes` come from a reference implementation of its documented rules in the
  script, fed only with that independent metadata (float32 comparisons as in Rust).
- `recast-radar-data`: the provider-contract tests run a provider over two complete ORD archive-bucket
  hour listings of RMI Jabbeke (`src/international/fixtures/ord_archive_bejab_2026061{2,3}T14_hour.xml`,
  captured 2026-09-17); the tropical merge tests use captured NHC and GDACS feeds under
  `tests/fixtures/tropical/` (see below).
- `recast-radar-scattering`: the LUT, P3 table, PSD and runtime tests load the committed PyTMatrix
  0.3.3 tables and the WRF P3 v5.4 tables (`testdata/scattering/manifest.toml`; `src/test_corpus.rs`
  loads them) and compare against `testdata/golden/scattering/{tmatrix_luts,p3_tables}.json`, written
  by `tools/scattering_golden.py` from the LUT bytes (schema-1 layout), the post-freeze held-out
  PyTMatrix report and the P3 table text.

Corpus additions made for this group: five ODIM per-quantity parts of two scans from the permanent
ORD archive bucket (`odim-bejab-20260612-1450-{dbzh,vrad}`, `odim-nohur-20260612-1445-{dbzh,th}`,
`odim-nohur-20260612-1446-vradh`), the two small conventional PyTMatrix tables with their exact
generator configs and manifests, the held-out interpolation report and node request, the two official
WRF P3 tables as downloads and their first blocks as committed prefixes (all in
`testdata/scattering/manifest.toml`). A decoder gap surfaced while converting: the Level II reader
takes the Message 1 Nyquist velocity from halfword 24 (unused) instead of halfword 31, so legacy
radials carry `nyquist_velocity_mps = None` (MetPy: 26.1 m/s on the KTLX 1999 batch cuts); the merge
test that fills Nyquist velocities therefore uses a Message 31 cut. That is `recast-radar-io-nexrad`'s
to fix. (Fixed in stream F: Message 1 radials carry the halfword 31 value, checked in
`tests/real_model.rs` `message1_reflectivity_maps_onto_the_doppler_range`.)

### FM301 model (stream F)

The FM301 model replaced the legacy types, so the detector now also lists `Volume`, `Sweep`, `Rays`,
`Field` and `RayVariables` (every crate), the bench's `Plane` (was `Field`), and the methods
`push_ray`, `add_field` and `push_row_u8` ... `push_row_f64` (`MODEL_TYPES` and `MODEL_METHODS` in
`crates/recast-radar-testdata/src/synthetic/rules.rs`; the legacy names stay listed). That surfaced 28
findings, all in code stream F wrote:

- converted: the Py-ART layout helper of `crates/recast-radar-core/tests/fm301_conformance.rs` (no
  filled buffer), `physical_copy` in `crates/recast-radar-render/tests/real_render_parity.rs` (an
  edit of the real field), the flattened and thinned sweeps of
  `crates/recast-radar-map/tests/rhi_real.rs` (edits of the real ray arrays, as on `main`), the ODIM
  quantity-preference test (Met Eireann Shannon, fields reordered), and the core model unit tests
  with a real equivalent: `tests/real_model.rs` (ICD code resolution and decode table, X-SAPR float
  fill value, CF packing of the Irene and DOW8 integer fields, buffer moves, the KTLX 1999 Message 1
  range mapping, the time reference, `Sweep::seal` on edits of a real sweep) and `tests/real_merge.rs`
  (the KTLX 1999 reflectivity and Doppler products with their own ranges merged in both orders, a
  Doppler product starting 1 km further out, misaligned products). The merge goldens now follow the
  FM301 rules: collisions by field name (DBZH and TH are both kept), fixed angles (Level II: the
  Message 5 cut angle, MetPy `vcp_info`) and the first radial's time floored to the second.
- converted from mutated real records: the three row-layout tests of `Field` and their helper. A
  Py-ART scan of the 48 readable cached Level II files found no sweep with a moment missing on some
  radials or a later radial longer than the first, so `crates/recast-radar-core/tests/real_rows.rs`
  edits real Message 31 radials of `l2-ktlx-20130520-201643-trim` (uncompressed): VEL block
  pointers zeroed on three Doppler radials (absent rows, first, middle and last), the first
  surveillance radial's REF gate count cut to 100 (a later row longer than the first), and one PHI
  block relabelled 8-bit (the storage-type error), each checked against the unmodified decode.
- exception (1 entry): `real_rows.rs` `u16_rows_decode_real_big_endian_bytes_and_reject_odd_lengths`
  pushes a real PHI block's big-endian bytes, whole and cut by one byte, through
  `Field::push_row_u16_be`. The odd-length and row-order checks are unreachable through any decoder.
- pending (2 entries): the horizontal/unspecified/vertical preference of `Sweep::find` and its
  helper. No corpus volume carries DBZH, DBZ and DBZV together, and renaming variables in a real
  file keeps their `standard_name`, which classification reads first. Needs user review.

metadata-complete added two exceptions, built on real bytes because no real file has the input: in
io-nexrad, `limits_real.rs` `rda_log_frame` and `rda_log_data_is_limited_per_volume` (message 33 compression
bombs inserted into the real KIWA chunks, a limit test; no message 33 in 560 real files).

It also added ten pending entries: feature tests on edited or rearranged real bytes, for features no real
file exercises. They are not corruption or limit tests, so the exception rule does not cover them; the owner
decides whether to allow them as exceptions or remove them (and leave the features untested). Their helpers
are named `fabricate*` so the scanner flags them and their tests.

- io-nexrad, `radial_extras_real.rs`: `fabricated_differing_radials` edits values inside the committed KTLX
  2024 trim (radial 5's ELV atmospheric attenuation minus 3, radial 7's VOL calibration constant plus 0.5 dB,
  radial 9's radar identifier KTLX to XTLX, radial 11's REF TOVER plus 7, radial 13's VOL latitude plus 0.01
  degree), and `radials_that_differ_keep_their_own_values` checks the per-ray fallback on it. No real file
  has a sweep whose radials differ in these values: a scan of 96 real Level II volumes (the corpus, the
  testdata cache and the committed fixtures, 2026-09-25) found none.
- io-nexrad, `metadata_carried_real.rs`: `fabricate_frames_after_metadata` inserts the non-empty frames of a
  real metadata record (KTLX 2024's, or KIWA 2026's own) after KIWA 2026's, for
  `a_later_different_message_is_carried_as_a_copy` and `a_repeated_message_is_counted_not_copied`, because
  no real Level II file has two messages of one type; `fabricate_relabelled_frame` relabels a real message 2
  frame as messages 6, 9, 11 and 12 for `relabelled_real_frames_carry_messages_6_9_11_and_12`, because no
  real file has any of them (they go from the RPG to the RDA or test the wideband link). The values are
  compared with the inserted bytes and with what the source volume carries alone.
- io-formats, `grib2_sections_real.rs`: `tar_of` repacks the two committed RS47773 members into one tar and
  `with_local_use` gives each a GRIB2 section 2, for `merged_members_keep_each_members_values`. No real tar
  holds two members of one station, no real JMA message has a section 2 and no real station's members
  differ in sections 0 and 1 (the committed RS47773 tars and the 2026-09-24 21Z national N5 and N6 tars, 40
  messages of 20 stations, were checked).

### `crates/recast-radar-core/tests/real_model.rs`

| test | real input | assertion source |
|---|---|---|
| `ray_instrument_metadata_is_optional_but_must_align` | `cfrad1-irene-sr2-20110827-120420-sur-sweeps01` (per-ray `prt`, `unambiguous_range`, `n_samples`); `l2-ktlx-20240315-000217-trim` (none) | netCDF4 values per sweep; sidecar edits (one entry popped, one duplicated) give the alignment error with the file's counts |
| `decoded_u8_reflectivity_grid_scales_real_codes` (was `moment_grid_scales_compact_u8_rows`) | `l2-ktlx-20240315-000217-trim` sweep 1 REF (480 x 1832, scale 2, offset 66) | Py-ART raw codes: per-row code sums, probes, no-data cells = code 0 + code 1 cells; storage exactly rows x gates (folds the old `reserve_rows` check) |
| `decoded_grid_pads_rows_with_fewer_gates` (was `moment_grid_expands_and_pads_variable_gate_rows`) | `l2-kdmx-20080525-205148` sweep 11 VEL (836 gates on the first radial, down to 816 later) | Py-ART per-radial `ngates`: short rows padded with code 0, probes at the last echo gate and the first padded gate. No corpus sweep has its longest radial after a shorter one, so the expansion path has no real sample |
| `decoded_u8_velocity_grid_reports_range_folded_gates` (was `moment_grid_pushes_u8_slice_without_row_allocation`) | `l2-ktlx-20240315-000217-trim` sweep 2 VEL (342 range-folded gates) | Py-ART raw codes, sums, probes |
| `decoded_u16_differential_phase_grid_matches_big_endian_codes` (was `moment_grid_pushes_u16_be_bytes_without_row_allocation`) | `l2-ktlx-20130520-201643-trim` sweep 1 PHI (16-bit) | Py-ART raw u16 codes and header (scale 2.8361, offset 2) |
| `cut_tracks_available_moments` | KTLX 2024 and 2013 trims, both sweeps | MetPy data-block names (REF/ZDR/PHI/RHO/CFP; REF/VEL/SW), radial counts, first azimuths |
| `volume_can_keep_repeated_elevation_cuts_separate` | `jma-n5-20191012-090000-rs47773` (26 sweeps, repeated 0.0/0.3/0.7/1.2/1.8/2.5/5.0 deg tilts) | GRIB2 walker elevations: 26 cuts sorted lowest first, scan order kept among equals, numbered 1..=26 |

`moment_grid_reserves_rows_and_gate_storage` was deleted: capacity is not observable in real data; the
storage-length invariant it served is asserted on every decoded grid.

### `crates/recast-radar-core/tests/real_merge.rs`

| test | real input | assertion source |
|---|---|---|
| `merge_single_part_is_identity_with_sorted_cuts` | `l2-ktlx-20240315-000217-trim` (0.58 deg surveillance sweep before the 0.48 deg Doppler sweep) | reference merge: sorted, renumbered, otherwise equal |
| `merge_rejects_mismatched_site_ids` | KIWA start chunk + chunk 002 vs the KTLX 2024 trim | error names KIWA and KTLX |
| `merge_keeps_earliest_volume_time` | Hurum DBZH/TH (14:45:13Z) + VRADH (14:46:48Z) in both orders | h5py `/what` times; the merge takes the earliest |
| `merge_unions_moments_of_elevation_matched_cuts` | Jabbeke DBZH + VRAD parts (9 sweeps each) | reference merge: 9 merged moments, identical grids |
| `merge_fills_aligned_ray_instrument_metadata_without_overwriting_source_values` | Irene split into DBZ and VEL parts; DBZ part's unambiguous range cleared, VEL part's ray-0 prt doubled (edits) | netCDF4 prt / range: filled from the VEL part, first part wins |
| `merge_ignores_malformed_incoming_ray_instrument_metadata` | Irene parts with one sidecar shortened by one entry | the aligned sidecar is kept / replaces the malformed one |
| `merge_collision_keeps_first_part_grid` | Hurum DBZH + TH (both map to reflectivity) | 10 collisions, DBZH grids kept; h5py raw probes of both planes as physical values |
| `merge_unions_unmatched_cuts_sorted_by_elevation` | KIWA chunks 026, 002, 014 as parts (1.33, 0.27, 1.01 deg) | reference merge: 3 cuts sorted, numbered |
| `merge_skips_matched_cut_with_different_radial_count` | KIWA sweep 1 as one chunk (120 radials) vs two chunks (240) | skipped_geometry 1 |
| `merge_skips_matched_cut_with_shifted_azimuths` | KTLX 2013 + 2024 trims (elevations within 0.05 deg, 480 radials, azimuth grids 44-58 deg apart) | MetPy azimuths: both cuts skipped |
| `merge_accepts_azimuths_equal_across_the_north_wrap` | Jabbeke parts with radial 0 rewritten to 359.99 / 0.01 deg (edit of the real 0.5 deg bin centre) | merged as the unedited parts |
| `merge_accepts_matched_cut_with_different_gate_layout` | `l2-ktlx-19990504-002218` sweep 5 (REF 356 x 1 km, VEL/SW 920 x 250 m on the same 367 radials) split by moment; KTLX 2024 Doppler cut for the Nyquist fill | Py-ART message header gate geometry; Nyquist velocities restored from the VEL part |
| `merge_three_product_parts_assembles_one_scan` (was `..._assembles_full_dual_pol_cut`) | Hurum DBZH + VRADH + TH | reference merge: 8 merged, 10 collisions, earliest time |
| `merge_jma_repeated_tilts_keep_repetition_velocity_and_renumber` | JMA N5 + N6 members (and N6 twice) | GRIB2 walker start azimuths: 11 velocity sweeps land on their repetition, the two 0.3 deg sweeps with no matching azimuth grid are skipped, numbering 1..=26; the repeated member collides 11 times |

### `crates/recast-radar-data/src/international.rs`

`ArchivedBejab`, `ArchivedBejabLoop` and `ArchivedBejabArchive` build frame plans from the two
committed ORD archive listings (24 split scans per hour: DBZH + TH on the 0.3 deg ladder, DBZH + VRAD on
the 0.5 deg ladder). The seven contract tests keep their names; `captured_bejab_listings_hold_twenty_four_split_scans_per_hour`
pins what the captures hold. Expected identities and URLs are the listed object keys.

### `crates/recast-radar-data/src/tropical.rs`

| test | real input | assertion source |
|---|---|---|
| `captured_feeds_parse_to_the_storms_they_list` (new) | `nhc_current_storms_20260618T0211Z.json` (Internet Archive capture of NHC `CurrentStorms.json`: TS Arthur), `gdacs_tc_search_20260610_20260630.json` (GDACS SEARCH list: MEKKHALA-26, HIGOS-26, ARTHUR-26, CRISTINA-26), `nhc_current_storms_20260917T0538Z.json` (no active storm) and `gdacs_events4app_20260917T0538Z.json` (DUJUAN-26, FIFTEEN-E-26 among 98 other events), the last two captured in the same minute | names and positions read from the JSON with serde_json |
| `merge_dedupes_per_storm_not_per_basin` | Arthur capture + June GDACS list | CRISTINA-26 (NHC basin, no NHC counterpart) survives; ARTHUR-26 (55 km from the NHC fix) is dropped |
| `merge_drops_gdacs_duplicate_by_name` | ARTHUR-26 moved 10 deg of longitude (edit) | dropped by name |
| `merge_drops_gdacs_duplicate_by_position` | ARTHUR-26 and CRISTINA-26 renamed `Unnamed` (edit) | the near one dropped, the far one kept |
| `combine_keeps_all_gdacs_storms_when_nhc_is_down` | September GDACS list with an NHC error | FIFTEEN-E-26 (East Pacific) survives; strongest first |
| `empty_nhc_feed_does_not_hide_gdacs_nhc_basin_storm` (was `..._atlantic_storm`) | the same-minute empty NHC and September GDACS captures | FIFTEEN-E-26 kept |

### `crates/recast-radar-scattering/src/lut.rs`

| test | real input | assertion source |
|---|---|---|
| `committed_pytmatrix_tables_round_trip_byte_exactly` (was `synthetic_fixture_round_trip_is_byte_deterministic`) | rain and dry-ice tables with their configs | generator manifests (sha256, counts), header fields and payload nodes read from the bytes |
| `held_out_nodes_match_the_validator_and_direct_pytmatrix` (was `synthetic_affine_fixture_interpolates_exactly_in_declared_axis_order`) | the 12 held-out nodes of both tables | the report's multilinear interpolation (1e-9 relative) and its per-node threshold verdict against direct PyTMatrix |
| `prepared_plan_has_fixed_serializable_axis_ordered_layout` | rain table, query between diameter and ratio nodes | layout computed from the axis coordinates in the golden script |
| `prepared_execution_is_bit_identical_to_legacy_corner_order` | held-out and golden queries on both tables | frozen legacy corner walk |
| `prepared_plan_handles_singletons_and_exact_boundaries` | first and last nodes of both tables | stored nodes |
| `plan_preparation_preserves_outside_axis_failures`, `interpolation_refuses_extrapolation_and_nonfinite_coordinates` | dry-ice / rain axis limits | `OutsideAxis` with the file's axis bounds |
| `payload_and_external_config_hash_mismatches_fail_closed`, `embedded_config_hash_is_recomputed_not_trusted`, `digest_cannot_hide_an_invalid_additive_grid_node`, `file_magic_and_schema_are_never_guessed`, `redundant_header_contract_rejects_mislabeled_schema_axes_and_outputs` | the committed rain bytes with one bit flipped, the dry-ice config swapped in, a node's covariance raised above sqrt(ZH ZV), magic/schema/header fields rewritten | rejected |
| `singleton_axis_is_exact_and_does_not_duplicate_corner_weight` | grid nodes of both tables (singleton frequency and elevation axes) | stored nodes, one corner |

### `crates/recast-radar-scattering/src/p3_table.rs`

| test | real input | assertion source |
|---|---|---|
| `parser_accepts_exact_two_and_three_moment_record_layouts` | the committed first blocks of both official tables (reduced layout) | records read from the text by the golden script (float32) |
| `official_tables_load_and_retain_every_golden_record` (new) | the full 2momI/3momI downloads (skipped offline) | byte length and sha256 gate, 1000/11000 retained records, the committed prefixes |
| `parser_rejects_truncation_extra_content_nonfinite_values_and_wrong_indices` | edits of the two-moment first block | parse errors |

### `crates/recast-radar-scattering/src/scheme_psd.rs` and `tmatrix_runtime.rs`

The per-particle callback of every PSD test is the committed dry-ice table looked up at the node's
diameter and axis ratio (sub-floor spheres at the floor sphere scaled by `(D/D_floor)^6`); the
fall-speed provenance is the SHA-256 of the table config's `terminal_velocity` object (Schiller-Naumann)
as an `ExternalVersionedResearch` token; distributions were resized to 0.5 mm semi-axes at 200 per kg so
their nodes sit inside the 0.1-50 mm table, with the convergence budget at 2e-2 (a 29-node table is
piecewise linear in D^6) and the omission budgets at 1e-4 (the sub-0.1 mm tail). The frozen CPU result
was re-pinned on those inputs. The runtime tests reject a real but wrong law: the rain table's Atlas
provenance.

## C.3: findings in tests merged from `main`

C.3 merged `main` (4047aa3: safety, Level II completeness, level3-polish, packaging, data-access,
l2-fixes and perf) into `real-tests` after the eight group branches. The detector reported 76 findings
in the tests `main` had added since C.1. All of them were on real data already or became so; none was
allowlisted. Most were helpers that mutate real bytes but take the bytes from their caller, so the
helper itself carried no real-data evidence: the fix folds the corpus read into the helper (it takes
the manifest id and returns the mutated file), which is also how the C.2 groups shaped their corruption
helpers.

| crate / file | findings | what was done |
|---|---:|---|
| `recast-radar-bzip2/tests/common/mod.rs`, `corruption.rs`, `real_records.rs` (+ `examples/differential_fuzz.rs`) | 12 | `ldm_records` split real LDM files by their control words but took the bytes from the caller (`magic-literal` on the `AR2V`/`BZh` checks); it now takes the manifest id and reads the file, so the 10 tests and helpers using it are evidence-carrying. The crate joined the io-nexrad group in `GROUPS`. |
| `recast-radar-io-level3/tests/*` | 25 | `common::sha256_hex` was a hand-written SHA-256 (`byte-encoding` on its padding), used by 22 items for golden digests; it now delegates to `recast_radar_testdata::sha256_hex` (new dev-dependency), with `sha256_hex_u16_be` for 16-bit level grids. `check_unknown_sizes` compares the packet code with `u16::from_be_bytes` instead of encoding it; `expected_threshold_label` splits the halfword with shifts; `generic_grid` no longer serialises decoded levels to bytes (the `Grid` carries the digest). |
| `recast-radar-io-nexrad/src/gzip.rs` | 3 | The gzip decoder tests ran on a pseudo-random "radar-like" payload. `payload(len, seed)` now returns `len` bytes of the decompressed committed gzip archive `l2-ktlx-19990504-002218-trim` (offset from `seed`), so every member, limit, truncation and corruption case is real Archive II bytes recompressed with `flate2`; `real_gzip_archive_matches_gz_decoder_and_presizes_exactly` decodes the real file itself. |
| `recast-radar-io-nexrad/src/lib.rs` (auto-merged from `main`) | 5 tests, not flagged by the detector because they merged in beside the converted helpers | `corrupt_bzip_block_error_names_the_record_path`, `whole_file_bzip2_archive_decodes_like_the_uncompressed_bytes`, `bzip2_stream_output_limit_is_exact_and_restores_the_buffer` and `oversized_bzip_block_is_rejected_at_the_per_block_limit` were rewritten from `synthetic_archive()` to real LDM records of `l2-ktlx-20240315-000217-trim` (zeroed after the bzip2 magic; the KTLX 2013 trim's decompressed bytes as one bzip2 stream; a real record's exact decoded length as the limit; a real record's bytes repeated past 16 MiB). `real_volume_exceeding_the_output_budget_is_rejected` (KIWA chunks 1-3) was kept. `bzip_buffer_pool_keeps_only_block_sized_buffers` holds no radar data. |
| `recast-radar-io-nexrad/tests/messages_msg31.rs` | 3 | `zdr_block_encoding_from_bytes` found the ZDR block with the literal `b"DZDR"`; it now matches the block type byte and `DataMomentName::from_bytes` (the crate's Table XVII-I mapping). |
| `recast-radar-io-nexrad/tests/volume_metadata.rs` | 3 | `rebuild_start_chunk` re-framed a mutated record into a start chunk passed by the caller; it now loads the committed KIWA start chunk itself. |
| `recast-radar-io-cfradial/tests/limits_real.rs`, `fuzz_regressions.rs` | 9 | `set_dimension_len` / `set_sweep_dimension` became `irene_with_dimension_len(name, len)` and `with_sweep_dimension(id, sweeps)`, which read the Irene volume or the fuzz input and return the edited file. |
| `recast-radar-io-dorade/tests/limits_real.rs` | 3 | `write_i32` became `with_i32(id, endian, at, value)`, reading the sweep and returning it with one `i32` replaced (the closures locate the block and check the real value first). |
| `recast-radar-io-jma/tests/limits_real.rs` | 5 | `set_grid` became `rs47773_n5_with_grid(gates, radials)`; the `b"GRIB"` check moved from `first_grid_section` into the unmodified-tar test. |
| `recast-radar-io-odim/tests/limits_real.rs` | 5 | `set_plane_dims` became `bejab_with_plane_dims(rays, bins)`. |
| `recast-radar-data/tests/fixtures/listings/chunks/*` | 5 data files | The five TLAS chunks the recorded `ChunkIterator` cassettes downloaded (real S3 objects, hashes in the fixtures README) were binaries outside every manifest. They moved to `testdata/files/level2-chunks/` as `l2chunk-tlas-{998-20260917-012843-001-s, 999-20260917-013443-001-s, 3-20260917-015242-001-s, -002-i, -003-i}` in `testdata/level2/manifest.toml`; `tests/iterator.rs` resolves a cassette's `file` body through `recast_radar_testdata::path("l2chunk-<name>")`, and `tests/timing.rs` and `tests/volume_fetch_retry.rs` use the ids. |
| `recast-radar-data/tests/volume_fetch_retry.rs` | 3 | `serve` wrote HTTP headers around a chunk passed in by the caller; it now reads `l2chunk-tlas-3-20260917-015242-003-i` itself and returns the bytes. |

Other changes at the merge: `crates/recast-radar-io/tests/router_real_files.rs` kept the C.2 real
Archive II tests over `main`'s edited synthetic one; `crates/recast-radar-data/src/international.rs`
kept the C.2 captured-listing providers with `main`'s `#[cfg(feature = "net")]` gates; the
`recast-radar-scattering` frozen-bits test keeps `main`'s per-target comparison over the C.2 real
inputs; the `recast-radar-io-nexrad` `bzip2` dependency is a dev-dependency (the reference encoder for
recompressing real bytes) now that the library decodes with `recast-radar-bzip2`.

## Follow-ups

- correct: `l2-klix-20050829-130035-trim` (Katrina, Message 1) can join `tools/correct_golden.py`
  `CASES` and `region_dealias_recovers_smooth_folded_ramp` /
  `velocity_dealias_preserves_supported_adjacent_folds` now that the Message 1 Nyquist offset fix
  (io-nexrad C.2) is merged.
- render-bench: `boundary_metric_counts_the_wrap_seam` can move to the committed KLIX 2005 fixture (18
  seam pairs) for the same reason.
- retrieve/track: `recast_radar_retrieve::detect_rotation_sites` does not detect the Moore 2013-05-20
  20:16Z tornado circulation at 22 km in `l2-ktlx-20130520-201643` (library behaviour, documented in
  the retrieve and track sections).
- render: the compact sample-cache render path and the direct render disagree on transparent palette
  entries (render-bench section).
- track: `identify_storm_cells` assumes full-circle radials, inflating sector-scan cell areas.
- core: `MomentGrid`'s gate-count expansion path has no real sample (no corpus sweep has its longest
  radial after a shorter one).
- scattering: `PsdFallSpeedAuthority::SyntheticTestOnly` remains as a library enum variant no test uses.

## Corpus additions needed

Inputs proposed above that are not in the corpus yet (the io-formats additions were made; see that
section):

| group | input | for |
|---|---|---|
| io-nexrad | a real GR2 `.msg31` export (back-to-back Message 31 records) | `decodes_gr2_style_variable_framed_msg31_records` |
| correct | added in C.2: `l2-klix-20210829-175748` and `l2-klix-20210829-173117` | v4 temporal-reference tests |
| track (added in C.2) | the KDVN derecho volumes 17:57, 18:10 and 18:17Z around `l2-kdvn-20200810-180401` and the Level III STI products of all four (`l3-kdvn-20200810-*-nst`) | `tests/tracking_real.rs` |

The core-data-scattering additions (per-quantity ODIM parts of one scan, NHC/GDACS captures, the WRF P3
tables, the PyTMatrix 0.3.3 tables with the held-out report) were made in C.2; see that group's section.

Three proposals depend on checking a corpus file first and need an addition only if the check fails: a
real volume without wavelength metadata (`unknown_band_blocks_band_sensitive_products_but_keeps_phif`),
valid native KDP (`native_kdp_is_preserved`; the decoded DORADE KDP fields are all missing) and
range-folded reflectivity codes (`reflectivity_range_folded_bins_render_table_rf_color`).

## Reviewed, not flagged

These tests were read while building the detector and are not findings. They are listed so the conversion
groups can decide on them; the detector enforces none of them.

- Hand-written 1-D gate rows fed to kernels (no model type, no field allocation the detector can tell
  from math arrays):
  - `crates/recast-radar-retrieve/src/sweep.rs`: `tests::unwraps_phase_crossing_zero`,
    `tests::unwrap_does_not_bridge_long_missing_phase_gap`, `tests::hampel_filter_removes_spike_in_flat_window`
  - `crates/recast-radar-retrieve/src/wind.rs`: `tests::convergence_window_finds_couplet`,
    `tests::median_qc_suppresses_single_gate_spike`
  - `crates/recast-radar-io-dorade/src/dorade.rs`: `tests::rle_run_of_missing_gates_pads_with_bad` (HRD RLE words)

  Each can take real rays instead (PHIDP rays of `l2-ktlx-20130520-201643-trim` sweep 1, velocity rays of
  `l2-kdvn-20200810-180401-trim` sweep 2, RLE words of `dorade-cow2-20260521-225514-sur-head24`), or become a
  documented pure-math exception.
- Published reference vectors: `crates/recast-radar-io-jma/src/lib.rs`
  `tests::decodes_jma_run_length_reference_example`, `crates/recast-radar-io-odim/src/hdf5lite.rs`
  `tests::jenkins_lookup3_matches_reference_vectors`, `crates/recast-radar-retrieve/src/detect.rs`
  `tests::stumpf_worked_example_ranks_3` and `tests::stumpf_table3_vertical_rank`.
- Pure decoding primitives, geometry and tables: hdf5lite integer/offset/unshuffle readers, JMA signed
  magnitude and run-length rejections, CfRadial scan-mode vocabulary and fixed-angle fallbacks, DORADE and
  ODIM name maps, `recast-radar-core` beam geometry and refractivity, `recast-radar-correct`
  `dealias_v4::env_profile` (NWP wind profiles, not radar data), `dealias_v4::solve` and
  `dealias_v4::confidence`, `recast-radar-track` Hungarian solver and TDS thresholds, `recast-radar-map`
  beam inversion, `recast-radar-filters` factor policy, render palettes and viewport arithmetic.
- Garbage input without radar structure: `crates/recast-radar-io/src/lib.rs`
  `tests::router_stringifies_level2_error_for_unrecognized_bytes` (`b"not radar"`),
  hdf5lite `tests::truncated_messages_return_errors_instead_of_indexing`
  (`crates/recast-radar-io-nexrad/src/level3_vwp.rs` `tests::rejects_non_vwp_input` went with that
  module when `recast-radar-io-level3` subsumed it on `main`).
- `recast-radar-data`: parsers read committed real captures (`tests/fixtures/`, `src/**/fixtures/`); a few
  unit tests use short inline HTML anchors (`src/international/listing.rs`,
  `src/international/meteoromania.rs`), which are not radar data.
- Real data read outside the corpus crate (not synthetic). Still present after C.3:
  - environment-gated (ignored without the variable): `crates/recast-radar-retrieve/src/gbvtd.rs`
    `tests::pgua_frame_moment_audit` (`BOWECHO_PGUA_DIR`) and `crates/recast-radar-bench/src/main.rs`
    `tests::smoke_bench_runs_one_iteration` (`BOWECHO_BENCH_FILE`);
    `tests::decodes_real_public_level2_file_from_env` and `tests::gbvtd_on_real_hurricane_volume` were
    replaced by corpus tests in C.2;
  - `include_bytes!` of the copies under `crates/recast-radar-io-{odim,cfradial,dorade}/tests/data/`,
    each byte-identical to a committed manifest entry (the detector accepts them because their hashes
    are in the manifests).
