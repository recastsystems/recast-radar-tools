# Synthetic test inputs

Tests in recast-radar-tools must read real radar files (spec section 5, plan stream C). This page lists
every test, helper and data file in the workspace that still feeds synthetic input, and proposes a real
replacement for each one: a corpus file (manifest id) and an independent source for the expected values.

The list is the detector's output on branch `real-tests` (from `main` at `c279db3`): **376 entries**,
274 tests, 98 helpers and 4 data files. Every entry is also in
`testdata/synthetic-allowlist.toml` with `status = "pending"`, under the conversion group (plan task C.2)
that owns it.

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

| group | crates | tests | helpers | data files | entries |
|---|---|---:|---:|---:|---:|
| io-nexrad | `recast-radar-io-nexrad` | 21 | 11 | 0 | 32 |
| io-formats | `recast-radar-io-odim`, `recast-radar-io-cfradial`, `recast-radar-io-dorade`, `recast-radar-io-jma`, `recast-radar-io` | 45 | 30 | 4 | 79 |
| correct | `recast-radar-correct` | 28 | 4 | 0 | 32 |
| filters-map | `recast-radar-filters`, `recast-radar-map` | 28 | 8 | 0 | 36 |
| retrieve | `recast-radar-retrieve` | 35 | 13 | 0 | 48 |
| track | `recast-radar-track` | 22 | 8 | 0 | 30 |
| render-bench | `recast-radar-render`, `recast-radar-bench` | 27 | 5 | 0 | 32 |
| core-data-scattering | `recast-radar-core`, `recast-radar-data`, `recast-radar-scattering` | 68 | 19 | 0 | 87 |
| **all** | | **274** | **98** | **4** | **376** |

## io-nexrad

### `crates/recast-radar-io-nexrad/src/lib.rs`

A hand-assembled Archive II volume: `AR2V00000.1` header plus one or more fixed-length records, each holding a Message 31 radial built field by field (VOL/RAD blocks, 3-gate DREF/DVEL/DPHI moments), optionally wrapped in hand-made gzip or LDM bzip2 framing; Message 1 bodies written byte by byte; GR2-style back-to-back Message 31 records.

| test | real input | assertion source |
|---|---|---|
| `tests::parses_archive_volume_header` | `l2-ktlx-20240315-000217-trim` (AR2V0006, KTLX) and `l2-ktlx-19910605-162126-trim` (ARCHIVE2.001, blank ICAO) | the 24-byte volume header read from the file bytes; MetPy `Level2File` station id and volume time |
| `tests::parses_message_header` | `l2-ktlx-20240315-000217-trim` | first message header of the decompressed metadata record read from the file bytes; message type and count sequence from the corpus inspector (docs/testdata/corpus.md) |
| `tests::parses_message_31_header` | `l2-ktlx-20240315-000217-trim` (first Message 31 radial) | MetPy `Level2File.sweeps[0][0]` radial header (azimuth number and angle, elevation, radial status) and data-block pointers read from the message bytes |
| `tests::decodes_synthetic_message_31_volume` | `l2-ktlx-20240315-000217-trim` | MetPy VOL block (site latitude/longitude), VCP 212, sweep and radial counts (480/720 radials per the manifest), reflectivity gate values from Py-ART `read_nexrad_archive` |
| `tests::decodes_legacy_message_1_reflectivity_and_velocity` | `l2-ktlx-19990504-002218-trim` (Message 1, VCP 11): the first REF and VEL radial bodies passed to `parse_message_1` | gate values and Nyquist from MetPy `Level2File` and Py-ART |
| `tests::decodes_legacy_message_1_spectrum_width_with_velocity_offset` | `l2-ktlx-19990504-002218-trim` | spectrum width gates from MetPy and Py-ART (pins SW = (code - 129) / 2) |
| `tests::decodes_gzip_stream_without_normalized_buffer` | `l2-kvwx-20080415-235337` (175 KB gzip archive object, Message 31) | MetPy/Py-ART sweep and radial counts (7 sweeps, 2500 radials); compression reported as gzip |
| `tests::gzip_preview_waits_for_complete_displayable_cut` | `l2-ktlx-19990503-230052` (9 KB gzip, first cut truncated after 68 radials) | no preview; Py-ART and MetPy read 1 sweep of 68 rays (manifest) |
| `tests::gzip_preview_returns_completed_displayable_cut` | `l2-kvwx-20080415-235337` | preview equals the first cut of the full decode; radial count of MetPy sweep 0 |
| `tests::gzip_preview_callback_continues_to_full_volume` | `l2-kvwx-20080415-235337` | callback radial count equals MetPy sweep 0; full volume has 2500 radials |
| `tests::decodes_bzip_blocks_without_concatenated_normalized_buffer` | `l2-ktlx-20240315-000217-trim` (LDM bzip2 records) | compression `bzip2-blocks`; radial counts from MetPy |
| `tests::bzip_preview_waits_for_complete_displayable_cut` | `l2chunk-kiwa-307-20260917-003629-001-s` + `-002-i` (a real archive prefix: start chunk and 120 radials of the first cut) | no preview (the first cut is incomplete); chunk contents per the manifest |
| `tests::bzip_preview_returns_completed_displayable_cut` | `l2-ktlx-20240315-000217-trim` | preview cut equals the first cut of the full decode; radial count from MetPy sweep 0 |
| `tests::bzip_preview_full_decode_reuses_path_and_returns_full_volume` | `l2-ktlx-20240315-000217-trim` | callback count equals MetPy sweep 0; full decode equals the non-preview decode |
| `tests::multi_block_bzip_decode_matches_uncompressed_reference` | `l2-ktlx-20240315-000217-trim` | decode of the LDM records equals decode of the same records decompressed and framed uncompressed (both from the file); radial counts from MetPy |
| `tests::bzip_preview_fires_past_legacy_block_window` | `l2-ktlx-20240315-000217-trim` re-framed into 40-radial LDM records (real Message 31 bytes, recompressed like the trim tool) so the first cut completes after record 16 | preview radial count equals MetPy sweep 0; full decode equals the original file's decode |
| `tests::corrupt_trailing_bzip_block_yields_partial_volume` | `l2-ktlx-20240315-000217-trim` with the last LDM record's bzip2 payload zeroed (mutated real bytes) | partial volume: radial count equals MetPy on the file truncated before that record; skipped messages reported |
| `tests::corrupt_first_bzip_block_is_a_hard_error` | `l2-ktlx-20240315-000217-trim` with the first (metadata) record's payload zeroed | decode error |
| `tests::pipelined_decode_works_on_single_thread_rayon_pool` | `l2-ktlx-20240315-000217-trim` | 1-thread pool decode equals default-pool decode; radial count from MetPy |
| `tests::decodes_synthetic_16_bit_moment` | `l2-ktlx-20130520-201643-trim` (PHI 16-bit, ZDR 8-bit) and `l2-ktlx-20240315-000217-trim` (ZDR 16-bit) | word sizes from the data-block headers; PHI and ZDR gates from Py-ART `differential_phase` / `differential_reflectivity` |
| `tests::decodes_gr2_style_variable_framed_msg31_records` | needs corpus addition: a real GR2 `.msg31` export (e.g. the `GR2 MSG31/COW2/nexrad.*.msg31` member of a CSWR deployment zip) | independent Python walker over the back-to-back Message 31 records (site, radial count, azimuths) |

| helper | builds | used by |
|---|---|---|
| `tests::synthetic_archive` | single-radial Archive II volume | `tests::parses_archive_volume_header`, `tests::parses_message_header`, `tests::decodes_synthetic_message_31_volume`, `tests::decodes_gzip_stream_without_normalized_buffer`, `tests::gzip_preview_waits_for_complete_displayable_cut`, `tests::gzip_preview_returns_completed_displayable_cut`, `tests::gzip_preview_callback_continues_to_full_volume`, `tests::decodes_bzip_blocks_without_concatenated_normalized_buffer`, `tests::bzip_preview_waits_for_complete_displayable_cut`, `tests::bzip_preview_returns_completed_displayable_cut`, `tests::bzip_preview_full_decode_reuses_path_and_returns_full_volume`, `tests::corrupt_first_bzip_block_is_a_hard_error`, `tests::decodes_synthetic_16_bit_moment` |
| `tests::set_first_synthetic_radial_status` | rewrites the radial status of the synthetic radial | `tests::gzip_preview_returns_completed_displayable_cut`, `tests::gzip_preview_callback_continues_to_full_volume`, `tests::bzip_preview_returns_completed_displayable_cut`, `tests::bzip_preview_full_decode_reuses_path_and_returns_full_volume` |
| `tests::synthetic_multi_radial_archive` | multi-radial Archive II volume with chosen azimuths and statuses | `tests::multi_block_bzip_decode_matches_uncompressed_reference`, `tests::bzip_preview_fires_past_legacy_block_window`, `tests::corrupt_trailing_bzip_block_yields_partial_volume`, `tests::pipelined_decode_works_on_single_thread_rayon_pool` |
| `tests::synthetic_bzip_blocks_from_chunks` | LDM bzip2 framing around synthetic payload chunks | `tests::multi_block_bzip_decode_matches_uncompressed_reference`, `tests::bzip_preview_fires_past_legacy_block_window`, `tests::pipelined_decode_works_on_single_thread_rayon_pool` |
| `tests::synthetic_bzip_block_archive` | one-block LDM bzip2 archive around the synthetic payload | `tests::decodes_bzip_blocks_without_concatenated_normalized_buffer`, `tests::bzip_preview_waits_for_complete_displayable_cut`, `tests::bzip_preview_returns_completed_displayable_cut`, `tests::bzip_preview_full_decode_reuses_path_and_returns_full_volume` |
| `tests::synthetic_message_31_body` | Message 31 body with VOL/RAD blocks and 3-gate moments | `tests::parses_message_31_header`, `tests::decodes_gr2_style_variable_framed_msg31_records`, `tests::synthetic_archive`, `tests::synthetic_multi_radial_archive` |
| `tests::push_volume_block` | RVOL block bytes | `tests::synthetic_message_31_body` |
| `tests::push_radial_block` | RRAD block bytes | `tests::synthetic_message_31_body` |
| `tests::push_u8_moment` | 8-bit moment data block | `tests::synthetic_message_31_body` |
| `tests::push_u16_moment` | 16-bit moment data block | `tests::synthetic_message_31_body` |
| `tests::set_pointer` | Message 31 data-block pointer | `tests::synthetic_message_31_body` |


## io-formats

### `crates/recast-radar-io-cfradial/src/netcdf3.rs`

A handcrafted 3-element CDF-1 file (`tiny_cdf1`) and hand-written CDF headers with absurd counts; netCDF magic literals.

| test | real input | assertion source |
|---|---|---|
| `tests::magic_sniffer_accepts_classic_versions` | first bytes of `cfrad1-xsapr-sgp-20110520-ppi-classic` (CDF-1), the same bytes with the version byte set to 2, 5 (accepted) and 3 (rejected), and `cfrad1-xsapr-sgp-20110520-ppi-netcdf4` (HDF5 signature, rejected) | netCDF classic format specification (magic `CDF` + version 1, 2 or 5) |
| `tests::parses_handcrafted_cdf1` | `cfrad1-xsapr-sgp-20110520-ppi-classic` (CDF-1, UNLIMITED time of 40 records, range 42) | dimensions, a global attribute and a packed variable with its attributes from netCDF4-python on the same file |
| `tests::cdf5_is_rejected_with_guidance` | `cfrad1-xsapr-sgp-20110520-ppi-classic` with the version byte set to 5 (mutated real bytes) | error text mentions CDF-5 |
| `tests::rejects_absurd_header_counts_before_allocating` | `cfrad1-xsapr-sgp-20110520-ppi-classic` with the dimension count set to u32::MAX (mutated real header) | error before allocating; offsets from the netCDF classic header layout |

| helper | builds | used by |
|---|---|---|
| `tests::tiny_cdf1` | handcrafted CDF-1 file | `tests::parses_handcrafted_cdf1`, `tests::cdf5_is_rejected_with_guidance` |


### `crates/recast-radar-io-cfradial/tests/cfradial_real.rs`

Decodes `tests/data/cfrad_synth.nc`, a synthetic CfRadial 1.4 file written by `gen_cfradial_fixture.py` with ramp values.

| test | real input | assertion source |
|---|---|---|
| `decodes_synthetic_cfradial1_volume` | `cfrad1-irene-sr2-20110827-120420-sur-sweeps01` (tagged `replaces:cfrad_synth`: classic CfRadial 1.3, 2 sweeps, int8-packed DBZ/VEL, per-ray prt/nyquist/n_samples) and `cfrad1-xsapr-sgp-20110520-ppi-classic` (UNLIMITED time, record-interleaved variables) | Py-ART `read_cfradial` and xradar `open_cfradial1_datatree` goldens: site, time, fixed angles, rays per sweep, gate geometry, per-ray instrument values, sample packed gates |
| `level2_decoder_is_not_fooled_by_netcdf_magic` | bytes of `cfrad1-irene-sr2-20110827-120420-sur-sweeps01` and `cfrad1-xsapr-sgp-20110520-ppi-classic` | manifest format `cfradial1`: the HDF5 and DORADE sniffers reject both |

| helper | builds | used by |
|---|---|---|
| `FIXTURE` | includes `tests/data/cfrad_synth.nc` | `decodes_synthetic_cfradial1_volume`, `level2_decoder_is_not_fooled_by_netcdf_magic` |


### `crates/recast-radar-io-cfradial/tests/data/cfrad_synth.nc`

Data file: synthetic CfRadial 1.4 file (sha256 not in any manifest). Real replacement: `cfrad1-irene-sr2-20110827-120420-sur-sweeps01`, `cfrad1-xsapr-sgp-20110520-ppi-classic`. Delete it once no test includes it.

### `crates/recast-radar-io-cfradial/tests/data/gen_cfradial_fixture.py`

Data file: generator of `cfrad_synth.nc`. Delete with `cfrad_synth.nc`.

### `crates/recast-radar-io-dorade/src/dorade.rs`

`Synth::build` writes a DORADE sweep block by block (SSWB, VOLD, RADD, PARM, CSFD, SWIB, RYIB, RDAT) in either byte order, with 3 rays of 4 gates; tests patch blocks in place.

