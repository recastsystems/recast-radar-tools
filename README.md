# recast-radar-tools

Pure-Rust weather radar libraries. They decode NEXRAD Level II, NEXRAD and
TDWR Level III, ODIM_H5, CfRadial 1, DORADE and JMA radar GRIB2 files. They download NEXRAD Level II
volumes and real-time chunks from AWS, and data from other public feeds. They
also dealias Doppler velocity, filter gates, compute derived products, build
composites and cross sections, track storm cells and render sweeps to PNG.
These are libraries, not an application: there is no GUI.

Status: version 0.1.0, not published to crates.io, and the API is not stable.
The data model is moving to WMO FM301 (CfRadial 2) conventions, so type names
in `core` will change. Design:
[docs/superpowers/specs/2026-09-16-recast-radar-tools-design.md](docs/superpowers/specs/2026-09-16-recast-radar-tools-design.md).

## Using it

`recast-radar-tools` re-exports the other crates as modules, each behind a
Cargo feature (see [Features](#features)). Until the crates are published,
depend on it by path:

```toml
[dependencies]
recast-radar-tools = { path = "../recast-radar-tools/crates/recast-radar-tools", features = ["render"] }
```

To take only some modules, turn the defaults off, for example
`default-features = false, features = ["nexrad", "correct"]`. Each member crate
can also be used on its own.

Minimum Rust version: 1.94 (edition 2024).

## Examples

The three programs below are the files in
[`crates/recast-radar-tools/examples/`](crates/recast-radar-tools/examples/),
shown in full. CI compiles them, and `crates/recast-radar-tools/tests/readme.rs`
fails if a Rust code block here differs from its file. Each program takes a
Level II file as its first argument. The output shown comes from
[`KTLX20240315_000217_V06`](https://unidata-nexrad-level2.s3.amazonaws.com/2024/03/15/KTLX/KTLX20240315_000217_V06)
(10.8 MB, testdata id `l2-ktlx-20240315-000217`), from the public
`unidata-nexrad-level2` bucket on AWS. The output excerpts are copied from a run
of the examples; no test checks them.

### Decode a Level II file

<!-- example: crates/recast-radar-tools/examples/decode_level2.rs -->
```rust
// Decode a NEXRAD Level II file and list its sweeps.
//
// cargo run --release -p recast-radar-tools --example decode_level2 -- <level2-file>

use std::error::Error;
use std::path::PathBuf;

use recast_radar_tools::nexrad;

fn main() -> Result<(), Box<dyn Error>> {
    let path = PathBuf::from(std::env::args_os().nth(1).ok_or("usage: <level2-file>")?);

    // Uncompressed, gzip, bzip2 and LDM block-bzip2 archives all decode here.
    let volume = nexrad::read_volume_from_path(&path)?;

    println!(
        "{} at {}",
        volume.attrs.instrument_name, volume.time_reference
    );
    if let Some(vcp) = volume.scan.vcp_pattern {
        println!("VCP {vcp}");
    }
    for (index, sweep) in volume.sweeps.iter().enumerate() {
        let fields: Vec<&str> = sweep.fields.iter().map(|f| f.name.as_str()).collect();
        println!(
            "sweep {index:>2}: {:>5.2} deg, {} rays, {}",
            sweep.fixed_angle_deg,
            sweep.nrays(),
            fields.join(" ")
        );
    }
    Ok(())
}
```

Output (first five lines):

```text
KTLX at 2024-03-15 00:02:17 UTC
VCP 212
sweep  0:  0.48 deg, 720 rays, DBZH ZDR PHIDP RHOHV CCORH
sweep  1:  0.48 deg, 720 rays, DBZH VRADH WRADH
sweep  2:  0.88 deg, 720 rays, DBZH ZDR PHIDP RHOHV CCORH
```

For bytes of unknown format, `io::read_supported_volume_bytes(&bytes)`
(feature `io`) sniffs the format and calls the matching decoder: DORADE,
ODIM_H5, CfRadial 1, JMA GRIB2 tar, or Level II. It also unwraps gzip and
single-file ZIP archives. Level III products are not radar volumes and the
router does not read them: use `level3::decode_product(&bytes)` (feature
`level3`, part of `io`).

### Dealias velocity

<!-- example: crates/recast-radar-tools/examples/dealias_velocity.rs -->
```rust
// Dealias (unfold) the Doppler velocity of every sweep in a Level II file.
//
// cargo run --release -p recast-radar-tools --example dealias_velocity -- <level2-file>

use std::error::Error;
use std::path::PathBuf;

use recast_radar_tools::core::Quantity;
use recast_radar_tools::{correct, nexrad};

fn main() -> Result<(), Box<dyn Error>> {
    let path = PathBuf::from(std::env::args_os().nth(1).ok_or("usage: <level2-file>")?);
    let mut volume = nexrad::read_volume_from_path(&path)?;

    for sweep in &mut volume.sweeps {
        let Some(raw) = sweep.find(Quantity::RadialVelocity) else {
            continue;
        };
        // Region-based unfolding. The result (VRADDH) has the same rays and
        // gates as the source field.
        let dealiased = correct::dealias_velocity(sweep, raw);

        let mut unfolded = 0;
        let (rows, gates) = raw.shape();
        for row in 0..rows {
            for gate in 0..gates {
                let before = raw.value(row, gate).unwrap_or(f32::NAN);
                let after = dealiased.value(row, gate).unwrap_or(f32::NAN);
                if (after - before).abs() > 1.0 {
                    unfolded += 1;
                }
            }
        }
        let nyquist = sweep
            .ray_vars
            .nyquist_velocity_mps
            .as_ref()
            .and_then(|values| values.first().copied());
        println!(
            "{:>5.2} deg: Nyquist {:.1} m/s, {unfolded} gates unfolded",
            sweep.fixed_angle_deg,
            nyquist.unwrap_or(f32::NAN)
        );

        // Keep the dealiased field beside the raw velocity.
        sweep.add_field(dealiased)?;
    }
    Ok(())
}
```

Output (first three lines):

```text
 0.48 deg: Nyquist 23.8 m/s, 1833 gates unfolded
 0.88 deg: Nyquist 23.8 m/s, 2459 gates unfolded
 0.48 deg: Nyquist 23.8 m/s, 1878 gates unfolded
```

`correct` has two more dealiasers: a model-anchored volume engine
(`dealias_volume_v4`) and a port of Py-ART's region-based dealiaser
(`dealias_velocity_pyart_region`).

### Render PNG images

This example needs the `render` feature.

<!-- example: crates/recast-radar-tools/examples/render_png.rs -->
```rust
// Render the first reflectivity sweep and the first velocity sweep (dealiased)
// of a Level II file to PNG.
//
// cargo run --release -p recast-radar-tools --features render \
//     --example render_png -- <level2-file> <out-dir>

use std::error::Error;
use std::path::PathBuf;

use recast_radar_tools::core::{FieldName, Quantity, Volume};
use recast_radar_tools::nexrad;
use recast_radar_tools::render::{self, RasterOptions};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let (Some(input), Some(out_dir)) = (args.next(), args.next()) else {
        return Err("usage: <level2-file> <out-dir>".into());
    };
    let mut volume = nexrad::read_volume_from_path(&input)?;
    let options = RasterOptions::default(); // 1024 x 1024

    let (index, name) = first_sweep_with(&volume, Quantity::Reflectivity)?;
    let path = out_dir.join("reflectivity.png");
    render::render_field_png(&volume, index, &name, &path, options)?;
    println!("wrote {}", path.display());

    let (index, name) = first_sweep_with(&volume, Quantity::RadialVelocity)?;
    // Region-based unfolding; the result joins the sweep as VRADDH.
    let dealiased = render::dealiased_velocity_field(&volume, index, &name)?;
    let sweep = &mut volume.sweeps[index];
    sweep.add_field(dealiased)?;
    sweep.seal()?;
    let path = out_dir.join("velocity.png");
    render::render_field_png(&volume, index, &FieldName::Vraddh, &path, options)?;
    println!("wrote {}", path.display());
    Ok(())
}

/// The first sweep with a field of `quantity`, and that field's name.
fn first_sweep_with(volume: &Volume, quantity: Quantity) -> Result<(usize, FieldName), String> {
    volume
        .sweeps
        .iter()
        .enumerate()
        .find_map(|(index, sweep)| {
            sweep
                .find(quantity)
                .map(|field| (index, field.name.clone()))
        })
        .ok_or_else(|| format!("no sweep has a {quantity:?} field"))
}
```

Each image is 1024 by 1024 RGBA with a transparent background. The radar is at
the centre, and the far end of the sweep's last gate lies 94% of the way from
the centre to the edge.

## Crates

<!-- crate-map:start -->
| Crate | Module | Contents |
|---|---|---|
| `recast-radar-tools` | | The facade: re-exports the crates below as modules behind features |
| `recast-radar-core` | `core` | FM301 data model: volumes, sweeps, ray coordinates, fields with CF packing, the FM301 group view, beam geometry, field names |
| `recast-radar-io-nexrad` | `nexrad` | NEXRAD Archive II (Level II), Message 31 and legacy Message 1, uncompressed, gzip, bzip2 or LDM block-bzip2; the Level III VAD Wind Profile product |
| `recast-radar-bzip2` | | bzip2 decompressor without unsafe code or dependencies, written for LDM records: about a quarter of the instructions of C libbzip2 |
| `recast-radar-io-level3` | `level3` | NEXRAD and TDWR Level III products: NOAAPort/WMO framing, message and product description headers, symbology, graphic and tabular blocks, display packets, data levels |
| `recast-radar-io-odim` | `odim` | ODIM_H5 polar volumes and Cartesian products, through an HDF5 reader written in Rust |
| `recast-radar-io-cfradial` | `cfradial` | CfRadial 1, through a classic netCDF (CDF-1, CDF-2) reader written in Rust |
| `recast-radar-io-dorade` | `dorade` | DORADE sweepfiles and mobile-radar (DOW, COW, RaXPol) archives |
| `recast-radar-io-jma` | `jma` | Japan Meteorological Agency polar-coordinate radar GRIB2 tar archives |
| `recast-radar-io` | `io` | Format sniffing: routes a byte buffer to the matching decoder |
| `recast-radar-data` | `data` | NEXRAD Level II archive and real-time chunks on AWS, site catalogs, international and community feeds |
| `recast-radar-correct` | `correct` | Doppler velocity dealiasing |
| `recast-radar-filters` | `filters` | Gate filters, polar smoothing, display interpolation |
| `recast-radar-retrieve` | `retrieve` | Sweep and column products, CAPPI, azimuthal shear and rotation, GBVTD, VAD wind profile |
| `recast-radar-map` | `map` | Composites, echo tops, VIL, hail, cross sections, RHI panels, volume resampling |
| `recast-radar-track` | `track` | Storm cell identification and tracking, rotation tracks, swaths, temporal grids |
| `recast-radar-render` | `render` | CPU rendering to RGBA and PNG, color tables, GR `.pal` palettes |
| `recast-radar-scattering` | `scattering` | Radar-scattering primitives and offline lookup tables |
| `recast-radar-bench` | | Decode and render benchmark with output checksums (binary, not published) |
| `recast-radar-testdata` | | Real test files: manifests, committed fixtures, a SHA-256-verified download cache and the Level II trim tool (for tests only, not published) |
<!-- crate-map:end -->

Not in the workspace yet: `fuzz/` (cargo-fuzz targets).

`crates/recast-radar-tools/tests/readme.rs` checks that the map lists exactly
the crates under `crates/` and that the Module column matches the facade's
re-exports. It also checks that every crate's manifest has a description,
keywords, categories and a readme.

## Features

<!-- features:start -->
| Feature | Module | Crate | Also enables | Default |
|---|---|---|---|---|
| (always on) | `core` | `recast-radar-core` | | yes |
| `nexrad` | `nexrad` | `recast-radar-io-nexrad` | | via `io` |
| `level3` | `level3` | `recast-radar-io-level3` | | via `io` |
| `odim` | `odim` | `recast-radar-io-odim` | | via `io` |
| `cfradial` | `cfradial` | `recast-radar-io-cfradial` | | via `io` |
| `dorade` | `dorade` | `recast-radar-io-dorade` | | via `io` |
| `jma` | `jma` | `recast-radar-io-jma` | | via `io` |
| `io` | `io` | `recast-radar-io` | `nexrad` `level3` `odim` `cfradial` `dorade` `jma` | yes |
| `net` | `data` | `recast-radar-data` | | |
| `correct` | `correct` | `recast-radar-correct` | | yes |
| `filters` | `filters` | `recast-radar-filters` | | yes |
| `retrieve` | `retrieve` | `recast-radar-retrieve` | `correct` | yes |
| `map` | `map` | `recast-radar-map` | `correct` `filters` | yes |
| `track` | `track` | `recast-radar-track` | `correct` `map` `retrieve` | |
| `render` | `render` | `recast-radar-render` | `correct` | |
| `scattering` | `scattering` | `recast-radar-scattering` | | |
| `serde` | | | | |
| `full` | | | `io` `net` `correct` `filters` `retrieve` `map` `track` `render` `scattering` `serde` | |
<!-- features:end -->

- In the Default column, "yes" means the feature is listed in `default`, and
  "via `io`" means `io` turns it on.
- A feature also enables the features of the member crates its crate depends
  on, so the types a module's API uses can be named through the facade. There
  is one exception: `io` enables `level3` although the router does not depend
  on it, so that `io` turns on every format decoder.
- `nexrad` alone gives the Level II decoder without the other formats or the
  router.
- `net` (also part of `full`) is the only feature that makes HTTPS requests
  and the only one that compiles C (see [Pure Rust](#pure-rust)).
- `serde` is a placeholder. It turns on `recast-radar-core/serde`, which does
  nothing yet. serde is always compiled, even with `default-features = false`:
  the data model derives `Serialize` and `Deserialize` unconditionally, and
  `recast-radar-data` and `recast-radar-scattering` use serde directly.
- `crates/recast-radar-tools/tests/readme.rs` checks this table against the
  facade's `[features]` and `src/lib.rs`. It also checks the dependency rule,
  and its two exceptions, against the member crates' manifests.

## Pure Rust

Without `net`, nothing in the build compiles C or C++, for any target:

- Level II bzip2 (LDM records and whole-file) through `recast-radar-bzip2`,
  this repository's decoder without unsafe code; Level III bzip2 through the
  `bzip2` crate's Rust backend (`libbz2-rs-sys` is a Rust port, despite its
  name); gzip and zlib through `flate2` with `zlib-rs`.
- HDF5 (for ODIM_H5) and classic netCDF (for CfRadial) are read by parsers in
  this repository, not by the C libraries.
- chrono is built with its `now` feature instead of `clock`. `clock` would add
  `iana-time-zone`, which compiles C++ when the target is Haiku.

The one exception is `net`: `recast-radar-data` uses reqwest with rustls, and
its crypto provider, `ring`, compiles C and assembly. The test-only
`recast-radar-testdata` also uses rustls (through ureq) to download test files.
CI runs [`tools/ci/pure-rust-check.sh`](tools/ci/pure-rust-check.sh). It fails
if `cc` or `cmake` is in the dependency graph, for any target, of the facade
with every feature except `net` and `full`.

The same split applies to WebAssembly. Every library crate except
`recast-radar-data` and the test-only `recast-radar-testdata` passes
`cargo check --target wasm32-unknown-unknown`, and so does the facade with any
single feature other than `net` and `full`. CI checks this with
[`tools/ci/wasm-check.sh`](tools/ci/wasm-check.sh), which also leaves out the
benchmark binary. On that target, use the byte-slice entry points (such
as `nexrad::read_volume_from_bytes`), because the path-based ones return I/O
errors. rayon runs everything on the calling thread there. Details:
[docs/design/wasm.md](docs/design/wasm.md).

## No unsafe

The workspace sets `unsafe_code = "forbid"`, and every crate opts in with
`[lints] workspace = true`. There are no exceptions:

<!-- lint-exceptions:start -->
<!-- lint-exceptions:end -->

Library code also denies `clippy::unwrap_used` and `clippy::expect_used`.
`crates/recast-radar-tools/tests/readme.rs` checks the exception list above
against the crate manifests.

## Tests and CI

`cargo test --workspace` runs the tests. New tests read real radar files only.
Some tests carried over from the original code base still build synthetic
input, and they will be converted to real files. The real files are listed in
`testdata/manifest.toml` and `testdata/*/manifest.toml`. Small ones are
committed under `testdata/files/`. The others are downloaded on first use into
a cache (`$RECAST_RADAR_TESTDATA`, by default in the user's cache directory)
and checked against their SHA-256.

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) runs on Ubuntu:

- rustfmt and clippy on Rust 1.94.0, the minimum supported version, so the
  clippy job also checks that every target builds with that version;
- the workspace tests on the latest stable Rust, keeping the testdata download
  cache between runs;
- a `cargo hack check` of each facade feature on its own, and
  [`tools/ci/pure-rust-check.sh`](tools/ci/pure-rust-check.sh);
- the wasm32 check in [`tools/ci/wasm-check.sh`](tools/ci/wasm-check.sh).

## License

Licensed under either of MIT ([LICENSE-MIT](LICENSE-MIT)) or Apache-2.0
([LICENSE-APACHE](LICENSE-APACHE)) at your option.
