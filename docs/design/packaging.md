# Packaging and CI (wave 2, stream G)

Scope: the `recast-radar-tools` facade crate, crate metadata, CI, WASM checks,
and the root README (plan `docs/superpowers/plans/2026-09-16-wave2.md`,
Stream G; spec section 4.1).

## Facade (G.1)

`crates/recast-radar-tools` depends on every library crate. `recast-radar-core`
is always on and re-exported as `model`; every other crate is an optional
dependency re-exported as a module behind one feature:

| Feature | Module | Crate | Also enables |
|---|---|---|---|
| (always) | `model` | `recast-radar-core` | |
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
stream E.1 removed that dependency.) Features never add compile cost beyond
the crate itself, since the implied crates are its dependencies anyway. (`io`
used to enable `level3` although `recast-radar-io` did not depend on it; the
router has read Level III products since stream level3-complete, so that
exception is gone too.) `tests/readme.rs` checks the rule against the member
manifests. Its exception lists `NOT_IMPLIED` and `ALSO_IMPLIED` are empty, and
the test fails once an exception no longer applies.

Module names are the crate names without the `recast-radar-` and `io-`
prefixes; `recast-radar-data` is `data` (its feature is `net`, per the spec).
No root-level item re-exports: stream F renames the model types, and module
paths stay stable across that.

`level3` was added when the `level3` branch merged into `main` (1da8d33).
The VAD Wind Profile decoder that lived in `recast-radar-io-nexrad` has since
moved into `recast-radar-io-level3` (`vwp`); the facade did not change.

The facade module of `recast-radar-core` was called `core` until wave 3. A
module named `core` shadows Rust's built-in `core` crate in `use` paths
(`use core::...` inside a crate that imports the facade module), so it is
`model` now, the name of the core crate's main module; nothing in the facade
is called `core`.

- `net`: `recast-radar-data`'s networking is its default `net` feature
  (stream E.1). The facade depends on it with default features, so `net`
  keeps networking.
- `serde`: `recast-radar-core`'s derives are behind its `serde` feature
  (`dep:serde` and `chrono/serde`, `cfg_attr` derives; wave 3), which the
  facade feature forwards to. Without it the model does not depend on serde.
  `recast-radar-data` and `recast-radar-scattering` use serde for their own
  file formats, so `net` and `scattering` still build it.
  `crates/recast-radar-core/tests/serde_real.rs` round-trips real volumes.
- The dev-dependency on `recast-radar-testdata` is declared with
  `default-features = false`: without its `download` feature it has no
  HTTPS client, so `cargo run --example` (which builds dev-dependencies)
  does not compile ureq, rustls and ring. The facade tests read committed
  fixtures only.

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
own READMEs. `repository` is omitted: the remote is private, and whether a
published crate should link to it is the owner's decision.
`recast-radar-bench` (a harness binary) and `recast-radar-testdata` (test-only)
are `publish = false`. `tests/readme.rs` fails when a crate under `crates/`
lacks this metadata.

The facade's package excludes `tests/`. Those tests read the repository
(README, manifests, `ci.yml`, testdata), not the packaged crate. When
packaging, cargo drops the `[[test]]` target whose file is excluded. The
library crates exclude `tests/` for the same reason (their integration tests
read `testdata/` through the unpublished `recast-radar-testdata`) and
`examples/` (developer probes on dev-dependencies); the scattering crate also
excludes `tools/`, its offline PyTMatrix table generator.

Packages (wave 3): every internal normal dependency states
`version = "0.1.0"` beside its path (cargo drops path-only dev-dependencies
from a package), and every crate directory holds copies of `LICENSE-MIT` and
`LICENSE-APACHE`. `tests/readme.rs` checks both. `cargo package --workspace`
(a dry run) packages and verifies every crate, and CI runs it through
`tools/ci/package-check.sh`. Nothing is published. The dry run warns once
per excluded test or example target (cargo drops it from the packaged
manifest; about 110) and once per crate for the missing
`documentation`/`homepage`/`repository` links. The script counts those two
kinds and fails on any other warning. Shipping `tests/` and `examples/`
instead would silence the first kind, but would ship targets that cannot
run outside the repository (cargo drops their path-only dev-dependencies,
and their inputs live in `testdata/`); `autotests = false` would stop the
tests from running in the workspace.