| test | real input | assertion source |
|---|---|---|
| `tests::decodes_big_endian_synthetic_sweep` | `dorade-cow2-20260521-225514-sur-head24` (big-endian, HRD RLE, CSFD, 3 transition rays) | independent Python DORADE block walker and RLE decoder (the method of tests/dorade_real.rs): site, scan mode, gate geometry, time offsets, bad gates |
| `tests::decodes_little_endian_rle_sweep` | `dorade-dow6-20211230-222139-rhi-head41` (little-endian HRD RLE) and `dorade-noxp-20090501-190244-ppi` (little-endian uncompressed) | Python block walker: decoded gate values per field |
| `tests::rejects_extended_parm_with_absurd_gate_count` | `dorade-cow2-20260521-225514-sur-head24` with a PARM cell count set to i32::MAX (mutated real block) | error names gates per radial |
| `tests::peek_reads_grouping_metadata_without_rays` | `dorade-cow2-20260521-225514-sur-head24` | manifest and Python walker: instrument COW2, volume 215, sweep 6, fixed angle 1.0, start 22:55:14Z, latitude |
| `tests::multi_sweep_volume_sorts_cuts_by_elevation` | needs corpus addition: two sweeps of one DORADE volume at different fixed angles (every corpus DORADE sweep set is single-elevation) | fixed angles from the Python walker; cut order and radial totals |
| `tests::mismatched_instruments_are_rejected` | `dorade-cow2-20260521-225514-sur-head24` with `dorade-noxp-20090501-190244-ppi` (COW2 vs NOXPRVP) | error: instruments do not match |
| `tests::rhi_scan_mode_is_detected_from_radd` | `dorade-dow6-20211230-222139-rhi-head41` (RADD scan mode 3, fixed azimuth 144 deg) | manifest and Python walker: scan mode, per-ray elevations |
| `tests::u16_grids_preserve_dorade_scaling` | `dorade-cow2-20260521-225514-sur-head24` | PARM scale, bias and bad value from the Python walker |

| helper | builds | used by |
|---|---|---|
| `tests::put_i16` | writes an i16 into a block | `tests::Synth::build`, `tests::rejects_extended_parm_with_absurd_gate_count`, `tests::rhi_scan_mode_is_detected_from_radd` |
| `tests::put_i32` | writes an i32 into a block | `tests::put_f32`, `tests::base_block`, `tests::Synth::build`, `tests::rejects_extended_parm_with_absurd_gate_count` |
| `tests::put_f32` | writes an f32 into a block | `tests::Synth::build`, `tests::rejects_extended_parm_with_absurd_gate_count`, `tests::multi_sweep_volume_sorts_cuts_by_elevation` |
| `tests::base_block` | empty DORADE descriptor block | `tests::Synth::build`, `tests::rejects_extended_parm_with_absurd_gate_count` |
| `tests::Synth` | synthetic sweep builder settings | `tests::decodes_big_endian_synthetic_sweep`, `tests::decodes_little_endian_rle_sweep`, `tests::peek_reads_grouping_metadata_without_rays`, `tests::multi_sweep_volume_sorts_cuts_by_elevation`, `tests::mismatched_instruments_are_rejected`, `tests::rhi_scan_mode_is_detected_from_radd`, `tests::u16_grids_preserve_dorade_scaling` |
| `tests::Synth::build` | synthetic DORADE sweep file | `tests::decodes_big_endian_synthetic_sweep`, `tests::decodes_little_endian_rle_sweep`, `tests::peek_reads_grouping_metadata_without_rays`, `tests::multi_sweep_volume_sorts_cuts_by_elevation`, `tests::mismatched_instruments_are_rejected`, `tests::rhi_scan_mode_is_detected_from_radd`, `tests::u16_grids_preserve_dorade_scaling` |
| `tests::synth_rays` | three synthetic rays | `tests::decodes_big_endian_synthetic_sweep`, `tests::decodes_little_endian_rle_sweep`, `tests::peek_reads_grouping_metadata_without_rays`, `tests::multi_sweep_volume_sorts_cuts_by_elevation`, `tests::mismatched_instruments_are_rejected`, `tests::u16_grids_preserve_dorade_scaling` |


### `crates/recast-radar-io-dorade/src/mobile_archive.rs`

`synthetic_sweep` writes a one-ray DORADE sweep; `write_zip` packs synthetic sweeps and text into a zip; grouping tests build `GroupableSweep` headers by hand.

| test | real input | assertion source |
|---|---|---|
| `tests::groups_zip_members_into_ascending_elevation_runs_per_instrument` | needs corpus addition: a real CSWR/FARM deployment zip with tilt directories and a second radar; until then a zip of real sweep members (`dorade-noxp-20090501-sweeps-tgz` members, `dorade-cow2-20260521-225514-sur-head24`) | grouping expected from member names and Python-walker headers |
| `tests::same_elevation_sequences_become_one_volume_per_sweep` | headers peeked from `dorade-noxp-20090501-190244-ppi` and `dorade-noxp-20090501-190324-ppi` (consecutive 0.5 deg single tilts) | one run per sweep; times and angles from the Python walker |
| `tests::long_time_gap_splits_an_ascending_run` | headers peeked from `dorade-noxp-20090501-190244-ppi` and `dorade-noxp-20090525-203211-sector` (24 days apart) | run split; times from the Python walker |
| `tests::rejects_archive_without_radar_members` | a zip with no radar members (no radar bytes involved; candidate `exception` if no real archive without radar members is added) | error: no radar members |
| `tests::loose_sweepfile_groups_directory_siblings_from_same_run` | sweep files extracted from `dorade-noxp-20090501-sweeps-tgz` into a temporary directory | siblings grouped per the archive member names and walker times |
| `tests::zip_sniffers_match_magic_and_extension` | first bytes of a real zip (needs corpus addition, as above) and of `dorade-noxp-20090501-sweeps-tgz` (not zip) | zip local-header signature from the PKWARE APPNOTE |

| helper | builds | used by |
|---|---|---|
| `tests::synthetic_sweep` | one-ray DORADE sweep | `tests::groups_zip_members_into_ascending_elevation_runs_per_instrument`, `tests::loose_sweepfile_groups_directory_siblings_from_same_run` |
| `tests::write_zip` | zip archive of synthetic members | `tests::groups_zip_members_into_ascending_elevation_runs_per_instrument`, `tests::rejects_archive_without_radar_members` |


### `crates/recast-radar-io-jma/src/lib.rs`

`synthetic_jma_grib2` writes a JMA polar GRIB2 message section by section; `tar_member_blocks`/`tar_archive` write ustar members; tests build two-station tars and patch headers.

| test | real input | assertion source |
|---|---|---|
| `tests::grid_axis_limits_reject_pathological_radial_tables` | section 3 of the `jma-n5-20191012-090000-rs47773` member with the radial count set above the limit (mutated real bytes) | error: grid dimensions exceed limits; section offsets from an independent Python GRIB2 section walker |
| `tests::oversized_tar_member_is_rejected_from_its_header` | `jma-n5-20191012-090000-rs47773` with the first member's size field set above the member limit | error: declares ... limit |
| `tests::sniffs_jma_tar_bytes` | `jma-n5-20191012-090000-rs47773`, `jma-n6-20191012-090000-rs47773`, and the non-JMA ustar inside `dorade-noxp-20090501-sweeps-tgz` | manifest formats; ustar magic and JMA member names from `tar -t` |
| `tests::cuts_sort_lowest_elevation_first_across_members` | `jma-n5-20191012-090000-rs47773` (26 sweeps in four descending ladders with repeated angles) | elevation order from the Python GRIB2 section walker |
| `tests::decodes_every_station_in_archive_order` | `jma-n5-20191012-090000` (20 station members) | station order from `tar -t` member names (RS47899 first) |
| `tests::site_filter_selects_one_station_by_id_or_number` | `jma-n5-20191012-090000` | RS47773 / TAKA selected by number and id; coordinates 34.6164N 135.6564E (manifest) |
| `tests::first_station_decode_takes_the_first_member_only` | `jma-n5-20191012-090000` | first member RS47899 per `tar -t` |
| `tests::station_headers_skip_gate_data_and_dedupe` | `jma-n5-20191012-090000` and `jma-n6-20191012-090000` | 20 unique stations with coordinates from the Python GRIB2 walker |
| `tests::repeated_station_members_merge_into_one_volume` | members of `jma-n5-20191012-090000-rs47773` and `jma-n6-20191012-090000-rs47773` in one tar (real members) | one TAKA volume with reflectivity and velocity; sweep counts 26 and 13 (manifest) |
| `tests::corrupt_member_is_skipped_but_alone_is_an_error` | `jma-n5-20191012-090000` with one member's GRIB2 bytes zeroed; that member alone | 19 stations decode; the lone corrupt member is an error |
| `tests::truncated_tar_member_is_an_error_not_a_panic` | `jma-n5-20191012-090000-rs47773` truncated inside the member data | error, no panic |

| helper | builds | used by |
|---|---|---|
| `tests::push_u16` | big-endian u16 writer | `tests::synthetic_jma_grib2_at_elevation`, `tests::grid_axis_limits_reject_pathological_radial_tables` |
| `tests::push_u32` | big-endian u32 writer | `tests::synthetic_jma_grib2_at_elevation`, `tests::grid_axis_limits_reject_pathological_radial_tables` |
| `tests::section` | GRIB2 section with length prefix | `tests::synthetic_jma_grib2_at_elevation`, `tests::grid_axis_limits_reject_pathological_radial_tables` |
| `tests::synthetic_jma_grib2` | JMA polar GRIB2 message for one station | `tests::two_station_tar`, `tests::station_headers_skip_gate_data_and_dedupe`, `tests::repeated_station_members_merge_into_one_volume`, `tests::corrupt_member_is_skipped_but_alone_is_an_error` |
| `tests::synthetic_jma_grib2_at_elevation` | JMA polar GRIB2 message at a chosen elevation | `tests::synthetic_jma_grib2`, `tests::cuts_sort_lowest_elevation_first_across_members` |
| `tests::tar_member_blocks` | ustar header and data blocks | `tests::tar_archive` |
| `tests::tar_archive` | tar archive of members | `tests::two_station_tar`, `tests::sniffs_jma_tar_bytes`, `tests::cuts_sort_lowest_elevation_first_across_members`, `tests::station_headers_skip_gate_data_and_dedupe`, `tests::repeated_station_members_merge_into_one_volume`, `tests::corrupt_member_is_skipped_but_alone_is_an_error` |
| `tests::two_station_tar` | two-station JMA tar | `tests::sniffs_jma_tar_bytes`, `tests::decodes_every_station_in_archive_order`, `tests::site_filter_selects_one_station_by_id_or_number`, `tests::first_station_decode_takes_the_first_member_only`, `tests::truncated_tar_member_is_an_error_not_a_panic` |


### `crates/recast-radar-io-odim/src/hdf5lite.rs`

HDF5 signature literals and hand-written 40-byte object headers and B-tree nodes with self-referencing addresses.

| test | real input | assertion source |
|---|---|---|
| `tests::magic_sniffer_matches_signature_only` | first bytes of `odim-bejab-20190606-0000-pvol` (accepted), the same prefix cut to 7 bytes, `cfrad1-xsapr-sgp-20110520-ppi-classic` and `l2-ktlx-20240315-000217-trim` (rejected) | HDF5 format specification superblock signature |
| `tests::v1_object_header_rejects_continuation_cycle` | `odim-bejab-20190606-0000-pvol` (superblock v0, v1 object headers) with an object header continuation address pointed back at its own header (mutated real bytes) | error mentions a cycle; header addresses located with h5py low-level `h5o`/`h5g` info |
| `tests::btree_walks_reject_self_references` | `odim-bejab-20190606-0000-pvol` with a group B-tree child pointer and a chunk B-tree child pointer set to the node's own address | error mentions a cycle; node addresses from h5py (dataset `id.get_offset`, chunk info) |


### `crates/recast-radar-io-odim/src/odim.rs`

One-ray float `MomentGrid`s and an `ElevationCut` with hand-picked reflectivity/velocity gates for the copied-`what`-group recovery.

| test | real input | assertion source |
|---|---|---|
| `tests::copied_whatgroup_recovery_masks_only_no_echo_offset_gates` | `odim-espdg-20260707-1927-pvol-dbzh-vradh` (VRADH `what` copies the DBZH sentinels: nodata 95.5, undetect -32.0, checked with h5py) | expected mask computed from the h5py raw DBZH/VRADH planes |
| `tests::distinct_velocity_sentinels_are_never_reflectivity_gated` | `odim-dkrom-20260820-1130-pvol` (VRAD gain/offset differ from DBZH) | velocity equals h5py raw * gain + offset with nodata/undetect masked |

| helper | builds | used by |
|---|---|---|
| `tests::float_grid` | one-ray float moment grid | `tests::copied_sentinel_cut` |
| `tests::copied_sentinel_cut` | cut with 4 hand-picked Z/V gates | `tests::copied_whatgroup_recovery_masks_only_no_echo_offset_gates`, `tests::distinct_velocity_sentinels_are_never_reflectivity_gated` |


### `crates/recast-radar-io-odim/tests/data/gen_odim_fixture.py`

Data file: generator of `odim_pvol_synth.h5`. Delete with `odim_pvol_synth.h5`.

### `crates/recast-radar-io-odim/tests/data/odim_pvol_synth.h5`

Data file: synthetic ODIM PVOL (sha256 not in any manifest). Real replacement: `odim-iesha-20260305-0115-pvol`. Delete it once no test includes it.

### `crates/recast-radar-io-odim/tests/odim_real.rs`

Decodes `tests/data/odim_pvol_synth.h5`, a synthetic PVOL written by `gen_odim_fixture.py` with ramp values.

| test | real input | assertion source |
|---|---|---|
| `decodes_synthetic_odim_pvol` | `odim-iesha-20260305-0115-pvol` (tagged `replaces:odim_pvol_synth`: 10 sweeps 0.5-90 deg, DBZH/TH/VRADH, gate counts by tier) | h5py raw planes and `what`/`where`/`how` attributes; xradar `open_odim_datatree` sweep geometry |
| `non_odim_hdf5_is_rejected_with_guidance` | `cfrad1-xsapr-sgp-20110520-ppi-netcdf4` (HDF5 container without ODIM `Conventions`) | error guidance text |

| helper | builds | used by |
|---|---|---|
| `FIXTURE` | includes `tests/data/odim_pvol_synth.h5` | `decodes_synthetic_odim_pvol`, `non_odim_hdf5_is_rejected_with_guidance` |


### `crates/recast-radar-io/src/lib.rs`

Format sniffing on hand-written headers (DORADE `VOLD`, HDF5, CDF, a zeroed tar with a JMA member name, AR2V, gzip) and a hand-assembled zip local-file header.

| test | real input | assertion source |
|---|---|---|
| `tests::sniffs_supported_volume_formats_in_router_order` | leading bytes of `dorade-cow2-20260521-225514-sur-head24`, `odim-bejab-20190606-0000-pvol`, `cfrad1-xsapr-sgp-20110520-ppi-classic`, `jma-n5-20191012-090000-rs47773`, `l2-ktlx-20240315-000217-trim`, `l2-kvwx-20080415-235337` (gzip) and the non-JMA ustar inside `dorade-noxp-20090501-sweeps-tgz` | each id's manifest `format` (the tar and gzip fall through to Level II) |
| `tests::unwraps_zip_local_member_stream_without_central_directory` | needs corpus addition: one real Australia NCI THREDDS `{site}_{date}.pvol.zip/{member}.pvol.h5` response (the zip local-member stream `recast-radar-data` `australia_nci` requests) | unwrapped bytes equal the member extracted with Python `zipfile` and open in h5py |


### `crates/recast-radar-io/tests/router_real_files.rs`

