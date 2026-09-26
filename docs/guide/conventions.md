# Conventions

What holds across every module: how functions are named, how errors and
enums behave, what the decoders refuse to allocate, where the code runs, and
what "not stable yet" means.

## Features

The facade's features are listed in the [README](../../README.md#features).
The short version: the defaults (`io`, `correct`, `filters`, `retrieve`,
`map`) read every format and run the algorithms; add `render` for images,
`net` for downloads, `track` for storm tracking, `serde` for serialization
of the data model, or `full` for everything. Only `net` makes network
requests or compiles C.

## Naming

- `read_*` functions of the io crates decode a file into the data model
  and return a `Volume`: `nexrad::read_volume_from_path`,
  `nexrad::read_volume_from_bytes`, `odim::read_odim_h5_volume`,
  `cfradial::read_cfradial1_volume`, `dorade::read_dorade_sweep_volume`,
  `level3::read_level3_volume`, `io::read_supported_volume_bytes`. Some
  return several volumes (`jma::read_jma_tar_volumes`,
  `io::read_mobile_archive_from_path`), a volume with the format's metadata
  beside it (`nexrad::read_volume_with_metadata`,
  `io::read_supported_volume_with_metadata`), or an `Option<Volume>`: the
  Level II previews (`nexrad::read_gzip_preview_from_bytes`,
  `nexrad::read_bzip_block_preview_from_bytes`) return `None` for input
  they do not preview or that holds no complete first sweep. Two `read_*`
  functions are not decoders and return no volume:
  `model::bounded_read::read_to_end_limited` reads a reader to the end
  under a size limit, like `std::io::Read::read_to_end`, and
  `cfradial::netcdf3::Nc3File::read_var` reads one netCDF variable's
  values.
- `decode_*` functions decode a format's own structures, which are not
  volumes: `level3::decode_product` and `decode_message` (Level III
  products and messages), `odim::decode_odim_h5_cartesian_max` (a Cartesian
  grid), and the Level II message parsers in `nexrad::messages`.
- `_from_path` and `_from_bytes` say what the input is; `_with_metadata`
  returns the format's metadata beside the volume.
- `looks_like_*_bytes` functions sniff a format from its first bytes without
  decoding it.
- Algorithm functions are named for what they return
  (`composite_reflectivity`, `echo_top`, `dealias_velocity`), take the model
  types by reference and return a new `Field`.

The decoders were called `decode_*` before the FM301 data model replaced the
old one (plan A.4). They were renamed to `read_*` when their return type
changed to `Volume`, so that every old call site became a "cannot find
function" error instead of a type error further along, and the prefix is
the same across every io crate. Design note
[fm301-model.md section 13.4](../design/fm301-model.md#134-acceptance-and-removal)
has the full rename table.

## Errors

Every crate that can fail has its own error enum (`NexradError`,
`OdimError`, `CfRadialError`, `DoradeError`, `JmaError`, `Level3Error`,
`IoError`, `DataSourceError`, `RenderError`, ...), built with `thiserror`, so
each implements `std::error::Error` and `Display` and works with `?` and
`Box<dyn Error>`. The router's `IoError` wraps the decoder's error and
displays exactly its message. Library code never panics on bad input: it
has no `unwrap` or `expect` (clippy denies both), and malformed files are
errors.

## Enums are `#[non_exhaustive]`

Public enums and error types are marked `#[non_exhaustive]`, so a new
variant (a new format, error case, product or field name) is not a breaking
change. Outside the defining crate, a `match` on one needs a wildcard arm:

```rust,ignore
match error {
    IoError::Nexrad(error) => eprintln!("Level II: {error}"),
    other => eprintln!("{other}"),
}
```

A few enums are closed sets whose exhaustive matching is the point, and are
not marked; each says so in its documentation:

- the storage and numeric types of the data model and their packing:
  `model::FieldData`, `model::RowRef`, `model::Coding`,
  `model::LinearTransform`, `model::FloatWidth`, `model::Scalar`,
  `model::ArrayBuf` and `model::fm301::ArrayRef`. Code that reads raw
  storage (a writer, a binding, a renderer) must handle every type and
  transform, so a new one is a breaking change that each such `match` has
  to see;
- `data::sites::SiteKind`, so that a feature written for US sites cannot
  quietly ignore international ones.

The one public error struct, `data::realtime::iterator::TransportError`, is
`#[non_exhaustive]` as well; build it with `TransportError::new`.

## Resource limits

Radar files come from the network, so every decoder bounds what a file can
make it allocate: decompressed sizes, rays, gates per ray, sweeps, members
of an archive and total decoded bytes. The shared limits are in
`model::bounded_read` (for example 512 MiB of expanded input), and each
decoder crate's documentation has a `# Limits` section with its own. A file
over a limit is an error, never an abort. The fuzz targets in `fuzz/`
exercise these paths.

## Threads

Decoders and algorithms use rayon for data parallelism (bzip2 records, rows
of a sweep, gates of a composite) on rayon's global pool. Set
`RAYON_NUM_THREADS`, or build a pool with `rayon::ThreadPoolBuilder` and
call the library inside its `install`, to limit it.

A pool of one thread does not make the work run on the calling thread: a
parallel loop called from outside the pool runs on the pool's one thread
while the caller waits. Two cases run on the calling thread: the Level II
bzip2 block decoder, which starts no worker when the pool has one thread
and decompresses every block itself as it parses, and `wasm32`, where rayon
runs everything on the calling thread.

## WebAssembly

Every crate builds for `wasm32-unknown-unknown` except `recast-radar-data`
with its `net` feature (it builds without it) and the test-only
`recast-radar-testdata`, which is never a normal dependency of a library
crate; the facade builds with any feature but `net` and `full`. On wasm32,
use the byte entry points (`read_supported_volume_bytes`,
`read_volume_from_bytes`); path entry points return an I/O error. rayon
runs on the calling thread. CI checks the build with
`tools/ci/wasm-check.sh`. Details: [docs/design/wasm.md](../design/wasm.md).

## No unsafe code

Every crate forbids `unsafe` (`unsafe_code = "forbid"` for the whole
workspace, with no exceptions). The guarantee covers this workspace's code,
not its dependencies: many of them use unsafe code internally. Examples are
rayon and the crossbeam crates, memchr, hashbrown, bytemuck and zerocopy,
flate2 with zlib-rs, crc32fast, zip, chrono, and image with its codecs, and
with `net`, tokio, reqwest, rustls and ring. `cargo tree -p recast-radar-tools
--features full` lists the whole graph.

## Minimum Rust version and stability

The minimum supported Rust version is 1.94 (edition 2024); CI builds and
lints every target with it. The crates are at version 0.1.0 and not
published: the API can change between commits, and
[CHANGELOG.md](../../CHANGELOG.md) records the changes that affect callers.
