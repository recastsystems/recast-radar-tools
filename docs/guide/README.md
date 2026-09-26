# recast-radar-tools user guide

This guide is for Rust developers who want to read, fetch, process or draw
weather radar data with recast-radar-tools. Each page explains one task and
shows a complete program from
[`crates/recast-radar-tools/examples/`](../../crates/recast-radar-tools/examples/).
The programs take their inputs on the command line; run one with

```sh
cargo run --release -p recast-radar-tools --features full --example <name> -- <args>
```

`--features full` builds every module; each example lists the features it
needs in the facade's `Cargo.toml`. The outputs shown come from runs on real
files named in each page; CI reruns them and compares
([`tools/check_example_outputs.py`](../../tools/check_example_outputs.py)),
except the live AWS listing.

1. [Reading radar files](reading.md): any format through the router, the
   format-specific decoders, Level III products, format metadata, limits.
2. [The data model](data-model.md): volumes, sweeps and fields; physical
   values and sentinels; gate positions, beam heights and ray times; the
   FM301 view; merging; serde.
3. [Fetching data](fetching.md): NEXRAD Level II from AWS, real-time chunks,
   site catalogs, international feeds.
4. [Processing](processing.md): velocity dealiasing, gate filters and
   smoothing, composites, echo tops and VIL, derived products, tracking.
5. [Rendering](rendering.md): PNG and RGBA rasters, color tables, viewports.
6. [Conventions](conventions.md): features, naming, errors and
   `#[non_exhaustive]`, resource limits, WebAssembly, the minimum Rust
   version and API stability.

## Adding the dependency

Until the crates are published, depend on the facade by path (or by git):

```toml
[dependencies]
recast-radar-tools = { path = "../recast-radar-tools/crates/recast-radar-tools", features = ["render"] }
```

The default features are `io`, `correct`, `filters`, `retrieve` and `map`.
`net` adds the downloaders, `render` the PNG renderer, `track` storm
tracking, `scattering` the scattering tables and `serde` serialization of the
data model; `full` turns everything on. Every module is also its own crate
(`recast-radar-io-nexrad`, `recast-radar-render`, ...), for callers who want
only one. The facade's modules are those crates: `recast_radar_tools::model`
is `recast_radar_core`, `recast_radar_tools::nexrad` is
`recast_radar_io_nexrad`, and so on.

The API documentation is in the crates: `cargo doc -p recast-radar-tools
--features full --open`.