Routes the synthetic ODIM and CfRadial fixtures alongside real files, and a byte-for-byte copy of the io-nexrad synthetic single-radial Archive II volume.

| test | real input | assertion source |
|---|---|---|
| `router_matches_direct_odim_decoder_on_real_pvols` | drop the synthetic row; add `odim-iesha-20260305-0115-pvol` and `odim-dkrom-20260820-1130-pvol` | routed decode equals direct decode; site ids IESHA/DKROM |
| `router_matches_direct_cfradial_decoder_on_classic_netcdf` | drop the synthetic row; add `cfrad1-irene-sr2-20110827-120420-sur-sweeps01` | routed decode equals direct decode |
| `image_decoder_and_volume_router_remain_separate` | `odim-iesha-20260305-0115-pvol` (PVOL) in place of the synthetic PVOL | image decoder rejects the PVOL |
| `router_decodes_synthetic_archive_ii_same_as_direct_decoder` | `l2-ktlx-20240315-000217-trim`, `l2-ktlx-19990504-002218-trim` (Message 1), `l2-kvwx-20080415-235337` (gzip) | routed decode equals `recast_radar_io_nexrad::decode_volume_from_bytes` |
| `router_matches_direct_archive_ii_decoder_on_synthetic_volume` | same files as above | routed decode equals direct decode; site ids from the manifest |

| helper | builds | used by |
|---|---|---|
| `ODIM_SYNTH` | includes `odim_pvol_synth.h5` | `router_matches_direct_odim_decoder_on_real_pvols`, `image_decoder_and_volume_router_remain_separate` |
| `CFRADIAL_SYNTH` | includes `cfrad_synth.nc` | `router_matches_direct_cfradial_decoder_on_classic_netcdf` |
| `synthetic_archive_ii` | single-radial Archive II volume | `router_decodes_synthetic_archive_ii_same_as_direct_decoder`, `router_matches_direct_archive_ii_decoder_on_synthetic_volume` |
| `synthetic_message_31_body` | Message 31 body | `synthetic_archive_ii` |
| `push_volume_block` | RVOL block | `synthetic_message_31_body` |
| `push_radial_block` | RRAD block | `synthetic_message_31_body` |
| `push_u8_moment` | 8-bit moment block | `synthetic_message_31_body` |
| `set_pointer` | data-block pointer | `synthetic_message_31_body` |


## correct

### `crates/recast-radar-correct/src/dealias_v4/merge.rs`

`vec![f32::NAN; rows * gates]` velocity fields filled by hand: an aliased island across a gap, a +-14 m/s couplet, an isolated speck, a wrapped uniform wind.

| test | real input | assertion source |
|---|---|---|
| `tests::bridged_pairs_resolve_an_isolated_island_across_a_gap` | a real velocity window (rows x gates, per-row Nyquist, azimuths) cut from `l2-pahg-20250909-212549` (stratiform; Py-ART region-based unfolds 4,120 of 404,963 gates at 0.53 deg) around an aliased echo island separated by no-data gates | island fold from Py-ART region-based on the same sweep |
| `tests::aggregation_preserves_an_embedded_shear_couplet` | a real velocity window (rows x gates, per-row Nyquist, azimuths) cut from `l2-ktlx-20130520-201643-trim` sweep 2 around the Moore tornado couplet | no region unfolded where Py-ART also leaves the couplet unfolded |
| `tests::uncorroborated_bridge_welds_without_unwrapping` | a real velocity window (rows x gates, per-row Nyquist, azimuths) cut from `l2-kdvn-20200810-180401-trim` sweep 2 (derecho, azimuth 286-346 deg, Nyquist 21.0 m/s; Py-ART region-based unfolds 38,656 of 84,964 gates) containing a speck across a gap from strong outbound flow | speck fold 0; Py-ART region-based agrees |
| `tests::merge_solve_is_deterministic` | a real velocity window (rows x gates, per-row Nyquist, azimuths) cut from `l2-kdvn-20200810-180401-trim` sweep 2 (derecho, azimuth 286-346 deg, Nyquist 21.0 m/s; Py-ART region-based unfolds 38,656 of 84,964 gates) | two solves identical |


### `crates/recast-radar-correct/src/dealias_v4/mod.rs`

`velocity_cut`/`wind_cut` build 360- or 720-radial velocity tilts from closures (analytic uniform wind, patches, islands) wrapped into +-Nyquist, assembled into `RadarVolume`s with hand-set times.

| test | real input | assertion source |
|---|---|---|
| `tests::v4_temporal_reference_recovers_a_topmost_aliased_high_tilt` | needs corpus addition: the KLIX volume before `l2-klix-20210829-180425` (about 17:58Z, same VCP), as the previous volume | current-tilt branch agrees with Py-ART region-based and with the previous volume's dealiased tilt |
| `tests::v4_lower_current_tilt_can_reference_a_folded_higher_tilt` | `l2-klix-20210829-180425` (full volume, VCP 112 with MPDA: the 0.48 deg cut at Nyquist 32.1 m/s and the 0.53 deg cut at 23.2 m/s, where Py-ART region-based unfolds 70,140 and 152,673 gates) | Py-ART `pyart.correct.dealias_region_based` on the same sweep: agreement modulo one global 2N offset, and no gate-to-gate jump above Nyquist inside regions Py-ART unfolds identically |
| `tests::v4_current_lower_tilt_fixes_an_isolated_high_tilt_branch` | `l2-klix-20210829-180425`, an isolated echo region on a higher tilt | Py-ART `pyart.correct.dealias_region_based` on the same sweep: agreement modulo one global 2N offset, and no gate-to-gate jump above Nyquist inside regions Py-ART unfolds identically |
| `tests::v4_repairs_a_folded_patch_fused_into_legitimate_inbound` | `l2-kdvn-20200810-180401` (full volume; Py-ART region-based unfolds 79,228 of 337,846 gates at 0.44 deg) | Py-ART `pyart.correct.dealias_region_based` on the same sweep: agreement modulo one global 2N offset, and no gate-to-gate jump above Nyquist inside regions Py-ART unfolds identically |
| `tests::v4_stale_temporal_volume_is_ignored` | needs corpus addition: a KLIX volume 30 min or more before `l2-klix-20210829-180425` | output identical with and without the stale previous volume |
| `tests::v4_weak_edge_subgraph_rebranches_only_with_environmental_evidence` | `l2-klix-20210829-180425` with `crates/recast-radar-bench/fixtures/dealias/env_klix_hrrr.json` (real HRRR profile) | without the profile equals the v1 region engine exactly; with it the rebranched regions agree with the profile projection and Py-ART |
| `tests::v4_branch_degenerate_volume_is_decided_by_the_environment` | `l2-ktlx-20130520-201643` with `crates/recast-radar-bench/fixtures/dealias/env_ktlx.json` (RAP analysis 2013-05-20 20Z) | with the profile, error against the profile projection below tolerance on both tilts |
| `tests::v4_stale_environment_profile_is_ignored` | `l2-klix-20210829-180425` with `crates/recast-radar-bench/fixtures/dealias/env_klix_hrrr.json` (real HRRR profile) whose `valid_time` is moved 4 h earlier (edit of a real fixture) | output identical to no profile |
| `tests::v4_solve_is_deterministic_across_runs` | `l2-klix-20210829-180425` with `crates/recast-radar-bench/fixtures/dealias/env_klix_hrrr.json` (real HRRR profile) | two solves byte-identical (grids and confidence) |
| `tests::v4_confidence_grid_reflects_decision_margins` | `l2-klix-20210829-180425` with `crates/recast-radar-bench/fixtures/dealias/env_klix_hrrr.json` (real HRRR profile) | confidence above the interior-only level where the profile covers; diagnostics report the profile |

| helper | builds | used by |
|---|---|---|
| `tests::velocity_cut` | velocity tilt from a value closure | `tests::wind_cut`, `tests::v4_current_lower_tilt_fixes_an_isolated_high_tilt_branch`, `tests::v4_repairs_a_folded_patch_fused_into_legitimate_inbound`, `tests::v4_weak_edge_subgraph_rebranches_only_with_environmental_evidence`, `tests::v4_stale_environment_profile_is_ignored` |
| `tests::wind_cut` | wrapped analytic uniform-wind tilt | `tests::v4_temporal_reference_recovers_a_topmost_aliased_high_tilt`, `tests::v4_lower_current_tilt_can_reference_a_folded_higher_tilt`, `tests::v4_stale_temporal_volume_is_ignored`, `tests::v4_branch_degenerate_volume_is_decided_by_the_environment`, `tests::v4_solve_is_deterministic_across_runs`, `tests::v4_confidence_grid_reflects_decision_margins` |


### `crates/recast-radar-correct/src/dealias_v4/repair.rs`

Hand-filled observed/reference/truth velocity arrays: a +-17 m/s couplet, a folded lobe with reference holes, an everywhere-disagreeing reference, a 2-gate speck.

| test | real input | assertion source |
|---|---|---|
| `tests::meso_couplet_survives_untouched` | a real velocity window (rows x gates, per-row Nyquist, azimuths) cut from `l2-ktlx-20130520-201643-trim` sweep 2 around the Moore couplet | gauntlet leaves every couplet gate at fold 0 |
| `tests::patch_repair_closes_its_ring` | a real velocity window (rows x gates, per-row Nyquist, azimuths) cut from the 0.53 deg cut of `l2-klix-20210829-180425` (Nyquist 23.2 m/s), with the Py-ART region-based output of that cut as the reference (its missing gates are the holes) | boundary pairs after repair do not exceed those of the Py-ART field |
| `tests::change_cap_aborts_the_patch_module` | a real velocity window (rows x gates, per-row Nyquist, azimuths) cut from `l2-kdvn-20200810-180401-trim` sweep 2 (derecho, azimuth 286-346 deg, Nyquist 21.0 m/s; Py-ART region-based unfolds 38,656 of 84,964 gates) with the reference set to the observed field plus 2N (edit of real values) | module aborts, no fold changes |
| `tests::box_median_ladder_snaps_speckle_and_converges` | a real velocity window (rows x gates, per-row Nyquist, azimuths) cut from `l2-pgua-20230524-030945-trim` sweep 2 (Mawar; Py-ART region-based unfolds 22,560 of 165,809 gates) containing isolated aliased specks | specks snapped to Py-ART's branch; converges within the round budget |


### `crates/recast-radar-correct/src/dealias_v4/super_regions.rs`

Hand-filled 8x8 and 16x8 velocity blocks joined by one contact or a long fold boundary.

| test | real input | assertion source |
|---|---|---|
| `tests::single_contact_edge_is_weak_and_splits_super_regions` | a real velocity window (rows x gates, per-row Nyquist, azimuths) cut from `l2-kdvn-20200810-180401-trim` sweep 2 (derecho, azimuth 286-346 deg, Nyquist 21.0 m/s; Py-ART region-based unfolds 38,656 of 84,964 gates) where `solve_region_folds` finds two regions touching along one gate pair | weak edge with support below 12; two super-regions |
| `tests::high_support_edge_welds_a_super_region` | a real velocity window (rows x gates, per-row Nyquist, azimuths) cut from `l2-kbox-20220129-150537-trim` sweep 2 (Py-ART region-based unfolds 13,900 of 189,629 gates) where two regions share a long unanimous fold boundary | one super-region, no weak edges; fold vote from Py-ART region-based |


### `crates/recast-radar-correct/src/lib.rs`

`test_velocity_grid_rows` and `tilt_with_uniform_wind` build velocity `ElevationCut`/`MomentGrid`s from hand-written rows, folded ramps, LCG noise patches, or an analytic uniform wind wrapped into +-Nyquist.

| test | real input | assertion source |
|---|---|---|
| `tests::lightweight_velocity_dealias_unfolds_radial_continuity` | `l2-kdvn-20200810-180401-trim` sweep 2 (derecho, azimuth 286-346 deg, Nyquist 21.0 m/s; Py-ART region-based unfolds 38,656 of 84,964 gates), a radial crossing a fold | Py-ART `pyart.correct.dealias_region_based` on the same sweep: agreement modulo one global 2N offset, and no gate-to-gate jump above Nyquist inside regions Py-ART unfolds identically |
| `tests::dealias_skip_detection_reports_nyquist_less_feeds` | `jma-n6-20191012-090000-rs47773` (JMA radial velocity; the test comment says the decoder leaves Nyquist unset: confirm on the decoded cut) and `l2-tstl-20230331-230314-trim` sweep 2 (TDWR; Py-ART reports Nyquist 0); `l2-kdvn-20200810-180401-trim` as the positive control | skip reported and output equal to the decoded input; control not skipped |
| `tests::region_dealias_recovers_smooth_folded_ramp` | `l2-klix-20050829-130035-trim` sweep 2 (Katrina, 362 radials over the full circle, Nyquist 32.1 m/s; Py-ART region-based unfolds 53,164 of 153,501 gates) and `l2-kbox-20220129-150537-trim` sweep 2 (Py-ART region-based unfolds 13,900 of 189,629 gates) | Py-ART `pyart.correct.dealias_region_based` on the same sweep: agreement modulo one global 2N offset, and no gate-to-gate jump above Nyquist inside regions Py-ART unfolds identically |
| `tests::region_dealias_does_not_propagate_errors_down_a_radial` | `l2-klix-20210829-180425-trim` sweep 2 (nearly alias-free: Py-ART region-based unfolds 11 of 63,544 gates) | every gate Py-ART leaves unchanged stays unchanged |
| `tests::region_dealias_is_deterministic_across_runs` | `l2-kdvn-20200810-180401-trim` sweep 2 (derecho, azimuth 286-346 deg, Nyquist 21.0 m/s; Py-ART region-based unfolds 38,656 of 84,964 gates) | 16 runs byte-identical |
| `tests::region_dealias_unfolds_geometrically_supported_fold` | `l2-kdvn-20200810-180401-trim` sweep 2 (derecho, azimuth 286-346 deg, Nyquist 21.0 m/s; Py-ART region-based unfolds 38,656 of 84,964 gates), a folded patch surrounded by unfolded gates | Py-ART `pyart.correct.dealias_region_based` on the same sweep: agreement modulo one global 2N offset, and no gate-to-gate jump above Nyquist inside regions Py-ART unfolds identically |
| `tests::external_harmonic_reference_selects_the_absolute_branch` | `l2-klix-20210829-180425` (full volume, VCP 112 with MPDA: the 0.48 deg cut at Nyquist 32.1 m/s and the 0.53 deg cut at 23.2 m/s, where Py-ART region-based unfolds 70,140 and 152,673 gates): reference fit on the 32.1 m/s cut, target the 23.2 m/s cut | branch agrees with Py-ART region-based output and with `crates/recast-radar-bench/fixtures/dealias/env_klix_hrrr.json` projected along the beams |
| `tests::velocity_dealias_preserves_supported_adjacent_folds` | `l2-klix-20050829-130035-trim` sweep 2 (Katrina, 362 radials over the full circle, Nyquist 32.1 m/s; Py-ART region-based unfolds 53,164 of 153,501 gates) | Py-ART `pyart.correct.dealias_region_based` on the same sweep: agreement modulo one global 2N offset, and no gate-to-gate jump above Nyquist inside regions Py-ART unfolds identically |

