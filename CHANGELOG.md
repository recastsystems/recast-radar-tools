# Changelog

Changes to the recast-radar-tools crates that affect callers. All crates
share one version. Nothing has been published yet, so every entry is under
"Unreleased"; the first release will be 0.1.0. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## Unreleased

### Breaking

- `recast-radar-tools`: the data model module is `model`, not `core`
  (`recast_radar_tools::model::Volume`). A module named `core` shadowed
  Rust's built-in `core` crate in `use` paths.
- `recast-radar-core`: `Serialize` and `Deserialize` on the data model are
  behind the `serde` feature (the facade's `serde` feature turns it on).
  Without it the crate does not depend on serde. The workspace's chrono no
  longer enables its `serde` feature; the crates that need it do.
- Public enums and error types are `#[non_exhaustive]`: a `match` on one
  outside its crate needs a wildcard arm. Enums whose exhaustive matching is
  their purpose are exempt and say so in their documentation. Among them
  are `LinearTransform` and `FloatWidth` in `recast-radar-core`, which stay
  exhaustive: code that converts or writes packed values must handle every
  transform, as it must every `FieldData` variant. For the same reason the
  file writers' inputs `AttrValue`, `RangeCoord` and `fm301::Values` are
  exhaustive.
- `recast-radar-core`: deserializing a `Field`, `Sweep` or `Volume` checks
  what `Volume::seal` checks, and a document that fails is a serde error. A
  field must hold `nrays × ngates` values with its absent rows ascending
  below `nrays`, every field of a sweep must have a row for every ray, the
  per-ray arrays one entry per ray, and `sweeps[i].sweep_number` must be
  `i`. Each sweep's range coordinate, and the range gates each field covers
  on it, must stay within `bounded_read::MAX_GATES_PER_RADIAL` (16,384)
  gates (`SweepError::RangeGates`). Each extra variable, of a sweep or of
  the volume, must name one dimension per `shape` entry, and `shape` must
  multiply out to the number of values it holds (`SweepError::ExtraShape`).
  Before, a document could claim
  `u32::MAX × u32::MAX` gates over a few values, and `Field::to_physical`
  then panicked on the capacity; and a uniform range could claim up to
  `u32::MAX` gates, for which the FM301 view built a `range` array of that
  length and mapped every field onto it; and an extra variable could
  claim shape `[nrays, u32::MAX]` over four values, for which the FM301
  view reserved `nrays × u32::MAX` values and the process aborted. A volume
  serialized after `seal`,
  as every decoded volume is, reads back unchanged, unless merging gate
  spacings grew a range past the limit (decoders hold each radial to it).
- `recast-radar-io-level3`: a generic packet (28) whose radial component,
  padded to its longest radial, exceeds `packets::radial::MAX_RADIAL_CELLS`
  (2^24) cells is `Level3Error::InvalidPacket`. Before, a 1.3 MB packet
  could make `read_level3_volume` build a grid of over 100 million cells.

### Added

- A user guide (`docs/guide/`) and runnable examples for reading any
  format, physical values, downloading from AWS, dealiasing, composites and
  rendering (`crates/recast-radar-tools/examples/`).
- `recast-radar-map`: `column_base_sweep`, the sweep whose rays and gates
  the column products (`composite_reflectivity`, `echo_top`, `vil`, ...)
  are on.
- `recast-radar-core`: a real-file serde round-trip test for Level II,
  ODIM_H5, CfRadial 1 and JMA volumes.
- Every public item is documented; the workspace denies `missing_docs`.
- `recast-radar-core` re-exports every item of its `model` module at the
  crate root, so each has one short path (`recast_radar_core::RowRef`,
  `recast_radar_tools::model::SweepError`). About 36 types, among them
  `RowRef`, `Coding`, `SweepError`, `FieldError`, `Location`, `Provenance`
  and `GlobalAttrs`, were reachable only as `recast_radar_core::model::X`
  (`recast_radar_tools::model::model::X`). The `model` module is left out
  of the documentation, so each item has one page, at the crate root; its
  paths still compile. It is hidden from rustdoc only
  (`cfg_attr(doc, doc(hidden))`): a plain `doc(hidden)` also switches
  `missing_docs` off for every item in the module, so an undocumented model
  item would build.
- Every package carries both license texts, and every internal dependency
  states its version, so `cargo package` succeeds for each crate.
- `recast-radar-testdata`: a `download` feature (on by default). The facade
  uses the crate without it, so `cargo run --example` no longer builds an
  HTTPS client.

### Fixed

- `recast-radar-core`: the FM301 view no longer reserves memory by an
  extra variable's `shape` when it reorders a per-ray variable's rows.
  `ArrayBuf::take_rows` checks every row against the values before it
  reserves the output, and an overflowing `shape` is no longer multiplied
  out (it panicked in debug builds). A shape that does not describe the
  values leaves them in source order. The model's fields are public, so a
  caller could build such a variable without deserializing one.
- `recast-radar-render`: pixel azimuths come from the crate's own `atan2`,
  written in plain f64 arithmetic and rounded to f32, so they are the same
  bits on every target. `f32::atan2` calls the C library's `atan2f`, whose
  last bit differs between the Windows CRT and glibc, and a pixel on an
  azimuth bin boundary could land in the neighbouring bin on Linux. The
  Windows output is unchanged (the bench pixel checksums and the render
  fingerprints are identical), Linux with glibc or musl now prints the same
  bench checksums (before, it printed other values for all three volumes),
  and the single-core raster stages of `recast-radar-bench` take 8-10% less
  time than before on KTLX 2024 and KILX (`docs/perf/render-atan2.md`). The
  other C-library calls in drawing paths (the viewport rotation and
  storm-motion `sin`/`cos` in `recast-radar-render`, once per frame or per
  ray, and the cross-section and box-grid `hypot`/`atan2` in
  `recast-radar-map`) run in f64 and are rounded to f32, which makes a
  platform difference unlikely but does not rule it out. The render
  fingerprints are identical on Windows, Linux with glibc and Linux with
  musl. macOS has not been checked.
- `recast-radar-map`: the column products (`column_base_sweep`,
  `composite_reflectivity`, `echo_top`, `vil`, the hail products), the
  cross-sections and the box resampling skip sweeps at a fixed azimuth:
  sweep mode `rhi`, `manual_rhi` or `elevation_surveillance`, and a mode
  outside FM301 Table 301-15 with the ray geometry of an RHI
  (`sweep_looks_like_rhi`). Before, such a sweep was taken as a tilt whose
  elevation was its azimuth: on the DOW8 CfRadial RHI, `column_base_sweep`
  returned the RHI ("184.00 deg") and the products were empty fields. On a
  volume of RHIs only they now return `None`.

### Contents at this point

For orientation, what the crates do before the first release:

- Decoders into the FM301 data model: NEXRAD Level II (Message 31 and
  Message 1; uncompressed, gzip, bzip2 and LDM records, with the metadata
  messages), NEXRAD and TDWR Level III, ODIM_H5 (polar volumes and Cartesian
  products), CfRadial 1 (classic netCDF), DORADE and mobile-radar archives,
  and JMA radar GRIB2 tar archives; a format-sniffing router.
- A pure-Rust bzip2 decoder (`recast-radar-bzip2`).
- Data access: NEXRAD Level II archive and real-time chunks on AWS, site
  catalogs, international and community feeds.
- Algorithms: velocity dealiasing, gate filters and smoothing, derived
  products, composites and cross sections, storm tracking, CPU rendering,
  scattering tables.
