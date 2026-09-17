# Packaging and CI (wave 2, stream G)

Scope: the `recast-radar-tools` facade crate, crate metadata, CI, WASM checks,
and the root README (plan `docs/superpowers/plans/2026-09-16-wave2.md`,
Stream G; spec section 4.1).

## Facade (G.1)

`crates/recast-radar-tools` depends on every library crate. `recast-radar-core`
is always on and re-exported as `core`; every other crate is an optional
dependency re-exported as a module behind one feature:

| Feature | Module | Crate | Also enables |
|---|---|---|---|
| (always) | `core` | `recast-radar-core` | |
| `nexrad` | `nexrad` | `recast-radar-io-nexrad` | |
| `odim` | `odim` | `recast-radar-io-odim` | |
| `cfradial` | `cfradial` | `recast-radar-io-cfradial` | |
| `dorade` | `dorade` | `recast-radar-io-dorade` | |
| `jma` | `jma` | `recast-radar-io-jma` | |
| `io` | `io` | `recast-radar-io` | all five format features |
| `net` | `data` | `recast-radar-data` | |
| `correct` | `correct` | `recast-radar-correct` | |
| `filters` | `filters` | `recast-radar-filters` | |
| `retrieve` | `retrieve` | `recast-radar-retrieve` | `correct` |
| `map` | `map` | `recast-radar-map` | `correct`, `filters` |
| `track` | `track` | `recast-radar-track` | `correct`, `map`, `retrieve` |
| `render` | `render` | `recast-radar-render` | `correct` |
| `scattering` | `scattering` | `recast-radar-scattering` | |
| `serde` | | forwards to `recast-radar-core/serde` | |
| `full` | | | every feature above |

Default: `io`, `correct`, `filters`, `retrieve`, `map` (spec 4.1).

Rule: a feature enables the facade features of the member crates its crate
normally depends on, so any type a module's API mentions can be named through
the facade. The one exception is `net`, which does not enable `jma`:
`recast-radar-data` uses `recast-radar-io-jma` only internally (station
headers) and stream E.1 removes that dependency. Features never add compile
cost beyond the crate itself, since the implied crates are its dependencies
anyway.

Module names are the crate names without the `recast-radar-` and `io-`
prefixes; `recast-radar-data` is `data` (its feature is `net`, per the spec).
No root-level item re-exports: stream F renames the model types, and module
paths stay stable across that.

Deferred until other streams land:
- `level3`: added when the `level3` branch merges (`dep:recast-radar-io-level3`,
  and `io` enables it).
- `net`: when E.1 adds the `net` feature to `recast-radar-data`, the facade
  feature becomes `["dep:recast-radar-data", "recast-radar-data/net"]`; the
  dependency keeps default features until then so `net` keeps networking.
- `serde`: the model's derives are unconditional today. `recast-radar-core`
  declares an empty `serde` feature so the facade wiring is final; making the
  derives conditional (`serde = ["dep:serde"]` plus `cfg_attr`) belongs to the
  model owner (stream F) or wave 3.

Test: `tests/facade_real_files.rs` (requires `io`, `correct`, `filters`,
`map`) decodes a real ODIM_H5 PVOL through `io`, then checks invariants of
`filters` smoothing (coverage and value range preserved), `correct`
dealiasing (differences are whole multiples of twice the Nyquist velocity)
and `map` composites (column max at least the lowest-sweep max), all through
facade paths. `cargo hack check -p recast-radar-tools --each-feature
--no-dev-deps` covers each feature alone.

## Crate metadata (G.1)

Every crate sets `description`, `keywords` (at most five), `categories`
(crates.io slugs), `readme` and `rust-version.workspace = true`. `readme` is
inherited from `[workspace.package]` (the root `README.md`, which G.4 expands)
except for `recast-radar-bench` and `recast-radar-scattering`, which keep their
own READMEs. `repository` is omitted until a remote exists.
`recast-radar-bench` is `publish = false` (a harness binary).

Not in G.1: internal path dependencies have no `version`, so `cargo publish`
of the library crates is not yet possible; that change touches every
dependency line and waits until the other wave 2 streams have merged.

## CI (G.2)

`.github/workflows/ci.yml` has five jobs on ubuntu-latest with stable Rust:

- `rustfmt`: `cargo fmt --all --check`.
- `clippy`: `cargo clippy --workspace --all-targets --all-features --locked`,
  without `-D warnings` until D.3 lands (the comment in the workflow gives the
  flags to add then). `--all-features` also builds the facade examples that
  need `render`.
- `test`: `cargo test --workspace --locked --no-fail-fast`, with a cache of
  downloaded test files at `$RECAST_RADAR_TESTDATA`. The cache key starts with
  the hash of `testdata/**/manifest.toml`, and a new entry is saved only when
  a run downloaded new files. These steps are skipped until the testdata
  manifests are in the tree.
- `facade features`: `cargo hack check -p recast-radar-tools --each-feature
  --no-dev-deps`.
- `wasm32`: `tools/ci/wasm-check.sh` (see below).

## WASM (G.3)

See [wasm.md](wasm.md). Every non-`net` crate already passed
`cargo check --target wasm32-unknown-unknown` without source changes, so no
`parallel` feature was added: rayon falls back to the calling thread on that
target. `tools/ci/wasm-check.sh` checks each workspace crate on its own
(except `recast-radar-data`, which is checked with `--no-default-features` once
E.1 gives it a `net` feature, and the dev-only `recast-radar-testdata`), and
the facade with each feature except `net` and `full`.

## README (G.4)

The root `README.md` has the crate map, the feature table, three examples, and
the pure-Rust, WebAssembly and no-unsafe statements.

- The examples are `crates/recast-radar-tools/examples/{decode_level2,
  dealias_velocity,render_png}.rs`, shown in the README verbatim, with
  `required-features` in the facade manifest. They take a Level II path on the
  command line. They were run with `--release` on the three corpus volumes
  (`KTLX20240315_000217_V06`, `KILX20260418_013553_V06`,
  `KTLX20130520_201643_V06.gz`); the README's output excerpts come from the
  first.
- `crates/recast-radar-tools/tests/readme.rs` keeps the README honest. Every
  `rust` code block must be preceded by `<!-- example: <path> -->` and equal
  that file, and every facade example must appear. The crate map
  (`crate-map` markers) must list exactly the crates under `crates/`. The
  feature table (`features` markers) must match `[features]` (crate, implied
  features, default set) and the modules in `src/lib.rs`. The no-unsafe list
  (`lint-exceptions` markers) must name exactly the crates without
  `[lints] workspace = true`.
- When a stream changes one of these (for example D.1 opts the last two
  crates into the workspace lints, or Level III adds a crate and a feature),
  the test fails with the mismatch, and the README is updated in the same
  merge.