| helper | builds | used by |
|---|---|---|
| `tests::tilt_with_uniform_wind` | 360-radial tilt of an analytic uniform wind, wrapped | `tests::external_harmonic_reference_selects_the_absolute_branch` |
| `tests::test_velocity_grid_rows` | velocity cut and grid from hand-written rows | `tests::dealias_skip_detection_reports_nyquist_less_feeds`, `tests::region_dealias_recovers_smooth_folded_ramp`, `tests::region_dealias_does_not_propagate_errors_down_a_radial`, `tests::region_dealias_is_deterministic_across_runs`, `tests::region_dealias_unfolds_geometrically_supported_fold`, `tests::velocity_dealias_preserves_supported_adjacent_folds` |


## filters-map

### `crates/recast-radar-filters/src/gate_filter.rs`

`cut_with` builds a 1-row REF/VEL cut from hand-written gate values.

| test | real input | assertion source |
|---|---|---|
| `tests::keeps_velocity_only_where_reflectivity_clears_the_threshold` | `l2-ktlx-20240315-000217-trim` sweep 2 (REF/VEL/SW on the same radials) | Py-ART `GateFilter.exclude_below('reflectivity', threshold)` applied to Py-ART velocity |
| `tests::no_reflectivity_moment_blanks_everything` | `jma-n6-20191012-090000-rs47773` (velocity-only cut) | every velocity gate blanked |

| helper | builds | used by |
|---|---|---|
| `tests::cut_with` | REF/VEL cut from hand-written gates | `tests::keeps_velocity_only_where_reflectivity_clears_the_threshold`, `tests::no_reflectivity_moment_blanks_everything` |


### `crates/recast-radar-filters/src/interpolate.rs`

`cut_and_grid` builds full-circle cuts with evenly spaced azimuths and uniform or hand-patterned gate data (edges, folds, CC steps, sector gaps).

| test | real input | assertion source |
|---|---|---|
| `tests::geometry_subdivides_exactly` | `l2-ktlx-19990504-002218-trim` sweep 1 (Message 1: 1 deg radials, 1 km REF gates) | range and azimuth arrays from MetPy/Py-ART: 4x in both axes, annulus preserved, native rows at their azimuths |
| `tests::azimuth_wraps_between_last_and_first_row` | `l2-ktlx-19990504-002218-trim` sweep 1 (Message 1: 1 deg radials, 1 km REF gates) | sub-row azimuths between the file's last and first radial |
| `tests::uniform_field_is_unchanged_and_fine_grids_pass_through` | `l2-ktlx-19990504-002218-trim` sweep 1 (Message 1: 1 deg radials, 1 km REF gates) and `l2-ktlx-20240315-000217-trim` (0.5 deg x 250 m, passes through) | native rows equal Py-ART values; fine grid returned unchanged |
| `tests::coverage_does_not_grow` | `l2-ktlx-19990504-002218-trim` sweep 1 (Message 1: 1 deg radials, 1 km REF gates) | no upsampled gate valid where Py-ART has no parent value |
| `tests::echo_edges_use_nearest_parent_not_partial_blends` | `l2-ktlx-19990504-002218-trim` sweep 1 (Message 1: 1 deg radials, 1 km REF gates) | edge gates equal a native parent value from Py-ART |
| `tests::velocity_fold_guard_uses_nearest_parent` | `l2-klix-20050829-130035-trim` sweep 2 (Katrina, legacy 1 deg, strongly aliased velocity) | no blend across adjacent gates differing by more than Nyquist (MetPy Nyquist) |
| `tests::cc_guard_never_blends_through_the_melting_layer` | `l2-kgwx-20130601-235640` (dual-pol at 1 deg azimuth with 250 m gates, so azimuth is upsampled; storms to 60 dBZ): radials where RHOHV falls below the guard threshold next to high values | no blend across those RHOHV steps, located in Py-ART `cross_correlation_ratio` |
| `tests::sector_scan_gap_stays_native` | `dorade-noxp-20090525-203211-sector` (100 rays over -160..-60 deg) | no sub-rows across the sector gap (azimuths from the DORADE walker) |
| `tests::upsample_cost_smoke` | `l2-ktlx-19990504-002218` (full legacy volume) | completes; output dimensions from the factor policy |

| helper | builds | used by |
|---|---|---|
| `tests::radial` | radial with a given azimuth | `tests::cut_and_grid` |
| `tests::cut_and_grid` | cut and grid from azimuths and data | `tests::geometry_subdivides_exactly`, `tests::azimuth_wraps_between_last_and_first_row`, `tests::uniform_field_is_unchanged_and_fine_grids_pass_through`, `tests::coverage_does_not_grow`, `tests::echo_edges_use_nearest_parent_not_partial_blends`, `tests::velocity_fold_guard_uses_nearest_parent`, `tests::cc_guard_never_blends_through_the_melting_layer`, `tests::sector_scan_gap_stays_native`, `tests::upsample_cost_smoke` |


### `crates/recast-radar-filters/src/smooth.rs`

`grid` builds an 8x8 reflectivity grid from uniform values, a half-filled field or a 0/40 dBZ step.

| test | real input | assertion source |
|---|---|---|
| `tests::uniform_field_is_unchanged` | `l2-kdvn-20200810-180401-trim` sweep 1 reflectivity (66 dBZ core at azimuth 270 deg, 15 km) | gates whose 3x3 neighbourhood is constant (Py-ART values) are unchanged |
| `tests::steps_soften_and_coverage_does_not_grow` | `l2-kdvn-20200810-180401-trim` sweep 1 reflectivity | empty gates stay empty; edge gates keep their Py-ART value |
| `tests::interior_step_blends` | `l2-ktlx-20130520-201643-trim` sweep 1 (Moore core: 69.5 dBZ at azimuth 268 deg, 23 km) | smoothed values lie between the neighbouring Py-ART values across the steepest gradients |

| helper | builds | used by |
|---|---|---|
| `tests::grid` | 8x8 reflectivity grid | `tests::uniform_field_is_unchanged`, `tests::steps_soften_and_coverage_does_not_grow`, `tests::interior_step_blends` |


### `crates/recast-radar-map/src/rhi.rs`

`rhi_cut` builds a fan of beams at 271 deg with hand-set elevations and gates; PPI and north-wrap cuts are built by hand.

| test | real input | assertion source |
|---|---|---|
| `tests::rhi_section_samples_the_matching_beam` | `cfrad1-dow8-20211011-223602-rhi-trim3-classic` (already used by tests/rhi_real.rs) and `dorade-dow6-20211230-222139-rhi-head41` | 4/3-earth beam height of the sampled beam/gate (netCDF4 elevation and range arrays) |
| `tests::rhi_section_is_empty_above_the_top_beam` | `cfrad1-dow8-20211011-223602-rhi-trim3-classic` (already used by tests/rhi_real.rs) | pixels above the top beam height are empty |
| `tests::rhi_section_is_empty_beyond_gate_coverage` | `cfrad1-dow8-20211011-223602-rhi-trim3-classic` (already used by tests/rhi_real.rs) | pixels beyond 950 x 125 m are empty |
| `tests::rhi_heuristic_accepts_elevation_sweeps_and_rejects_ppi` | `cfrad1-dow8-20211011-223602-rhi-trim3-classic` (already used by tests/rhi_real.rs), `dorade-dow6-20211230-222139-rhi-head41` (accepted) and `l2-ktlx-20240315-000217-trim` (rejected) | scan modes from the files |
| `tests::rhi_coverage_extents_track_the_sweep` | `cfrad1-dow8-20211011-223602-rhi-trim3-classic` (already used by tests/rhi_real.rs) | coverage top and range from the file's elevations and gate count |
| `tests::azimuth_circular_mean_handles_north_wrap` | `l2-ktlx-20130520-201643-trim` sweep 1 (azimuths 123.2 through 2.7 deg, crossing north) | numpy circular mean of the file's azimuths |

| helper | builds | used by |
|---|---|---|
| `tests::rhi_cut` | synthetic RHI fan | `tests::rhi_section_samples_the_matching_beam`, `tests::rhi_section_is_empty_above_the_top_beam`, `tests::rhi_section_is_empty_beyond_gate_coverage`, `tests::rhi_heuristic_accepts_elevation_sweeps_and_rejects_ppi`, `tests::rhi_coverage_extents_track_the_sweep` |


### `crates/recast-radar-map/src/volumetric.rs`

`cut_with_ref`/`cut_with_vel` build 360-radial cuts filled with one constant value; `volume_with` stacks them into a `RadarVolume`.

| test | real input | assertion source |
|---|---|---|
| `tests::composite_takes_column_max` | `l2-kewx-20160413-022531` (full volume; lowest-sweep maximum 70.5 dBZ at azimuth 254.7 deg, 57.6 km) | Py-ART `pyart.retrieve.composite_reflectivity` / numpy column maximum on Py-ART fields |
| `tests::echo_top_rises_with_higher_tilt` | `l2-kewx-20160413-022531` (full volume; lowest-sweep maximum 70.5 dBZ at azimuth 254.7 deg, 57.6 km) | numpy echo top: highest beam height with reflectivity above threshold (4/3-earth) |
| `tests::cross_section_reconstructs_a_reflectivity_column` | `l2-ktlx-20130520-201643` (full volume) | section values equal the Py-ART gate values at the sampled beams |
| `tests::velocity_cross_section_reconstructs_velocity` | `l2-ktlx-20130520-201643` | section velocity equals the Py-ART gate values at the sampled beams |
| `tests::derived_products_handle_degraded_inputs_without_panicking` | `l2-tbwi-20230601-175101-stub` (no radials), `l2-ktlx-19990503-230052` (68 radials, REF only), `jma-n6-20191012-090000-rs47773` (velocity only), `l2-ktlx-20240315-000217-trim` (two cuts) | no panic; empty or partial outputs |
| `tests::vil_positive_for_deep_reflectivity` | `l2-kewx-20160413-022531` (full volume; lowest-sweep maximum 70.5 dBZ at azimuth 254.7 deg, 57.6 km) | numpy VIL (Greene and Clark 1972) on Py-ART reflectivity columns |
| `tests::mehs_flags_deep_intense_cores_only` | `l2-kewx-20160413-022531` (full volume; lowest-sweep maximum 70.5 dBZ at azimuth 254.7 deg, 57.6 km) and `l2-ktlx-20240515-000014` (clear air) | numpy SHI/MEHS (Witt et al. 1998) with the melting level used by the code; SPC hail reports near San Antonio 2016-04-13 |
| `tests::vil_density_is_in_physical_range` | `l2-kewx-20160413-022531` (full volume; lowest-sweep maximum 70.5 dBZ at azimuth 254.7 deg, 57.6 km) | numpy VIL density (VIL / echo top) on Py-ART fields |

| helper | builds | used by |
|---|---|---|
| `tests::cut_with_ref` | constant-reflectivity cut | `tests::composite_takes_column_max`, `tests::echo_top_rises_with_higher_tilt`, `tests::cross_section_reconstructs_a_reflectivity_column`, `tests::derived_products_handle_degraded_inputs_without_panicking`, `tests::vil_positive_for_deep_reflectivity`, `tests::mehs_flags_deep_intense_cores_only`, `tests::vil_density_is_in_physical_range` |
| `tests::volume_with` | volume from cuts | `tests::composite_takes_column_max`, `tests::echo_top_rises_with_higher_tilt`, `tests::cross_section_reconstructs_a_reflectivity_column`, `tests::velocity_cross_section_reconstructs_velocity`, `tests::derived_products_handle_degraded_inputs_without_panicking`, `tests::vil_positive_for_deep_reflectivity`, `tests::mehs_flags_deep_intense_cores_only`, `tests::vil_density_is_in_physical_range` |
| `tests::cut_with_vel` | constant-velocity cut | `tests::velocity_cross_section_reconstructs_velocity`, `tests::derived_products_handle_degraded_inputs_without_panicking` |


## retrieve

### `crates/recast-radar-retrieve/src/availability.rs`

`grid`/`cut` build cuts with zero-filled grids for chosen moment lists and radial counts.

| test | real input | assertion source |
|---|---|---|
| `tests::derive_on_demand_admits_a_cut_the_presence_gate_rejects` | `l2-ktlx-20240315-000217-trim` sweep 1 (REF/ZDR/PHI/RHO/CFP) | derivable products from the moment list (MetPy moment names) |
| `tests::derive_on_demand_never_admits_kdp` | `l2-ktlx-20240315-000217-trim` sweep 1 | KDP not admitted |
| `tests::native_moments_route_straight_through_the_presence_gate` | `l2-ktlx-20240315-000217-trim` sweep 2 (REF/VEL/SW) | native moments listed by MetPy |
| `tests::a_partial_sweep_carries_no_sources` | `l2-ktlx-19990503-230052` (68 radials of a truncated first cut) | no sources for the partial sweep |
| `tests::unknown_names_that_match_nothing_are_not_derivable` | `l2-ktlx-20240315-000217-trim` | unknown product ids rejected |

| helper | builds | used by |
|---|---|---|
| `tests::grid` | zero-filled moment grid | `tests::cut`, `tests::a_partial_sweep_carries_no_sources` |
| `tests::cut` | cut with chosen moments | `tests::derive_on_demand_admits_a_cut_the_presence_gate_rejects`, `tests::derive_on_demand_never_admits_kdp`, `tests::native_moments_route_straight_through_the_presence_gate`, `tests::a_partial_sweep_carries_no_sources`, `tests::unknown_names_that_match_nothing_are_not_derivable` |


### `crates/recast-radar-retrieve/src/detect.rs`

`tilt` paints a +-velocity couplet and a reflectivity echo onto synthetic 360-radial tilts; `volume_of` stacks them.

| test | real input | assertion source |
|---|---|---|
| `tests::vertically_continuous_couplet_is_detected` | `l2-ktlx-20130520-201643` (full volume, Moore EF5 tornado at 20:16Z) and `l2-kdgx-20230325-010651` (Rolling Fork) | detection within a few km of the NWS damage-survey track position at the volume time |
| `tests::explicit_rotation_api_never_falls_back_to_an_internal_engine` | `l2-ktlx-20130520-201643` (full volume, Moore EF5 tornado at 20:16Z) with the Py-ART region-based dealiased velocity passed explicitly | results computed from the supplied grid |
| `tests::single_tilt_couplet_is_rejected` | `l2-ktlx-20130520-201643-trim` (one Doppler tilt) | no detection |
| `tests::couplet_without_echo_is_rejected` | `l2-ktlx-20240515-000014` (clear air, biological returns) | no detection |
| `tests::quiet_volume_detects_nothing` | `l2-ktlx-20240515-000014` and `l2-kmaf-20230331-230843` (clear air) | no detection |

