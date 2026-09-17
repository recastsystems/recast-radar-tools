# WASM (wave 2, stream G.3)

Goal (plan `docs/superpowers/plans/2026-09-16-wave2.md` G.3; spec section 9,
criterion 7): every non-`net` library crate passes
`cargo check --target wasm32-unknown-unknown`.

## Check

```sh
rustup target add wasm32-unknown-unknown
cargo install cargo-hack
bash tools/ci/wasm-check.sh
```

CI runs this script, and the script is the definition of the check. Its header
comment gives the same rules as this section. It runs:

1. `cargo hack check --locked --target wasm32-unknown-unknown --workspace`,
   with the exclusions listed below. cargo-hack checks each crate on its own
   with its default features, so a crate cannot pass only because another
   crate in the same command turned on a feature of a shared dependency. New
   crates (for example Level III) are included without changing the command.
2. `cargo hack check --locked --target wasm32-unknown-unknown -p
   recast-radar-tools --each-feature --exclude-features net,full`: the facade
   with no features, with the defaults, and with each feature alone.
3. `cargo check --locked --target wasm32-unknown-unknown -p recast-radar-data
   --no-default-features`, once `recast-radar-data` has a `net` feature
   (stream E.1). Until then the script prints a note and skips this step.

Plain `cargo check` builds only lib and bin targets, so dev-dependencies (such
as `recast-radar-io`'s dev-dependency on `recast-radar-data`) are not built.

Not checked:

- `recast-radar-data` with default features: its networking uses reqwest's
  blocking client, which does not exist on wasm32 (30 compile errors at
  `ba35387`).
- The facade's `net` and `full` features, because both enable `recast-radar-data`.
- `recast-radar-bench`: the benchmark harness binary. It is not a library, so
  wasm32 is not a goal for it. It did compile for wasm32 at `ba35387` (see
  Result).
- `recast-radar-testdata`: the test-only corpus crate. It downloads over HTTPS
  through ureq and rustls, and no library crate has it as a normal dependency.

## Result (branch `packaging` at `ba35387`)

This run used an earlier form of the check: one
`cargo check --workspace --exclude recast-radar-data`, which also covered
`recast-radar-bench`. Everything passed without any source or manifest change
and with zero warnings:
`core`, `io-nexrad`, `io-odim`, `io-cfradial`, `io-dorade`, `io-jma`, `io`,
`correct`, `filters`, `retrieve`, `map`, `track`, `render`, `scattering`,
`bench`, and the facade with `--no-default-features` and with each non-`net`
feature on its own (16 cargo-hack runs). After `testdata` merged,
`tools/ci/wasm-check.sh` passed as well (14 crates and 16 facade runs). The normal dependency graph is pure
Rust (bzip2 through libbz2-rs-sys, flate2 through zlib-rs, zip with deflate,
image with png/jpeg/tiff, sha2, chrono, serde, thiserror, rayon), so no crate
needs a C toolchain for wasm32.

## rayon: no `parallel` feature

The plan puts rayon behind a default-on `parallel` feature only where wasm32
needs it. No crate needs it, so no feature was added. rayon stays a plain
dependency of `correct`, `filters`, `io-dorade`, `io-nexrad`, `map`, `render`
and `retrieve`:

- rayon 1.12 and rayon-core 1.13 compile for wasm32-unknown-unknown.
- On that target `std::thread::spawn` returns `Unsupported`. rayon-core then
  builds the global pool as a single thread that runs on the calling thread
  (`default_global_registry` in `rayon-core/src/registry.rs`), so `par_*` calls
  run one after another. The Level II pipelined bzip decode
  (`rayon::in_place_scope` with `BlockSlots::wait_block`) still makes
  progress, because the parsing thread claims and decompresses blocks itself
  when no worker is free.
- Output does not depend on the rayon thread count. With `RAYON_NUM_THREADS=1`
  the bench prints the baseline checksums.

A `parallel` feature would only remove rayon from wasm builds, which makes
them smaller. It would touch every rayon call site, including the Level II
pipeline that stream D.1 is rewriting. Revisit it if wasm binary size matters.

Runtime notes for wasm32-unknown-unknown users: the path-based APIs (`std::fs`)
return I/O errors there, so use the byte-slice entry points. `Instant::now` and
`Utc::now` would panic on this target, but they appear only in test code.

## Runtime smoke (one-off, not in CI)

To confirm that the crates also run on wasm32, a throwaway cdylib outside the
repository reproduced `recast-radar-bench`'s checksum pipeline without timers:
`recast_radar_io::decode_supported_volume_bytes`, the reflectivity raster and
the dealiased velocity raster, each at three viewports. It was built with
`--release --target wasm32-unknown-unknown` and run under Node 22 through a
bare `WebAssembly.instantiate` with no imports and no JS glue, on the three
corpus volumes. A native build of the same code printed the baseline checksums.

| File | Cuts | Native (baseline) | wasm32 |
|---|---|---|---|
| KTLX20240315_000217_V06 | 20 | `0xc04a5e2dfecc4c1f` | `0x873d9370fecc4c1f` |
| KILX20260418_013553_V06 | 23 | `0xd5080047ae5dfeb5` | `0x4a51bbfdae5dfeb5` |
| KTLX20130520_201643_V06.gz | 17 | `0x19e3735f42cdca4b` | `0x62e7ca24c87c43ce` |

All three volumes decode, dealias and render on wasm32 without a panic. The
wasm32 rasters differ from native in 18 pixels across the 18 buffers (at most
3 per buffer, each buffer 0.9 to 3.7 megapixels). Every differing pixel was
replayed through the renderer's viewport math on both targets. The screen
offsets (`east`, `north`), the squared range (`mul_add`) and the rotation
`sin_cos` match bit for bit. `f32::atan2` in `azimuth_from_xy`
(`recast-radar-render`) differs by exactly one ULP in all 18 cases, for example
`0x3fa7a9cf` native against `0x3fa7a9ce` on wasm32. Each of these azimuths
lies on the rounding edge of the renderer's 0.1-degree azimuth lookup bins
(for example 75.050003 native against 75.049995 on wasm32), so the one-ULP
difference selects the neighbouring bin.

The cause is the math library. On native Windows, `f32::atan2` calls the MSVC
UCRT; on wasm32 it calls Rust's `libm` port. Pixel checksums therefore depend on
the platform math library. The baselines in
`docs/baselines/import-checksums.txt` hold for x86_64-pc-windows-msvc. Other
targets are not expected to match them bit for bit: wasm32 does not, and Linux
glibc has not been checked.
