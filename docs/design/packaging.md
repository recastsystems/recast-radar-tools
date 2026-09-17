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
| `level3` | `level3` | `recast-radar-io-level3` | |
| `odim` | `odim` | `recast-radar-io-odim` | |
| `cfradial` | `cfradial` | `recast-radar-io-cfradial` | |
| `dorade` | `dorade` | `recast-radar-io-dorade` | |
| `jma` | `jma` | `recast-radar-io-jma` | |
| `io` | `io` | `recast-radar-io` | all six format features |
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
the facade. (`net` used to be an exception that did not enable `jma`, while
`recast-radar-data` used `recast-radar-io-jma` internally for station headers;
stream E.1 removed that dependency.) Apart from `io`, features never add
compile cost beyond the crate itself, since the implied crates are its
dependencies anyway. `io` is the one exception, in the opposite direction: it
also enables `level3`, whose crate `recast-radar-io` does not depend on,
because spec 4.1 defines `io` as all formats and the router does not read
Level III products (they are not radar volumes). `tests/readme.rs` checks the
rule against the member manifests. The exceptions are its `NOT_IMPLIED` (now
empty) and `ALSO_IMPLIED` lists, and the test fails once an exception no
longer applies.

Module names are the crate names without the `recast-radar-` and `io-`
prefixes; `recast-radar-data` is `data` (its feature is `net`, per the spec).
No root-level item re-exports: stream F renames the model types, and module
paths stay stable across that.

`level3` was added when the `level3` branch merged into `main` (1da8d33).
The VAD Wind Profile decoder that lived in `recast-radar-io-nexrad` has since
moved into `recast-radar-io-level3` (`vwp`); the facade did not change.

Deferred until other streams land:
- `net`: when E.1 adds the `net` feature to `recast-radar-data`, the facade
  feature becomes `["dep:recast-radar-data", "recast-radar-data/net"]`; the
  dependency keeps default features until then so `net` keeps networking.
- `serde`: the model's derives are unconditional today. `recast-radar-core`
  declares an empty `serde` feature so the facade wiring is final; making the
  derives conditional (`serde = ["dep:serde"]` plus `cfg_attr`) belongs to the
  model owner (stream F) or wave 3. Until then the facade feature does
  nothing, and serde is in every build (`recast-radar-data` and
  `recast-radar-scattering` also use it directly). The README says so.

Test: `tests/facade_real_files.rs` (requires `io`, `correct`, `filters`,
`map`) reads testdata `odim-espdg-20260707-1927-pvol-dbzh-vradh` through
`recast-radar-testdata` (a dev-dependency) and decodes the ODIM_H5 PVOL
through `io`. It then checks invariants of `filters` smoothing (coverage and
value range preserved), `correct` dealiasing (differences are whole multiples
of twice the Nyquist velocity) and `map` composites (column max at least the
lowest-sweep max), all through facade paths. `cargo hack check -p recast-radar-tools --each-feature
--no-dev-deps` covers each feature alone.

## Crate metadata (G.1)

Every crate sets `description`, `keywords` (at most five), `categories`
(crates.io slugs), `readme` and `rust-version.workspace = true`. `readme` is
inherited from `[workspace.package]` (the root `README.md`, which G.4 expands)
except for `recast-radar-bench` and `recast-radar-scattering`, which keep their
own READMEs. `repository` is omitted until a remote exists.
`recast-radar-bench` (a harness binary) and `recast-radar-testdata` (test-only)
are `publish = false`. `tests/readme.rs` fails when a crate under `crates/`
lacks this metadata.

The facade's package excludes `tests/`. Those tests read the repository
(README, manifests, `ci.yml`, testdata), not the packaged crate. When
packaging, cargo drops the `[[test]]` target whose file is excluded.

Not in G.1: internal path dependencies have no `version`, so `cargo publish`
of the library crates is not yet possible; that change touches every
dependency line and waits until the other wave 2 streams have merged.

## CI (G.2)