| helper | builds | used by |
|---|---|---|
| `tests::velocity_cut` | synthetic velocity tilt geometry | `tests::tilt` |
| `tests::f32_grid` | float grid from values | `tests::tilt` |
| `tests::tilt` | tilt with painted couplet and echo | `tests::vertically_continuous_couplet_is_detected`, `tests::explicit_rotation_api_never_falls_back_to_an_internal_engine`, `tests::single_tilt_couplet_is_rejected`, `tests::couplet_without_echo_is_rejected`, `tests::quiet_volume_detects_nothing` |
| `tests::volume_of` | volume from tilts | `tests::vertically_continuous_couplet_is_detected`, `tests::explicit_rotation_api_never_falls_back_to_an_internal_engine`, `tests::single_tilt_couplet_is_rejected`, `tests::couplet_without_echo_is_rejected`, `tests::quiet_volume_detects_nothing` |


### `crates/recast-radar-retrieve/src/gbvtd.rs`

`synthetic_vortex(_asym)` builds a `PolarVelocityField` of an analytic Rankine vortex (optionally with an imposed wavenumber-1 asymmetry) seen from a radar.

| test | real input | assertion source |
|---|---|---|
| `tests::retrieves_rankine_profile_at_true_center` | `l2-klix-20210829-180425` (full volume, Ida eye about 140 km SW) and `l2-tjua-20220918-190621` (Fiona) | tangential wind maximum and radius consistent with the NHC best track (HURDAT2) intensity at the volume time |
| `tests::simplex_recovers_the_storm_center` | `l2-klix-20210829-180425` (full volume, Ida eye about 140 km SW) | center within tolerance of the HURDAT2 position interpolated to 18:04Z |
| `tests::recovers_imposed_wavenumber1_asymmetry` | `l2-klix-20210829-180425` (full volume, Ida eye about 140 km SW) | wavenumber-1 phase consistent with the HURDAT2 storm motion; amplitude finite and bounded |

| helper | builds | used by |
|---|---|---|
| `tests::synthetic_vortex` | analytic Rankine vortex velocity field | `tests::retrieves_rankine_profile_at_true_center`, `tests::simplex_recovers_the_storm_center` |
| `tests::synthetic_vortex_asym` | analytic vortex with wavenumber-1 asymmetry | `tests::recovers_imposed_wavenumber1_asymmetry` |


### `crates/recast-radar-retrieve/src/shear.rs`

Tests build cuts with linear rotational or divergent velocity fields and degraded (NaN/sparse) variants.

| test | real input | assertion source |
|---|---|---|
| `tests::detects_linear_rotational_shear` | `l2-ktlx-20130520-201643-trim` sweep 2 (Moore couplet) | numpy linear least-squares derivative (Smith and Elmore 2004) on Py-ART region-dealiased velocity |
| `tests::shear_handles_degraded_velocity_without_panicking` | `l2-ktlx-19990503-230052` (REF only), `jma-n6-20191012-090000-rs47773` (no Nyquist) | no panic; empty or NaN output |
| `tests::detects_linear_radial_divergence` | `l2-kdvn-20200810-180401-trim` sweep 2 (derecho outflow) | numpy LLSD radial divergence on Py-ART dealiased velocity |
| `tests::from_dealiased_derivative_does_not_run_a_second_engine` | `l2-ktlx-20130520-201643-trim` sweep 2 with a Py-ART dealiased grid supplied | shear computed from the supplied grid only |


### `crates/recast-radar-retrieve/src/sweep.rs`

`f32_grid`/`cut_with_rows` build cuts from hand-written rows: linear wrapped PHIDP, gaps, out-of-bounds KDP, wrapped velocity, RHO/REF offsets, dual-pol values, unknown band.

| test | real input | assertion source |
|---|---|---|
| `tests::linear_wrapped_phi_retrieves_kdp` | `l2-ktlx-20130520-201643-trim` sweep 1 rays through the Moore core (69.5 dBZ at azimuth 268 deg, 23 km) and the lowest sweep of `l2-kewx-20160413-022531` through the hail core | Py-ART `kdp_vulpiani` (and `kdp_maesaka`) on the same rays, within tolerance |
| `tests::short_gap_is_used_for_fit_but_not_emitted_by_default` | `l2-ktlx-20130520-201643-trim` sweep 1 rays with short runs of missing PHIDP gates | no KDP emitted at missing gates; neighbouring KDP close to Py-ART |
| `tests::native_kdp_is_preserved` | a real file with valid native KDP: the KDP fields of `dorade-noxp-20090525-203211-sector` and `dorade-dow6-20211230-222139-rhi-head41` decode as all missing, so check `KDP_F` of the DOW6 sweep (decoded as an unknown moment); otherwise a corpus addition (e.g. an ODIM PVOL with KDP) | KDP equals the values read with the independent reader |
| `tests::filtered_phase_survives_when_kdp_is_out_of_bounds` | `l2-ktlx-20130520-201643-trim` sweep 1 (Moore core) | filtered PHIDP present where KDP is rejected |
| `tests::velocity_range_gradient_uses_nyquist_wrapped_delta` | `l2-kdvn-20200810-180401-trim` sweep 2 (derecho, azimuth 286-346 deg, Nyquist 21.0 m/s; Py-ART region-based unfolds 38,656 of 84,964 gates) | numpy wrapped gate-to-gate difference with MetPy Nyquist |
| `tests::rho_qc_is_aligned_by_physical_range` | `l2-kgwx-20130601-235640` (first gate 125 m) and `l2-ktlx-20240315-000217-trim` | RHO and REF gates matched by range from MetPy data-block headers |
| `tests::cdr_is_finite_for_valid_dual_pol_values` | `l2-ktlx-20130520-201643-trim` sweep 1 | Py-ART `pyart.retrieve.compute_cdr` on the same gates |
| `tests::unknown_band_blocks_band_sensitive_products_but_keeps_phif` | a real volume without wavelength metadata (check `odim-iesha-20260305-0115-pvol` `how/wavelength`; otherwise a corpus addition) | band-sensitive products absent, filtered PHIDP present |

| helper | builds | used by |
|---|---|---|
| `tests::f32_grid` | float grid from rows | `tests::linear_wrapped_phi_retrieves_kdp`, `tests::short_gap_is_used_for_fit_but_not_emitted_by_default`, `tests::native_kdp_is_preserved`, `tests::filtered_phase_survives_when_kdp_is_out_of_bounds`, `tests::velocity_range_gradient_uses_nyquist_wrapped_delta`, `tests::rho_qc_is_aligned_by_physical_range`, `tests::cdr_is_finite_for_valid_dual_pol_values`, `tests::unknown_band_blocks_band_sensitive_products_but_keeps_phif` |
| `tests::cut_with_rows` | cut geometry for rows | `tests::linear_wrapped_phi_retrieves_kdp`, `tests::short_gap_is_used_for_fit_but_not_emitted_by_default`, `tests::native_kdp_is_preserved`, `tests::filtered_phase_survives_when_kdp_is_out_of_bounds`, `tests::velocity_range_gradient_uses_nyquist_wrapped_delta`, `tests::rho_qc_is_aligned_by_physical_range`, `tests::cdr_is_finite_for_valid_dual_pol_values`, `tests::unknown_band_blocks_band_sensitive_products_but_keeps_phif` |


### `crates/recast-radar-retrieve/src/volume.rs`

`test_volume` builds a two-tilt volume with hand-set reflectivity.

| test | real input | assertion source |
|---|---|---|
| `tests::column_max_finds_upper_tilt_value` | `l2-kewx-20160413-022531` (full volume; lowest-sweep maximum 70.5 dBZ at azimuth 254.7 deg, 57.6 km) | numpy column maximum on Py-ART reflectivity |
| `tests::echo_depth_is_nonnegative` | `l2-kewx-20160413-022531` (full volume; lowest-sweep maximum 70.5 dBZ at azimuth 254.7 deg, 57.6 km) | numpy echo depth on Py-ART fields |

| helper | builds | used by |
|---|---|---|
| `tests::test_volume` | two-tilt synthetic volume | `tests::column_max_finds_upper_tilt_value`, `tests::echo_depth_is_nonnegative` |


### `crates/recast-radar-retrieve/src/vwp.rs`

`synthetic_volume` builds velocity tilts from an analytic wind profile (uniform, sheared, harmonic, outliers, sector coverage).

| test | real input | assertion source |
|---|---|---|
| `tests::uniform_wind_recovers_components_speed_and_from_direction` | `l2-kbox-20220129-150537` (widespread snow, strong winds) | Py-ART `pyart.retrieve.vad_browning` / `vad_michelson` on the same dealiased sweeps |
| `tests::vertical_shear_is_sampled_at_four_thirds_earth_beam_height` | `l2-pahg-20250909-212549` (stratiform, all tilts) | VAD levels vs Py-ART VAD at 4/3-earth beam heights |
| `tests::robust_refit_removes_large_convective_outliers` | `l2-kilx-20260418-013553` (dense convection) | refit wind closer to Py-ART VAD on the stratiform-only gates than the first fit |
| `tests::sector_scan_is_explicitly_rejected_for_azimuth_coverage` | `dorade-noxp-20090525-203211-sector` (100 deg sector) | rejected for azimuth coverage |
| `tests::unresolved_second_harmonic_is_rejected_by_residual_qc` | `l2-kilx-20260418-013553` levels where Py-ART VAD residuals are large | rejected by residual QC |
| `tests::missing_height_coverage_is_a_level_rejection_not_a_profile_error` | `l2-ktlx-20240315-000217-trim` (lowest tilt only) | levels above the coverage rejected, profile returned |
| `tests::input_contract_and_scan_mode_fail_loudly` | `cfrad1-dow8-20211011-223602-rhi-trim3-classic` (already used by tests/rhi_real.rs) (RHI) and real grids of mismatched counts | scan-mode and grid-count errors |

| helper | builds | used by |
|---|---|---|
| `tests::synthetic_volume` | analytic wind-profile volume | `tests::uniform_wind_recovers_components_speed_and_from_direction`, `tests::vertical_shear_is_sampled_at_four_thirds_earth_beam_height`, `tests::robust_refit_removes_large_convective_outliers`, `tests::sector_scan_is_explicitly_rejected_for_azimuth_coverage`, `tests::unresolved_second_harmonic_is_rejected_by_residual_qc`, `tests::missing_height_coverage_is_a_level_rejection_not_a_profile_error`, `tests::input_contract_and_scan_mode_fail_loudly` |


### `crates/recast-radar-retrieve/src/wind.rs`

`identity_grid` builds a reflectivity grid with hand-picked `radial_indices`.

| test | real input | assertion source |
|---|---|---|
| `tests::reflectivity_rows_follow_raw_radial_identity_not_row_position` | a real cut whose reflectivity grid covers a subset of the radials (check the decoded `radial_indices` of the Doppler cut of `l2-ktlx-19990504-002218-trim` or `l2-tstl-20230331-230314-trim`) | row mapping from the decoded radial indices and MetPy radial order |

| helper | builds | used by |
|---|---|---|
| `tests::identity_grid` | grid with hand-picked radial indices | `tests::reflectivity_rows_follow_raw_radial_identity_not_row_position` |


## track

### `crates/recast-radar-track/src/cells.rs`

`volume_with_field` builds a one-tilt volume from an analytic reflectivity function (Gaussian cores).

| test | real input | assertion source |
|---|---|---|
| `tests::finds_a_single_strong_cell_at_the_right_place` | `l2-kewx-20160413-022531` (full volume; lowest-sweep maximum 70.5 dBZ at azimuth 254.7 deg, 57.6 km) | cell centroid from scipy.ndimage labelling of the thresholded Py-ART reflectivity, near azimuth 255 deg, 58 km |
| `tests::weak_echo_yields_no_cells` | `l2-ktlx-20240515-000014` (clear air, max 35.5 dBZ) | no cells |
| `tests::empty_volume_yields_no_cells` | `l2-tbwi-20230601-175101-stub` (no radials) | no cells |
| `tests::watershed_splits_two_cores_in_one_envelope` | `l2-koax-20140616-205305` (Pilger twin-tornado supercells) | two cores from a scipy watershed on the thresholded Py-ART reflectivity |

| helper | builds | used by |
|---|---|---|
| `tests::volume_with_field` | one-tilt volume from an analytic field | `tests::finds_a_single_strong_cell_at_the_right_place`, `tests::weak_echo_yields_no_cells`, `tests::empty_volume_yields_no_cells`, `tests::watershed_splits_two_cores_in_one_envelope` |


### `crates/recast-radar-track/src/swath.rs`

`volume_with` builds one-tilt u8 volumes with hand-set gates for swath frames.

| test | real input | assertion source |
|---|---|---|
| `tests::max_reflectivity_takes_per_gate_maximum` | consecutive sweeps from `dorade-noxp-20090525-sweeps-tgz` (13 NOXP 0.5 deg sector sweeps, 20:32:11-20:51:27Z, about 90 s apart; from 20:33:47Z each has 170-171 rays over 200-10 deg, 1001 x 150 m gates and about 3,000 reflectivity gates at or above 30 dBZ; the first is the committed `dorade-noxp-20090525-203211-sector`) | numpy per-gate maximum of the decoded reflectivity |
| `tests::swath_covers_union_of_two_positions` | consecutive sweeps from `dorade-noxp-20090525-sweeps-tgz` (13 NOXP 0.5 deg sector sweeps, 20:32:11-20:51:27Z, about 90 s apart; from 20:33:47Z each has 170-171 rays over 200-10 deg, 1001 x 150 m gates and about 3,000 reflectivity gates at or above 30 dBZ; the first is the committed `dorade-noxp-20090525-203211-sector`) | swath coverage equals the union of valid gates |
| `tests::max_magnitude_keeps_sign_of_extreme` | consecutive sweeps from `dorade-noxp-20090525-sweeps-tgz` (13 NOXP 0.5 deg sector sweeps, 20:32:11-20:51:27Z, about 90 s apart; from 20:33:47Z each has 170-171 rays over 200-10 deg, 1001 x 150 m gates and about 3,000 reflectivity gates at or above 30 dBZ; the first is the committed `dorade-noxp-20090525-203211-sector`), velocity | numpy signed extreme per gate |
| `tests::empty_when_no_frame_has_the_moment` | `l2-tstl-20230331-230314-trim` frames (TDWR, no dual-pol) asked for ZDR | empty swath |
| `tests::picks_lowest_tilt_carrying_the_moment` | `l2-ktlx-20240315-000217-trim` (REF on sweep 1, VEL first on sweep 2) | tilt choice from MetPy moment lists |

| helper | builds | used by |
|---|---|---|
| `tests::volume_with` | one-tilt u8 volume | `tests::max_reflectivity_takes_per_gate_maximum`, `tests::swath_covers_union_of_two_positions`, `tests::max_magnitude_keeps_sign_of_extreme`, `tests::empty_when_no_frame_has_the_moment`, `tests::picks_lowest_tilt_carrying_the_moment` |


### `crates/recast-radar-track/src/temporal.rs`

`grid` builds 1-row grids from hand-written values for difference, rate and probability products.