## CI (G.2, extended in wave 3)

`.github/workflows/ci.yml` has these jobs. Every job except rustfmt fails if
`Cargo.lock` (or `fuzz/Cargo.lock`) is out of date (`--locked`).

- `rustfmt` (Rust 1.94.0): `cargo fmt --all --check`.
- `clippy` (Rust 1.94.0): `cargo clippy --workspace --all-targets
  --all-features --locked -- -D warnings`. `--all-features` also builds the
  facade examples that need `render` and `net`. Library crates deny
  `unwrap`/`expect` from their roots; integration test crates exempt their
  helpers at their roots (`clippy.toml` records the rule).
- `test` (stable; ubuntu-latest, windows-latest, macos-latest):
  `cargo test --workspace --locked --no-fail-fast`, then recast-radar-core
  with its serde feature (`cargo test -p recast-radar-core --features serde`:
  the round trip in `tests/serde_real.rs` and the serde unit tests),
  with a cache of downloaded test files at `$RECAST_RADAR_TESTDATA`. The cache
  key starts with the OS and the hash of `testdata/**/manifest.toml`, and a
  new entry is saved only when a run downloaded new files.
- `msrv` (Rust 1.94.0, ubuntu-latest): the workspace tests again on the
  minimum supported version.
- `docs` (stable): `cargo doc --workspace --no-deps --all-features` with
  `RUSTDOCFLAGS=-D warnings`, then `tools/ci/package-check.sh`, which runs
  `cargo package --workspace --locked`, a dry run that builds and verifies
  every crate's `.crate` file, and fails on an unexpected warning. Nothing
  is published.
- `facade features, pure Rust` (stable): `cargo fetch --locked`, `cargo hack
  check -p recast-radar-tools --each-feature --no-dev-deps`, then
  `tools/ci/pure-rust-check.sh`. `--no-dev-deps` removes dev-dependencies from
  the manifests while cargo-hack runs, which prunes `Cargo.lock`, so that
  command cannot take `--locked`; `cargo fetch --locked` checks the lock file
  before it. The script fails if `cc` or `cmake` is in the
  facade's dependency graph with every feature except `net` and `full`, for
  all targets.
- `wasm32` (stable): `tools/ci/wasm-check.sh` (see below).
- `golden` (stable, Python 3.12): `tools/ci/level2-golden-check.sh`, which
  runs `tools/level2_golden.py --check all` through the ignored
  `golden_script` test: MetPy 1.7.1 and arm_pyart 2.2.5 must reproduce every
  committed Level II golden byte for byte. `tools/ci/golden-requirements.txt`
  pins the whole Python environment as it was when the check passed in the
  Ubuntu 24.04 container (2026-09-24).
- `examples` (stable): `tools/check_example_outputs.py` builds the facade
  examples and runs each one whose output README.md or `docs/guide/` shows,
  on the file its `<!-- output: ... -->` marker names, and fails when the
  printed output differs from the document. `tests/readme.rs` checks that
  every output block has a marker; the `fetch_aws` output (a live listing)
  is marked unchecked.
- `fuzz` (stable): `cargo check` of the fuzz workspace without libFuzzer, and
  `fuzz-tools regressions`, which replays every committed fuzz regression
  input through its target and `io-router` with debug assertions and
  overflow checks.
- `fuzz-smoke` (nightly): `tools/ci/fuzz-smoke.sh 60` writes the seed corpora
  from the real testdata manifest, runs every libFuzzer target for 60 seconds
  and fails on any crash, timeout or out-of-memory input (uploaded as the
  `fuzz-findings` artifact).