`.github/workflows/ci.yml` has five jobs on ubuntu-latest. Every job except
rustfmt fails if `Cargo.lock` is out of date (`--locked`).

- `rustfmt` (Rust 1.94.0): `cargo fmt --all --check`.
- `clippy` (Rust 1.94.0): `cargo clippy --workspace --all-targets
  --all-features --locked`, without `-D warnings` until D.3 lands (the comment
  in the workflow gives the flags to add then). `--all-features` also builds
  the facade examples that need `render`.
- `test` (stable): `cargo test --workspace --locked --no-fail-fast`, with a
  cache of downloaded test files at `$RECAST_RADAR_TESTDATA`. The cache key
  starts with the hash of `testdata/**/manifest.toml`, and a new entry is
  saved only when a run downloaded new files.
- `facade features, pure Rust` (stable): `cargo fetch --locked`, `cargo hack
  check -p recast-radar-tools --each-feature --no-dev-deps`, then
  `tools/ci/pure-rust-check.sh`. `--no-dev-deps` removes dev-dependencies from
  the manifests while cargo-hack runs, which prunes `Cargo.lock`, so that
  command cannot take `--locked`; `cargo fetch --locked` checks the lock file
  before it. The script fails if `cc` or `cmake` is in the
  facade's dependency graph with every feature except `net` and `full`, for
  all targets.
- `wasm32` (stable): `tools/ci/wasm-check.sh` (see below).

Toolchains. rustfmt and clippy are pinned to Rust 1.94.0, the workspace
`rust-version`. New Rust releases add clippy lints (1.98 warns
`clippy::chunks_exact_to_as_chunks` at 23 sites). With `stable`, the clippy
result would change without a commit, and the `-D warnings` planned for D.3
would fail on the next release. The pin also makes the clippy job the MSRV
check, because it builds every target with `--all-features` on 1.94.0. Tests,
features and wasm32 run on the latest stable. `tests/readme.rs` fails if the
pins, `[workspace.package] rust-version` and the README's "Minimum Rust
version" disagree.

Platform. CI runs the tests on Linux; they were written on Windows. Two
`recast-radar-scattering` tests froze f64 bits on x86_64-pc-windows-msvc, and
glibc's `pow`, `exp`, `ln` and `cbrt` round the last bits differently (1 to 70
ULPs). Those tests now compare exactly only on that target, and within a
relative 1e-12 elsewhere. The bench pixel checksums in
`docs/baselines/import-checksums.txt` are also Windows values (see
[wasm.md](wasm.md)), and CI does not check them.

## WASM (G.3)

See [wasm.md](wasm.md). Every non-`net` crate already passed
`cargo check --target wasm32-unknown-unknown` without source changes, so no
`parallel` feature was added: rayon falls back to the calling thread on that
target. `tools/ci/wasm-check.sh` checks each workspace crate on its own, and
the facade with each feature except `net` and `full`. It leaves out
`recast-radar-data` (to be checked with `--no-default-features` once E.1 gives
it a `net` feature), the `recast-radar-bench` harness binary and the test-only
`recast-radar-testdata`.

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
  (`crate-map` markers) must list exactly the crates under `crates/`, and its
  Module column must match the re-exports in `src/lib.rs`. The feature table
  (`features` markers) must match `[features]` (crate, implied features) and
  the modules in `src/lib.rs`. Its Default column says "yes" for features
  listed in `default` and "via `io`" for features that a default feature
  turns on. The no-unsafe list (`lint-exceptions` markers) must name exactly
  the crates without `[lints] workspace = true`. The test also checks the
  crate metadata, the feature dependency rule and the minimum Rust version
  (see above).
- Not checked: other prose, the example output excerpts (copied from a run on
  `l2-ktlx-20240315-000217`) and code blocks that are not `rust`.
- When a stream changes one of these (for example D.1 opts the last two
  crates into the workspace lints, or Level III adds a crate and a feature),
  the test fails with the mismatch, and the README is updated in the same
  merge.