| test | real input | assertion source |
|---|---|---|
| `tests::difference_and_trend` | consecutive sweeps from `dorade-noxp-20090525-sweeps-tgz` (13 NOXP 0.5 deg sector sweeps, 20:32:11-20:51:27Z, about 90 s apart; from 20:33:47Z each has 170-171 rays over 200-10 deg, 1001 x 150 m gates and about 3,000 reflectivity gates at or above 30 dBZ; the first is the committed `dorade-noxp-20090525-203211-sector`) | numpy difference and trend of the decoded gates |
| `tests::rate_accumulation_uses_trapezoids` | consecutive sweeps from `dorade-noxp-20090525-sweeps-tgz` (13 NOXP 0.5 deg sector sweeps, 20:32:11-20:51:27Z, about 90 s apart; from 20:33:47Z each has 170-171 rays over 200-10 deg, 1001 x 150 m gates and about 3,000 reflectivity gates at or above 30 dBZ; the first is the committed `dorade-noxp-20090525-203211-sector`) | numpy trapezoid accumulation with the sweep times |
| `tests::probability_ignores_missing_values` | consecutive sweeps from `dorade-noxp-20090525-sweeps-tgz` (13 NOXP 0.5 deg sector sweeps, 20:32:11-20:51:27Z, about 90 s apart; from 20:33:47Z each has 170-171 rays over 200-10 deg, 1001 x 150 m gates and about 3,000 reflectivity gates at or above 30 dBZ; the first is the committed `dorade-noxp-20090525-203211-sector`) | numpy probability over non-missing gates |

| helper | builds | used by |
|---|---|---|
| `tests::grid` | 1-row grid from values | `tests::difference_and_trend`, `tests::rate_accumulation_uses_trapezoids`, `tests::probability_ignores_missing_values` |


### `crates/recast-radar-track/src/tracking.rs`

`cell` builds `StormCell` detections by hand (positions, areas, dBZ) for crossing, QLCS, split, merge, speed-gate, coast and time-gate scenarios.

| test | real input | assertion source |
|---|---|---|
| `tests::crossing_cells_do_not_swap_ids` | needs corpus addition: a 30-60 min sequence of consecutive WSR-88D volumes with crossing cells, plus the Level III Storm Tracking Information (product 58) for the same volumes | cell ids compared with the Level III STI cell tracks |
| `tests::qlcs_line_no_steal` | needs corpus addition: consecutive volumes around `l2-kdvn-20200810-180401` (derecho QLCS) and their Level III STI | track continuity along the line vs STI |
| `tests::split_links_children_to_parent` | needs corpus addition: consecutive volumes with a splitting supercell (e.g. around `l2-koax-20140616-205305`) and Level III STI | split parent/children vs STI |
| `tests::merge_terminates_the_loser_with_a_link` | needs corpus addition: consecutive volumes with a cell merger and Level III STI | merge link vs STI |
| `tests::speed_gate_rejects_a_teleporting_cell` | cells detected in two real frames far apart in space: `l2-kewx-20160413-022531` and `l2-ktlx-20240315-000217` treated as successive frames (real cells, impossible motion) | no association |
| `tests::coast_and_reacquire_keeps_the_id` | needs corpus addition: consecutive volumes where a cell drops below threshold for one volume | id kept across the gap |
| `tests::time_gate_resets_everything` | cells from `l2-ktlx-20130520-201643` and `l2-ktlx-20240315-000217` (11 years apart) | tracker reset |

| helper | builds | used by |
|---|---|---|
| `tests::cell` | hand-built storm cell | `tests::crossing_cells_do_not_swap_ids`, `tests::qlcs_line_no_steal`, `tests::split_links_children_to_parent`, `tests::merge_terminates_the_loser_with_a_link`, `tests::speed_gate_rejects_a_teleporting_cell`, `tests::coast_and_reacquire_keeps_the_id`, `tests::time_gate_resets_everything` |


### `crates/recast-radar-track/src/tracks.rs`

`couplet_tilt` paints a velocity couplet and reflectivity onto a synthetic tilt; the TDS test fills REF/ZDR/CC arrays by hand.

| test | real input | assertion source |
|---|---|---|
| `tests::cartesian_frame_paints_couplet_location` | `l2-ktlx-20130520-201643` (full volume, Moore EF5 tornado at 20:16Z) | painted cell within a few km of the NWS damage-survey position at 20:16Z |
| `tests::height_cap_bounds_range_coverage` | `l2-ktlx-20130520-201643` (full volume, Moore EF5 tornado at 20:16Z) | coverage limited by 4/3-earth beam height at the cap |
| `tests::tds_gates_require_anchor_proximity_and_criteria` | `l2-ktlx-20130520-201643` (full volume, Moore EF5 tornado at 20:16Z) (its trimmed lowest sweep already has 406 gates with RHOHV < 0.8 and Z > 40 dBZ) | TDS gates within 5 km of the circulation; criteria checked with numpy on Py-ART REF/ZDR/RHOHV |

| helper | builds | used by |
|---|---|---|
| `tests::f32_grid` | float grid from values | `tests::couplet_tilt`, `tests::tds_gates_require_anchor_proximity_and_criteria` |
| `tests::full_circle_cut` | full-circle cut geometry | `tests::couplet_tilt`, `tests::tds_gates_require_anchor_proximity_and_criteria` |
| `tests::couplet_tilt` | tilt with painted couplet | `tests::cartesian_frame_paints_couplet_location`, `tests::height_cap_bounds_range_coverage` |
| `tests::volume_of` | volume from tilts | `tests::cartesian_frame_paints_couplet_location`, `tests::height_cap_bounds_range_coverage`, `tests::tds_gates_require_anchor_proximity_and_criteria` |


## render-bench

### `crates/recast-radar-bench/src/dealias_eval.rs`

`field` builds a bench `Field` from hand-written velocity rows (2-fold ramp, alternating wrap seam, one speck).

| test | real input | assertion source |
|---|---|---|
| `tests::boundary_metric_counts_a_two_fold_ramp_exactly` | raw and Py-ART-dealiased velocity of `l2-kdvn-20200810-180401-trim` sweep 2 (derecho, azimuth 286-346 deg, Nyquist 21.0 m/s; Py-ART region-based unfolds 38,656 of 84,964 gates) | boundary pairs recounted independently in numpy |
| `tests::boundary_metric_counts_the_wrap_seam` | raw velocity of `l2-klix-20050829-130035-trim` sweep 2 (Katrina, 362 radials over the full circle, Nyquist 32.1 m/s; Py-ART region-based unfolds 53,164 of 153,501 gates) | numpy count including the last-to-first radial seam |
| `tests::percent_modified_flags_whole_fold_moves_only` | raw vs Py-ART-dealiased `l2-kdvn-20200810-180401-trim` sweep 2 (derecho, azimuth 286-346 deg, Nyquist 21.0 m/s; Py-ART region-based unfolds 38,656 of 84,964 gates) | numpy percentage of gates moved by more than Nyquist |
| `tests::speck_count_finds_isolated_outliers_only` | raw velocity of `l2-pgua-20230524-030945-trim` sweep 2 | numpy isolated-speck count |

| helper | builds | used by |
|---|---|---|
| `tests::field` | bench velocity field from values | `tests::boundary_metric_counts_a_two_fold_ramp_exactly`, `tests::boundary_metric_counts_the_wrap_seam`, `tests::percent_modified_flags_whole_fold_moves_only`, `tests::speck_count_finds_isolated_outliers_only` |


### `crates/recast-radar-render/src/lib.rs`

`test_volume`/`test_u16_volume` build small u8/u16 REF and VEL cuts row by row; lookup tests build cuts with hand-picked azimuths; `derived_product_tests` builds constant-reflectivity volumes.

| test | real input | assertion source |
|---|---|---|
| `tests::velocity_range_folded_bins_render_table_rf_color` | range-folded velocity gates (raw code 1, located with MetPy raw data) in `l2-ktlx-20240315-000217-trim` sweep 2, else `l2-klix-20210829-180425-trim` | pixels at those gates carry the table's RF colour |
| `tests::reflectivity_range_folded_bins_render_table_rf_color` | a real reflectivity grid with range-folded codes (locate with MetPy raw data in the corpus; corpus addition if none) | RF colour at those gates |
| `tests::storm_relative_u8_row_palette_matches_direct_color_math` | `l2-ktlx-20240315-000217-trim` (u8 REF and VEL) | direct colour math on the same gates; Py-ART `pyart.retrieve.storm_relative_velocity` for the subtracted motion |
| `tests::custom_color_table_feeds_precomputed_u8_palette` | `l2-ktlx-20240315-000217-trim` (u8 REF and VEL) | palette lookup equals direct table sampling on real codes |
| `tests::storm_motion_basis_matches_direct_projection` | `l2-ktlx-20240315-000217-trim` (u8 REF and VEL) | basis equals direct projection per radial azimuth read from the file |
| `tests::grid_sample_cache_upper_bound_tracks_actual_radar_footprint` | `l2-ktlx-20240315-000217-trim` (u8 REF and VEL) | bound from the file's gate count and spacing |
| `tests::viewport_lookup_matches_reference_hypot_formula` | `l2-ktlx-20240315-000217-trim` (u8 REF and VEL) | the test's reference hypot lookup on the file's azimuths and gates |
| `tests::viewport_lookup_table_matches_reference_hypot_formula` | `l2-ktlx-20240315-000217-trim` (u8 REF and VEL) | reference hypot lookup |
| `tests::viewport_lookup_table_matches_rotated_viewport_lookup` | `l2-ktlx-20240315-000217-trim` (u8 REF and VEL) | rotated reference lookup |
| `tests::baked_rotation_changes_table_azimuth_bins` | `l2-ktlx-20240315-000217-trim` (u8 REF and VEL) | azimuth bins from the file's azimuths under rotation |
| `tests::viewport_row_span_covers_reference_samples` | `l2-ktlx-20240315-000217-trim` (u8 REF and VEL) | reference samples within the row span |
| `tests::azimuth_lookup_fills_wider_native_radial_sectors` | `l2-ktlx-19990504-002218-trim` sweep 1 (Message 1: 1 deg radials, 1 km REF gates) | reference lookup on the file's azimuths |
| `tests::azimuth_lookup_prefers_duplicate_row_with_longer_valid_extent` | a real sweep with duplicate azimuths: the transition rays of `dorade-cow2-20260521-225514-sur-head24` | row with the longer valid extent per the Python DORADE walker |
| `tests::compact_sample_resolution_keeps_visible_range_folded_candidates` | range-folded gates as in the velocity RF test above | RF candidates kept |
| `tests::viewport_render_uses_requested_screen_resolution` | `l2-ktlx-20240315-000217-trim` (u8 REF and VEL) | buffer dimensions |
| `tests::viewport_sample_cache_matches_direct_moment_render` | `l2-ktlx-20240315-000217-trim` (u8 REF and VEL) | cached render equals direct render |
| `tests::viewport_geometry_cache_resolves_across_compatible_products` | `l2-ktlx-20240315-000217-trim` (u8 REF and VEL) | REF and VEL share geometry |
| `tests::viewport_sample_cache_matches_direct_storm_relative_render` | `l2-ktlx-20240315-000217-trim` (u8 REF and VEL) | cached equals direct storm-relative render |
| `tests::viewport_sample_cache_rejects_mismatched_cache` | `l2-ktlx-20240315-000217-trim` (u8 REF and VEL) | moment mismatch error |
| `tests::viewport_render_rejects_wrong_sized_reusable_buffer` | `l2-ktlx-20240315-000217-trim` (u8 REF and VEL) | buffer size error |
| `tests::viewport_cache_rejects_different_volume` | `l2-ktlx-20240315-000217-trim` (u8 REF and VEL) and `l2-ktlx-20130520-201643-trim` | different-volume error |
| `tests::viewport_cache_renders_u16_palette_moments` | `l2-ktlx-20130520-201643-trim` (u16 PHI) | u16 palette render equals direct render |
| `derived_product_tests::derived_products_render_through_viewport_cache` | `l2-kewx-20160413-022531` (full volume; lowest-sweep maximum 70.5 dBZ at azimuth 254.7 deg, 57.6 km) | cached derived-product render equals direct render |

| helper | builds | used by |
|---|---|---|
| `tests::test_volume` | small u8 REF/VEL volume | `tests::velocity_range_folded_bins_render_table_rf_color`, `tests::reflectivity_range_folded_bins_render_table_rf_color`, `tests::storm_relative_u8_row_palette_matches_direct_color_math`, `tests::custom_color_table_feeds_precomputed_u8_palette`, `tests::storm_motion_basis_matches_direct_projection`, `tests::grid_sample_cache_upper_bound_tracks_actual_radar_footprint`, `tests::viewport_lookup_matches_reference_hypot_formula`, `tests::viewport_lookup_table_matches_reference_hypot_formula`, `tests::viewport_lookup_table_matches_rotated_viewport_lookup`, `tests::viewport_row_span_covers_reference_samples`, `tests::viewport_render_uses_requested_screen_resolution`, `tests::viewport_sample_cache_matches_direct_moment_render`, `tests::viewport_geometry_cache_resolves_across_compatible_products`, `tests::viewport_sample_cache_matches_direct_storm_relative_render`, `tests::viewport_sample_cache_rejects_mismatched_cache`, `tests::viewport_render_rejects_wrong_sized_reusable_buffer`, `tests::viewport_cache_rejects_different_volume` |
| `tests::test_u16_volume` | small u16 volume | `tests::viewport_cache_renders_u16_palette_moments` |
| `derived_product_tests::cut_with_ref` | constant-reflectivity cut | `derived_product_tests::derived_products_render_through_viewport_cache` |
| `derived_product_tests::volume_with` | volume from cuts | `derived_product_tests::derived_products_render_through_viewport_cache` |


## core-data-scattering

### `crates/recast-radar-core/src/lib.rs`

Model unit tests build `MomentGrid`s from hand-written u8/u16 rows and `ElevationCut`/`RadarVolume` parts (`merge_radial`, `merge_cut`, `merge_volume`) with chosen azimuths, gate layouts, times and ray metadata.

