# WASM (wave 2, stream G.3)

Goal (plan `docs/superpowers/plans/2026-09-16-wave2.md` G.3; spec section 9,
criterion 7): every non-`net` library crate passes
`cargo check --target wasm32-unknown-unknown`.

## Check

```sh
rustup target add wasm32-unknown-unknown
cargo check --workspace --exclude recast-radar-data --exclude recast-radar-testdata \
    --target wasm32-unknown-unknown
cargo hack check -p recast-radar-tools --each-feature --exclude-features net,full \
    --no-dev-deps --target wasm32-unknown-unknown
```

`--workspace` covers new crates (for example Level III) without changing the
command. Excluded:

- `recast-radar-data`: its networking uses reqwest's blocking client, which
  does not exist on wasm32 (30 compile errors today). Once E.1 (branch
  `data-access`) merges, `net` is a default feature and CI adds
  `cargo check -p recast-radar-data --no-default-features --target wasm32-unknown-unknown`.
- `recast-radar-testdata` (once `testdata` merges): a dev-only crate with HTTPS
  through ureq and rustls. Until it merges, cargo warns that this exclusion
  matches no package.
- The facade's `net` and `full` features, because both enable `recast-radar-data`.

`recast-radar-bench` is a harness binary. It compiles for wasm32, so it stays in
the command and needs no exclusion. Plain `cargo check` builds only lib and bin
targets, so dev-dependencies (such as `recast-radar-io`'s dev-dependency on
`recast-radar-data`) are not built.

## Result (branch `packaging` at `ba35387`)

Everything passes without any source or manifest change and with zero warnings:
`core`, `io-nexrad`, `io-odim`, `io-cfradial`, `io-dorade`, `io-jma`, `io`,
`correct`, `filters`, `retrieve`, `map`, `track`, `render`, `scattering`,
`bench`, and the facade with `--no-default-features` and with each non-`net`
feature on its own (16 cargo-hack runs). The normal dependency graph is pure
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
