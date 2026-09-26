# Changelog

Changes to the recast-radar-tools crates that affect callers. All crates
share one version. Nothing has been published yet, so every entry is under
"Unreleased"; the first release will be 0.1.0. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## Unreleased

### Breaking

- `recast-radar-core`: `LinearTransform` has a `Levels` variant for the
  NEXRAD Level III data level encodings that are not linear (16-level
  threshold tables, high resolution VIL, enhanced echo tops), and
  `scale_factor` and `add_offset` return `Option<f64>` (`None` for a level
  table, which has no CF equivalent). The FM301 view writes such a field
  decoded, with its codes beside it as `<name>_level`.
- `recast-radar-core`: `FieldData::I32` keeps 32-bit integer fields as
  stored (netCDF-4 CfRadial files) instead of widening them to f32.
- `recast-radar-core`: `Volume::variable_attrs` holds the source's own
  attributes of variables whose values a typed slot holds, and the per-ray
  and metadata values a decoder has no typed slot for are kept in
  `Sweep::other`, `FieldAttrs::other` and `extra_vars`; Level II carries
  every metadata message, message header and per-radial value.
  `Sweep::permute_rays` and `fm301::order_rays_for_view` put a volume's
  rays in the view's order in storage (`SweepError::UnknownRayAttribute`,
  `SweepError::RayOrder`).
- `recast-radar-io`: `SupportedVolumeFormat` gains `CfRadialNetcdf4`,
  `CfRadial2` and `NexradLevel3`: the router tells HDF5 containers apart by
  content and reads NEXRAD and TDWR Level III products.
- `recast-radar-io-odim`: the ODIM decoder reads HDF5 through
  `recast-radar-hdf5` (the `hdf5lite` module is gone; `odim::hdf5` is the
  new crate).

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

- Writers. `recast-radar-io-nexrad` (feature `write`; facade feature
  `write`): any volume as NEXRAD Archive II following ICD 2620010 and ICD
  2620002 (uncompressed or bzip2 LDM records, optionally gzip), real-time
  chunks (`realtime::write_realtime_chunks`, `ChunkWriter`) and a
  GR2Analyst polling directory that follows the GRLevelX polling
  conventions (`polling::PollingDirectory`), with typed refusals and a
  `WriteSummary`. `recast-radar-io-cfradial`: `write_cfradial1` (CfRadial
  1.4, classic netCDF) and `write_cfradial2` (CfRadial 2 / FM301,
  netCDF-4). `recast-radar-io-odim`: `write_odim_h5_volume` (ODIM_H5 PVOL).
  Guide page `docs/guide/writing.md` and the `write_formats` example.
- `recast-radar-bzip2`: `Encoder` and `Level`, a pure-Rust bzip2
  compressor that writes libbzip2 1.0.8's streams, and, with the `rayon`
  feature, `EncoderPool` and `encode_many`. The Level II writer compresses
  its records with it.
- `recast-radar-hdf5` (facade feature `hdf5`): an HDF5 reader without
  unsafe code (superblocks 0 to 3, v2 object headers, fractal heaps, v2
  B-trees, every chunk index, deflate, shuffle and Fletcher-32, dense
  attributes and links), the netCDF-4 data model, and an HDF5 / netCDF-4
  writer. `recast-radar-io-cfradial` reads netCDF-4 CfRadial 1 and CfRadial
  2 / FM301 through it.
- `recast-radar-io-level3`: every Level III product through the router,
  with typed storm attribute tables, radar coded messages, generic
  components, 1993-2001 products, text and status messages, and allocation
  budgets.
- `recast-radar-retrieve`: Vulpiani and Maesaka KDP, Z-PHI attenuation
  correction. `recast-radar-map`: Cartesian gridding of several volumes.
- `recast-radar-data`: `polling`, a reader for GR2Analyst polling
  directories (`dir.list`, `config.cfg`, `grlevel2.cfg`) that uses only
  listed names that are plain file names; `DwdProvider::filtered_reflectivity`,
  `OrdProvider::complete_cycles` and `OrdProvider::velocity_scan_only`;
  six ORD site-table entries; the `level3` module.
- `recast-radar-cli`: the `recast-radar` command (info, dump, render,
  validate, bench, fetch, convert, publish, serve). `recast-radar-py`: the
  Python package `recast_radar` (FM301 DataTrees, Py-ART radars, writers,
  fetchers), built with maturin and not published.
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

### Changed

- Level II decoding parses gzip while inflating, parses uncompressed input
  in place, decodes whole-file bzip2 block by block, decodes gzip or bzip2
  copies of LDM files and LDM files cut short, and bounds its decoded-record
  buffer pool (`docs/perf/cross-library.md`). The renderer's fast azimuth
  bin keeps its exactness against the plain path, whose angle comes from the
  crate's own `atan2`. The bench pixel checksums are unchanged.

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
  products), CfRadial 1 (classic netCDF and netCDF-4) and CfRadial 2 /
  FM301, DORADE and mobile-radar archives, and JMA radar GRIB2 tar
  archives; a format-sniffing router.
- Writers of NEXRAD Level II (files, real-time chunks, polling
  directories), CfRadial 1, CfRadial 2 / FM301 and ODIM_H5.
- A pure-Rust bzip2 decoder and encoder (`recast-radar-bzip2`) and HDF5
  reader and writer (`recast-radar-hdf5`).
- The `recast-radar` command and the `recast_radar` Python package.
- Data access: NEXRAD Level II archive and real-time chunks on AWS, site
  catalogs, international and community feeds.
- Algorithms: velocity dealiasing, gate filters and smoothing, derived
  products, composites and cross sections, storm tracking, CPU rendering,
  scattering tables.