| test | real input | assertion source |
|---|---|---|
| `tests::ray_instrument_metadata_is_optional_but_must_align` | cut decoded from `cfrad1-irene-sr2-20110827-120420-sur-sweeps01` (per-ray prt/nyquist/n_samples); misalignment by dropping one ray's metadata | alignment error; values from netCDF4-python |
| `tests::moment_grid_scales_compact_u8_rows` | u8 REF data blocks of `l2-ktlx-20240315-000217-trim` | raw codes, scale and offset from MetPy data-block headers |
| `tests::moment_grid_expands_and_pads_variable_gate_rows` | rows of different gate counts from `odim-bejab-20190606-0000-pvol` (598 and 300 gates) | h5py raw rows and gate counts |
| `tests::moment_grid_pushes_u8_slice_without_row_allocation` | u8 REF data blocks of `l2-ktlx-20240315-000217-trim` | MetPy raw codes |
| `tests::moment_grid_pushes_u16_be_bytes_without_row_allocation` | u16 PHI data blocks of `l2-ktlx-20130520-201643-trim` | MetPy raw codes |
| `tests::moment_grid_reserves_rows_and_gate_storage` | dimensions of `l2-ktlx-20240315-000217-trim` sweep 1 | radial and gate counts from MetPy |
| `tests::cut_tracks_available_moments` | `l2-ktlx-20240315-000217-trim` sweeps 1 and 2 | moment lists from MetPy (REF/ZDR/PHI/RHO/CFP; REF/VEL/SW) |
| `tests::merge_single_part_is_identity_with_sorted_cuts` | `l2-kiwa-20260917-003629` | merge of one part equals the part with cuts sorted |
| `tests::merge_rejects_mismatched_site_ids` | a KIWA chunk part and `l2-ktlx-20240315-000217-trim` | site mismatch error |
| `tests::merge_keeps_earliest_volume_time` | the committed KIWA chunks `l2chunk-kiwa-307-20260917-003629-001-s`..`-003-i` (and the cached rest of the 70) decoded as separate parts; `l2-kiwa-20260917-003629` is the same bytes as one archive | earliest part time (chunk radial times per the manifest) |
| `tests::merge_unions_moments_of_elevation_matched_cuts` | `jma-n5-20191012-090000-rs47773` (REF) and `jma-n6-20191012-090000-rs47773` (VEL) of the same scan | merged cuts carry both moments; sweep angles from the GRIB2 walker |
| `tests::merge_fills_aligned_ray_instrument_metadata_without_overwriting_source_values` | `cfrad1-irene-sr2-20110827-120420-sur-sweeps01` split into a DBZ part and a VEL part (both real) | per-ray values from netCDF4-python kept |
| `tests::merge_ignores_malformed_incoming_ray_instrument_metadata` | the Irene parts with one part's ray metadata truncated (edit of real values) | malformed metadata ignored |
| `tests::merge_collision_keeps_first_part_grid` | the JMA N5 member decoded twice (same moment in both parts) | first part's grid kept |
| `tests::merge_unions_unmatched_cuts_sorted_by_elevation` | the committed KIWA chunks `l2chunk-kiwa-307-20260917-003629-001-s`..`-003-i` (and the cached rest of the 70) decoded as separate parts; `l2-kiwa-20260917-003629` is the same bytes as one archive | all cuts present, sorted by elevation |
| `tests::merge_skips_matched_cut_with_different_radial_count` | the lowest cuts of `l2-ktlx-20240315-000217-trim` (480 radials) and `l2-ktlx-19990504-002218-trim` (367 radials), both KTLX near 0.5 deg | cut skipped; radial counts from Py-ART |
| `tests::merge_skips_matched_cut_with_shifted_azimuths` | sweep 1 of `l2-ktlx-20240315-000217-trim` (480 radials from 167.3 deg) and of `l2-ktlx-20130520-201643-trim` (480 radials from 123.2 deg) | cut skipped; azimuths from Py-ART |
| `tests::merge_accepts_azimuths_equal_across_the_north_wrap` | `jma-n5-20191012-090000-rs47773` and `jma-n6-20191012-090000-rs47773` (512 radials each; azimuths from the GRIB2 walker) | matched across 0/360 |
| `tests::merge_accepts_matched_cut_with_different_gate_layout` | `odim-iesha-20260305-0115-pvol` DBZH and VRADH as separate parts if their gate layouts differ (check with h5py), else a corpus addition | merged; gate layouts from h5py |
| `tests::merge_three_product_parts_assembles_full_dual_pol_cut` | needs corpus addition: one scan delivered as per-quantity files (e.g. MeteoRomania `TIM_*dBZ/V/ZDR/KDP.hdf` or DWD per-moment sweep files listed by `recast-radar-data`) | merged cut carries every quantity; values from h5py |
| `tests::merge_jma_repeated_tilts_keep_repetition_velocity_and_renumber` | `jma-n5-20191012-090000-rs47773` (repeated elevations) and `jma-n6-20191012-090000-rs47773` | repetitions kept with velocity, sequential numbering; ladder order from the GRIB2 walker |
| `tests::volume_can_keep_repeated_elevation_cuts_separate` | `jma-n5-20191012-090000-rs47773` (26 sweeps with repeated angles) | 26 cuts from the GRIB2 walker |

| helper | builds | used by |
|---|---|---|
| `tests::merge_radial` | hand-built radial | `tests::merge_cut` |
| `tests::merge_cut` | hand-built cut | `tests::merge_single_part_is_identity_with_sorted_cuts`, `tests::merge_unions_moments_of_elevation_matched_cuts`, `tests::merge_fills_aligned_ray_instrument_metadata_without_overwriting_source_values`, `tests::merge_ignores_malformed_incoming_ray_instrument_metadata`, `tests::merge_collision_keeps_first_part_grid`, `tests::merge_unions_unmatched_cuts_sorted_by_elevation`, `tests::merge_skips_matched_cut_with_different_radial_count`, `tests::merge_skips_matched_cut_with_shifted_azimuths`, `tests::merge_accepts_azimuths_equal_across_the_north_wrap`, `tests::merge_accepts_matched_cut_with_different_gate_layout`, `tests::merge_three_product_parts_assembles_full_dual_pol_cut`, `tests::merge_jma_repeated_tilts_keep_repetition_velocity_and_renumber` |
| `tests::merge_volume` | hand-built volume | `tests::merge_single_part_is_identity_with_sorted_cuts`, `tests::merge_rejects_mismatched_site_ids`, `tests::merge_keeps_earliest_volume_time`, `tests::merge_unions_moments_of_elevation_matched_cuts`, `tests::merge_fills_aligned_ray_instrument_metadata_without_overwriting_source_values`, `tests::merge_ignores_malformed_incoming_ray_instrument_metadata`, `tests::merge_collision_keeps_first_part_grid`, `tests::merge_unions_unmatched_cuts_sorted_by_elevation`, `tests::merge_skips_matched_cut_with_different_radial_count`, `tests::merge_skips_matched_cut_with_shifted_azimuths`, `tests::merge_accepts_azimuths_equal_across_the_north_wrap`, `tests::merge_accepts_matched_cut_with_different_gate_layout`, `tests::merge_three_product_parts_assembles_full_dual_pol_cut`, `tests::merge_jma_repeated_tilts_keep_repetition_velocity_and_renumber` |


### `crates/recast-radar-data/src/international.rs`

`FakeProvider`, `FakeLoopProvider` and `FakeArchiveProvider` implement the provider traits with invented sites and frame plans (no radar bytes).

| test | real input | assertion source |
|---|---|---|
| `tests::default_recent_is_a_single_frame_and_reports_no_loop_support` | a real provider parsing a committed listing capture (`src/international/fixtures/fmi_fianj_listing.xml` or `geosphere_listing_recent.xml`) | frames and site ids from the captured listing |
| `tests::recent_source_routes_recent_and_flips_supports_recent_together` | a real provider with recent-frame support over `src/international/fixtures/ord_*_hour.xml` | frame count from the listing |
| `tests::default_archive_source_is_absent_and_reports_no_archive_support` | a real provider without archive support | trait defaults |
| `tests::archive_source_routes_day_plans_and_flips_supports_archive_together` | a real archive-capable provider over committed day listings (ORD `ord_*_hour.xml`) | day plans from the listing |
| `tests::default_archive_progress_is_object_safe_and_honors_precancel` | the same real archive-capable provider | pre-cancel honoured |
| `tests::default_window_plans_folds_days_oldest_first_and_caps_to_the_newest` | the same provider over two captured day listings | oldest-first fold and cap from listing timestamps |
| `tests::trait_contract_round_trips_through_a_boxed_provider` | a real provider behind `Box<dyn IntlProvider>` | listing-derived values round-trip; candidate `exception` (trait contract, no radar data) if no provider fits |

| helper | builds | used by |
|---|---|---|
| `tests::FakeProvider` | provider with an invented site and plan | `tests::FakeLoopProvider::list_sites`, `tests::FakeLoopProvider::latest`, `tests::FakeArchiveProvider::list_sites`, `tests::FakeArchiveProvider::latest`, `tests::default_recent_is_a_single_frame_and_reports_no_loop_support`, `tests::default_archive_source_is_absent_and_reports_no_archive_support`, `tests::trait_contract_round_trips_through_a_boxed_provider` |
| `tests::FakeLoopProvider` | provider with invented recent frames | `tests::recent_source_routes_recent_and_flips_supports_recent_together` |
| `tests::FakeLoopProvider::list_sites` | delegates to FakeProvider | `tests::recent_source_routes_recent_and_flips_supports_recent_together` |
| `tests::FakeLoopProvider::latest` | delegates to FakeProvider | `tests::recent_source_routes_recent_and_flips_supports_recent_together` |
| `tests::FakeArchiveProvider` | provider with invented archive plans | `tests::archive_source_routes_day_plans_and_flips_supports_archive_together`, `tests::default_archive_progress_is_object_safe_and_honors_precancel`, `tests::default_window_plans_folds_days_oldest_first_and_caps_to_the_newest` |
| `tests::FakeArchiveProvider::list_sites` | delegates to FakeProvider | `tests::archive_source_routes_day_plans_and_flips_supports_archive_together`, `tests::default_archive_progress_is_object_safe_and_honors_precancel`, `tests::default_window_plans_folds_days_oldest_first_and_caps_to_the_newest` |
| `tests::FakeArchiveProvider::latest` | delegates to FakeProvider | `tests::archive_source_routes_day_plans_and_flips_supports_archive_together`, `tests::default_archive_progress_is_object_safe_and_honors_precancel`, `tests::default_window_plans_folds_days_oldest_first_and_caps_to_the_newest` |


### `crates/recast-radar-data/src/tropical.rs`

`synthetic_storm` builds `TropicalCyclone` records with invented ids, names and positions for merge/failover tests (the committed feed captures have no GDACS storm inside an NHC basin).

| test | real input | assertion source |
|---|---|---|
| `tests::merge_dedupes_per_storm_not_per_basin` | needs corpus addition: same-time captures of NHC `CurrentStorms.json` and the GDACS event list that share an Atlantic/East Pacific storm, committed under `tests/fixtures/tropical/` | storm ids, names and positions from the captured JSON |
| `tests::merge_drops_gdacs_duplicate_by_name` | the same-time NHC + GDACS captures | duplicate dropped by name |
| `tests::merge_drops_gdacs_duplicate_by_position` | the same-time NHC + GDACS captures | duplicate dropped by position |
| `tests::combine_keeps_all_gdacs_storms_when_nhc_is_down` | committed `tests/fixtures/tropical/gdacs_tc_list.json` with an NHC error | all GDACS storms kept |
| `tests::empty_nhc_feed_does_not_hide_gdacs_atlantic_storm` | the same-time captures with the NHC list emptied (edit of a real capture) | GDACS Atlantic storm kept |

| helper | builds | used by |
|---|---|---|
| `tests::synthetic_storm` | invented tropical cyclone record | `tests::merge_dedupes_per_storm_not_per_basin`, `tests::merge_drops_gdacs_duplicate_by_name`, `tests::merge_drops_gdacs_duplicate_by_position`, `tests::combine_keeps_all_gdacs_storms_when_nhc_is_down`, `tests::empty_nhc_feed_does_not_hide_gdacs_atlantic_storm` |


### `crates/recast-radar-scattering/src/lut.rs`

`synthetic_node`/`synthetic_fixture` build an `OfflineLut` of analytic affine scattering values (labelled `SyntheticFixtureOnly`); `singleton_axis_fixture` builds a one-point axis variant.

| test | real input | assertion source |
|---|---|---|
| `tests::synthetic_fixture_round_trip_is_byte_deterministic` | needs corpus addition: a small real T-matrix LUT generated by `crates/recast-radar-scattering/tools/pytmatrix-0.3.3` (pyTMatrix 0.3.3), committed with its generator config | serialized bytes equal the committed LUT |
| `tests::synthetic_affine_fixture_interpolates_exactly_in_declared_axis_order` | needs corpus addition: a small real T-matrix LUT generated by `crates/recast-radar-scattering/tools/pytmatrix-0.3.3` (pyTMatrix 0.3.3), committed with its generator config | interpolated values vs pyTMatrix evaluated at the query points (tolerance) |
| `tests::prepared_plan_has_fixed_serializable_axis_ordered_layout` | needs corpus addition: a small real T-matrix LUT generated by `crates/recast-radar-scattering/tools/pytmatrix-0.3.3` (pyTMatrix 0.3.3), committed with its generator config | layout per PACK_FORMAT.md |
| `tests::prepared_execution_is_bit_identical_to_legacy_corner_order` | needs corpus addition: a small real T-matrix LUT generated by `crates/recast-radar-scattering/tools/pytmatrix-0.3.3` (pyTMatrix 0.3.3), committed with its generator config | prepared vs legacy interpolation bit-identical |
| `tests::prepared_plan_handles_singletons_and_exact_boundaries` | needs corpus addition: a small real T-matrix LUT generated by `crates/recast-radar-scattering/tools/pytmatrix-0.3.3` (pyTMatrix 0.3.3), committed with its generator config (with a singleton axis) | exact at grid nodes |
| `tests::plan_preparation_preserves_outside_axis_failures` | needs corpus addition: a small real T-matrix LUT generated by `crates/recast-radar-scattering/tools/pytmatrix-0.3.3` (pyTMatrix 0.3.3), committed with its generator config | outside-axis errors |
| `tests::interpolation_refuses_extrapolation_and_nonfinite_coordinates` | needs corpus addition: a small real T-matrix LUT generated by `crates/recast-radar-scattering/tools/pytmatrix-0.3.3` (pyTMatrix 0.3.3), committed with its generator config | errors outside the axes and for non-finite coordinates |
| `tests::payload_and_external_config_hash_mismatches_fail_closed` | needs corpus addition: a small real T-matrix LUT generated by `crates/recast-radar-scattering/tools/pytmatrix-0.3.3` (pyTMatrix 0.3.3), committed with its generator config | hash mismatch errors after mutating the real payload/config |
| `tests::embedded_config_hash_is_recomputed_not_trusted` | needs corpus addition: a small real T-matrix LUT generated by `crates/recast-radar-scattering/tools/pytmatrix-0.3.3` (pyTMatrix 0.3.3), committed with its generator config | mutated embedded hash rejected |
| `tests::digest_cannot_hide_an_invalid_additive_grid_node` | needs corpus addition: a small real T-matrix LUT generated by `crates/recast-radar-scattering/tools/pytmatrix-0.3.3` (pyTMatrix 0.3.3), committed with its generator config, a node value mutated to non-physical | rejected despite a recomputed digest |
| `tests::file_magic_and_schema_are_never_guessed` | needs corpus addition: a small real T-matrix LUT generated by `crates/recast-radar-scattering/tools/pytmatrix-0.3.3` (pyTMatrix 0.3.3), committed with its generator config, magic/schema bytes mutated | rejected |
| `tests::redundant_header_contract_rejects_mislabeled_schema_axes_and_outputs` | needs corpus addition: a small real T-matrix LUT generated by `crates/recast-radar-scattering/tools/pytmatrix-0.3.3` (pyTMatrix 0.3.3), committed with its generator config, header fields mutated | rejected |
| `tests::singleton_axis_is_exact_and_does_not_duplicate_corner_weight` | needs corpus addition: a small real T-matrix LUT generated by `crates/recast-radar-scattering/tools/pytmatrix-0.3.3` (pyTMatrix 0.3.3), committed with its generator config (with a singleton axis) | exact node values |