Toolchains. rustfmt, clippy and the `msrv` tests are pinned to Rust 1.94.0,
the workspace `rust-version`. New Rust releases add clippy lints (1.98 warns
`clippy::chunks_exact_to_as_chunks` at 23 sites). With `stable`, the clippy
result would change without a commit, and `-D warnings` would fail on the
next release. The other jobs run on the latest stable, and the fuzz smoke run
on nightly. `tests/readme.rs` fails if the pins, `[workspace.package]
rust-version` and the README's "Minimum Rust version" disagree.

Platform. The tests were written on Windows and run on Linux, Windows and
macOS in CI. Two `recast-radar-scattering` tests froze f64 bits on
x86_64-pc-windows-msvc, and glibc's `pow`, `exp`, `ln` and `cbrt` round the
last bits differently (1 to 70 ULPs). Those tests compare exactly only on that
target, and within a relative 1e-12 elsewhere. The renderer computes pixel
azimuths with its own `atan2` (`recast-radar-render/src/trig.rs`, wave 3), in
plain f64 arithmetic rounded to f32, so the per-pixel angle is the same bits
on every target: the C libraries' `atan2f` differ in the last bit, which
moved display-upsampled pixels across azimuth bins on Linux. It is also
faster than the C library's `atan2f` and f64 `atan2`
([render-atan2.md](../perf/render-atan2.md)). The viewport rotation and
storm-motion `sin`/`cos` in recast-radar-render (once per frame or per ray),
and the cross-section and 3-D box-grid `hypot`/`atan2` in recast-radar-map,
call the platform's f64 functions and round to f32. No f32 C-library function
remains in a drawing path, but those f64 results round differently between
C libraries when one lies within about an ulp of an f32 rounding boundary:
unlikely, not ruled out. `real_render_parity` passes
with the same fingerprints on Windows (MSVC CRT), Linux with glibc 2.39 and
Linux with musl (x86_64-unknown-linux-musl), three independent libm
implementations; macOS has not been run. The bench pixel checksums in
`docs/baselines/import-checksums.txt` are now the same on Windows, Linux
glibc and Linux musl (`main`, with `atan2f`, printed other values on both
Linux libcs; see [wasm.md](wasm.md)). CI does not check them.

## WASM (G.3)

See [wasm.md](wasm.md). Every non-`net` crate already passed
`cargo check --target wasm32-unknown-unknown` without source changes, so no
`parallel` feature was added: rayon falls back to the calling thread on that
target. `tools/ci/wasm-check.sh` checks each workspace crate on its own, and
the facade with each feature except `net` and `full`. It checks
`recast-radar-data` with `--no-default-features` (without its `net` feature)
and with `--features async-client`, includes the `recast-radar-bench` harness
binary (wave 3; spec 9.7 asks for every non-`net` crate), and leaves out only
the test-only `recast-radar-testdata`.

## README (G.4)

The root `README.md` has a quick start (one example), the table of the user
guide, the crate map, the feature table, and the pure-Rust, WebAssembly and
no-unsafe statements. The user guide (`docs/guide/`, wave 3) has a page per
task with the other examples.

- The examples are `crates/recast-radar-tools/examples/*.rs` (`decode_level2`
  in the README; `read_any`, `physical_values`, `fetch_aws`,
  `dealias_velocity`, `composite` and `render_png` in the guide), shown
  verbatim, with `required-features` in the facade manifest. They take their
  inputs on the command line. The output excerpts come from runs on
  `KTLX20240315_000217_V06` and the committed ESPDG ODIM_H5 fixture
  (`fetch_aws` from a live run). `python tools/sync_doc_examples.py` copies
  the examples into the documents after an edit.
- `crates/recast-radar-tools/tests/readme.rs` keeps the README and the guide
  honest. Every `rust` code block must be preceded by
  `<!-- example: <path> -->` and equal that file (fragments are
  `rust,ignore`), and every facade example must appear; relative links must
  resolve. The crate map
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