| helper | builds | used by |
|---|---|---|
| `tests::synthetic_node` | analytic affine scattering node | `tests::synthetic_fixture`, `tests::singleton_axis_fixture`, `tests::synthetic_affine_fixture_interpolates_exactly_in_declared_axis_order`, `tests::singleton_axis_is_exact_and_does_not_duplicate_corner_weight` |
| `tests::synthetic_fixture` | 2x2 analytic LUT | `tests::synthetic_fixture_round_trip_is_byte_deterministic`, `tests::synthetic_affine_fixture_interpolates_exactly_in_declared_axis_order`, `tests::prepared_execution_is_bit_identical_to_legacy_corner_order`, `tests::interpolation_refuses_extrapolation_and_nonfinite_coordinates`, `tests::payload_and_external_config_hash_mismatches_fail_closed`, `tests::embedded_config_hash_is_recomputed_not_trusted`, `tests::digest_cannot_hide_an_invalid_additive_grid_node`, `tests::file_magic_and_schema_are_never_guessed`, `tests::redundant_header_contract_rejects_mislabeled_schema_axes_and_outputs` |
| `tests::singleton_axis_fixture` | analytic LUT with a singleton axis | `tests::prepared_plan_has_fixed_serializable_axis_ordered_layout`, `tests::prepared_plan_handles_singletons_and_exact_boundaries`, `tests::plan_preparation_preserves_outside_axis_failures` |


### `crates/recast-radar-scattering/src/p3_table.rs`

`synthetic_table` writes a P3 lookup table in the official text layout with formula values; `parse_synthetic` parses it.

| test | real input | assertion source |
|---|---|---|
| `tests::parser_accepts_exact_two_and_three_moment_record_layouts` | needs corpus addition: the WRF `run/p3_lookupTable_1.dat-v5.4_2momI` and `_3momI` tables the crate targets (or record excerpts derived from them with provenance) | record counts and sample values read from the table text |
| `tests::parser_rejects_truncation_extra_content_nonfinite_values_and_wrong_indices` | the WRF P3 tables above, truncated / with appended content / a value set to NaN / an index changed (edits of real text) | parse errors |

| helper | builds | used by |
|---|---|---|
| `tests::synthetic_table` | P3 table text with formula values | `tests::parser_accepts_exact_two_and_three_moment_record_layouts`, `tests::parser_rejects_truncation_extra_content_nonfinite_values_and_wrong_indices` |
| `tests::parse_synthetic` | parses the synthetic table | `tests::parser_accepts_exact_two_and_three_moment_record_layouts`, `tests::parser_rejects_truncation_extra_content_nonfinite_values_and_wrong_indices` |


### `crates/recast-radar-scattering/src/scheme_psd.rs`

`synthetic_per_particle` returns analytic scattering per particle; `synthetic_fall_speed_provenance` labels an invented size-speed relation `SyntheticTestOnly`.

| test | real input | assertion source |
|---|---|---|
| `tests::node_habit_comes_from_geometry_not_category_label` | needs corpus addition: per-particle scattering from the pyTMatrix 0.3.3 generator and an authenticated fall-speed relation (e.g. the P3 table fall speeds), replacing the `SyntheticTestOnly` provenance | habit from node geometry |
| `tests::authenticated_solid_ice_closure_preserves_mass_and_shape_at_918_for_both_habits` | needs corpus addition: per-particle scattering from the pyTMatrix 0.3.3 generator and an authenticated fall-speed relation (e.g. the P3 table fall speeds), replacing the `SyntheticTestOnly` provenance | mass and shape closure at 918 kg m-3 |
| `tests::solid_ice_closure_rejects_an_unauthenticated_material_endpoint` | needs corpus addition: per-particle scattering from the pyTMatrix 0.3.3 generator and an authenticated fall-speed relation (e.g. the P3 table fall speeds), replacing the `SyntheticTestOnly` provenance | rejection |
| `tests::equal_native_axes_produce_spherical_nodes` | needs corpus addition: per-particle scattering from the pyTMatrix 0.3.3 generator and an authenticated fall-speed relation (e.g. the P3 table fall speeds), replacing the `SyntheticTestOnly` provenance | spherical nodes |
| `tests::exact_sphere_policy_represents_sub_floor_nodes_with_an_audited_route` | needs corpus addition: per-particle scattering from the pyTMatrix 0.3.3 generator and an authenticated fall-speed relation (e.g. the P3 table fall speeds), replacing the `SyntheticTestOnly` provenance | audited route |
| `tests::small_sphere_policy_does_not_bridge_nonspheres_or_other_domain_misses` | needs corpus addition: per-particle scattering from the pyTMatrix 0.3.3 generator and an authenticated fall-speed relation (e.g. the P3 table fall speeds), replacing the `SyntheticTestOnly` provenance | no bridging |
| `tests::refined_integration_closes_number_mass_d6_and_preserves_tail_audit` | needs corpus addition: per-particle scattering from the pyTMatrix 0.3.3 generator and an authenticated fall-speed relation (e.g. the P3 table fall speeds), replacing the `SyntheticTestOnly` provenance | moment closure against analytic PSD integrals |
| `tests::size_dependent_fall_moments_produce_nonzero_variance` | needs corpus addition: per-particle scattering from the pyTMatrix 0.3.3 generator and an authenticated fall-speed relation (e.g. the P3 table fall speeds), replacing the `SyntheticTestOnly` provenance | non-zero variance |
| `tests::narrow_particle_domain_fails_instead_of_clamping_nodes` | needs corpus addition: per-particle scattering from the pyTMatrix 0.3.3 generator and an authenticated fall-speed relation (e.g. the P3 table fall speeds), replacing the `SyntheticTestOnly` provenance | error |
| `tests::sub_node_width_supported_sliver_cannot_hide_domain_omission` | needs corpus addition: per-particle scattering from the pyTMatrix 0.3.3 generator and an authenticated fall-speed relation (e.g. the P3 table fall speeds), replacing the `SyntheticTestOnly` provenance | error |
| `tests::wrf_two_micron_mass_gap_is_typed_before_quadrature` | needs corpus addition: per-particle scattering from the pyTMatrix 0.3.3 generator and an authenticated fall-speed relation (e.g. the P3 table fall speeds), replacing the `SyntheticTestOnly` provenance | typed gap error |
| `tests::qnsmall_qasmall_two_micron_mass_gap_is_typed_before_quadrature` | needs corpus addition: per-particle scattering from the pyTMatrix 0.3.3 generator and an authenticated fall-speed relation (e.g. the P3 table fall speeds), replacing the `SyntheticTestOnly` provenance | typed gap error |
| `tests::unrelated_geometry_mass_gap_is_not_typed_as_wrf_two_micron_gap` | needs corpus addition: per-particle scattering from the pyTMatrix 0.3.3 generator and an authenticated fall-speed relation (e.g. the P3 table fall speeds), replacing the `SyntheticTestOnly` provenance | untyped gap |
| `tests::callback_error_retains_quadrature_level_and_node` | needs corpus addition: per-particle scattering from the pyTMatrix 0.3.3 generator and an authenticated fall-speed relation (e.g. the P3 table fall speeds), replacing the `SyntheticTestOnly` provenance | error context |
| `tests::node_budget_and_tail_limit_are_fail_closed` | needs corpus addition: per-particle scattering from the pyTMatrix 0.3.3 generator and an authenticated fall-speed relation (e.g. the P3 table fall speeds), replacing the `SyntheticTestOnly` provenance | fail closed |
| `tests::prepared_workload_preserves_level_index_and_callback_order` | needs corpus addition: per-particle scattering from the pyTMatrix 0.3.3 generator and an authenticated fall-speed relation (e.g. the P3 table fall speeds), replacing the `SyntheticTestOnly` provenance | callback order |
| `tests::prepared_cpu_finish_is_bit_identical_to_frozen_pre_refactor_result` | needs corpus addition: per-particle scattering from the pyTMatrix 0.3.3 generator and an authenticated fall-speed relation (e.g. the P3 table fall speeds), replacing the `SyntheticTestOnly` provenance | re-freeze the pinned result on the real inputs |

| helper | builds | used by |
|---|---|---|
| `tests::synthetic_per_particle` | analytic per-particle scattering | `tests::node_habit_comes_from_geometry_not_category_label`, `tests::authenticated_solid_ice_closure_preserves_mass_and_shape_at_918_for_both_habits`, `tests::equal_native_axes_produce_spherical_nodes`, `tests::exact_sphere_policy_represents_sub_floor_nodes_with_an_audited_route`, `tests::small_sphere_policy_does_not_bridge_nonspheres_or_other_domain_misses`, `tests::refined_integration_closes_number_mass_d6_and_preserves_tail_audit`, `tests::size_dependent_fall_moments_produce_nonzero_variance`, `tests::narrow_particle_domain_fails_instead_of_clamping_nodes`, `tests::sub_node_width_supported_sliver_cannot_hide_domain_omission`, `tests::node_budget_and_tail_limit_are_fail_closed`, `tests::prepared_workload_preserves_level_index_and_callback_order`, `tests::prepared_cpu_finish_is_bit_identical_to_frozen_pre_refactor_result` |
| `tests::synthetic_fall_speed_provenance` | invented fall-speed provenance | `tests::node_habit_comes_from_geometry_not_category_label`, `tests::authenticated_solid_ice_closure_preserves_mass_and_shape_at_918_for_both_habits`, `tests::solid_ice_closure_rejects_an_unauthenticated_material_endpoint`, `tests::equal_native_axes_produce_spherical_nodes`, `tests::exact_sphere_policy_represents_sub_floor_nodes_with_an_audited_route`, `tests::small_sphere_policy_does_not_bridge_nonspheres_or_other_domain_misses`, `tests::refined_integration_closes_number_mass_d6_and_preserves_tail_audit`, `tests::size_dependent_fall_moments_produce_nonzero_variance`, `tests::narrow_particle_domain_fails_instead_of_clamping_nodes`, `tests::sub_node_width_supported_sliver_cannot_hide_domain_omission`, `tests::wrf_two_micron_mass_gap_is_typed_before_quadrature`, `tests::qnsmall_qasmall_two_micron_mass_gap_is_typed_before_quadrature`, `tests::unrelated_geometry_mass_gap_is_not_typed_as_wrf_two_micron_gap`, `tests::callback_error_retains_quadrature_level_and_node`, `tests::node_budget_and_tail_limit_are_fail_closed`, `tests::prepared_workload_preserves_level_index_and_callback_order`, `tests::prepared_cpu_finish_is_bit_identical_to_frozen_pre_refactor_result` |


### `crates/recast-radar-scattering/src/tmatrix_runtime.rs`

`synthetic_speed_provenance` labels an invented size-speed relation `SyntheticTestOnly`.

| test | real input | assertion source |
|---|---|---|
| `tests::malformed_external_particle_node_still_fails_closed_after_preparation_seam` | needs corpus addition: per-particle scattering from the pyTMatrix 0.3.3 generator and an authenticated fall-speed relation (e.g. the P3 table fall speeds), replacing the `SyntheticTestOnly` provenance | fails closed |
| `tests::particle_node_query_rejects_habit_frequency_and_wrong_table_role` | needs corpus addition: per-particle scattering from the pyTMatrix 0.3.3 generator and an authenticated fall-speed relation (e.g. the P3 table fall speeds), replacing the `SyntheticTestOnly` provenance | rejections |

| helper | builds | used by |
|---|---|---|
| `tests::synthetic_speed_provenance` | invented fall-speed provenance | `tests::malformed_external_particle_node_still_fails_closed_after_preparation_seam`, `tests::particle_node_query_rejects_habit_frequency_and_wrong_table_role` |


## Corpus additions needed

Inputs proposed above that are not in the corpus yet:

| group | input | for |
|---|---|---|
| io-nexrad | a real GR2 `.msg31` export (back-to-back Message 31 records) | `decodes_gr2_style_variable_framed_msg31_records` |
| io-formats | a real CSWR/FARM deployment zip (tilt directories, second radar) | `mobile_archive` zip grouping and zip sniffing |
| io-formats | two sweeps of one DORADE volume at different fixed angles | `multi_sweep_volume_sorts_cuts_by_elevation` |
| io-formats | one Australia NCI THREDDS `{site}_{date}.pvol.zip/{member}.pvol.h5` response | `unwraps_zip_local_member_stream_without_central_directory` |
| correct | the KLIX volume before `l2-klix-20210829-180425` (about 17:58Z) and one 30 min or more earlier | v4 temporal-reference tests |
| track | 30-60 min sequences of consecutive WSR-88D volumes (crossing, splitting, merging cells; a QLCS) with the Level III Storm Tracking Information product for the same volumes | `tracking.rs` |
| core-data-scattering | one scan delivered as per-quantity files (MeteoRomania or DWD) | `merge_three_product_parts_assembles_full_dual_pol_cut` |
| core-data-scattering | same-time NHC `CurrentStorms.json` and GDACS event-list captures sharing a storm | `tropical.rs` merge tests |
| core-data-scattering | WRF `p3_lookupTable_1.dat-v5.4_2momI` / `_3momI` (or derived record excerpts) | `p3_table.rs` |
| core-data-scattering | a small pyTMatrix 0.3.3 LUT and per-particle outputs from `tools/pytmatrix-0.3.3`, with an authenticated fall-speed relation | `lut.rs`, `scheme_psd.rs`, `tmatrix_runtime.rs` |

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
  `crates/recast-radar-io-nexrad/src/level3_vwp.rs` `tests::rejects_non_vwp_input` (120 zero bytes),
  hdf5lite `tests::truncated_messages_return_errors_instead_of_indexing`.
- `recast-radar-data`: parsers read committed real captures (`tests/fixtures/`, `src/**/fixtures/`); a few
  unit tests use short inline HTML anchors (`src/international/listing.rs`,
  `src/international/meteoromania.rs`), which are not radar data.
- Real data read outside the corpus crate (not synthetic; moving them to `require_file!` is C.2 cleanup):
  - environment-gated: `crates/recast-radar-io-nexrad/src/lib.rs`
    `tests::decodes_real_public_level2_file_from_env` (`NEXRAD_LEVEL2_SAMPLE`),
    `crates/recast-radar-retrieve/src/gbvtd.rs` `tests::gbvtd_on_real_hurricane_volume`
    (`BOWECHO_GBVTD_VOLUME`) and `tests::pgua_frame_moment_audit` (`BOWECHO_PGUA_DIR`),
    `crates/recast-radar-bench/src/main.rs` `tests::smoke_bench_runs_one_iteration` (`BOWECHO_BENCH_FILE`);
  - `include_bytes!` of the copies under `crates/recast-radar-io-{odim,cfradial,dorade,nexrad}/tests/data/`,
    each byte-identical to a committed manifest entry.
