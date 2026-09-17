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

## CI (G.2), WASM (G.3), README (G.4)

Planned per the wave 2 plan; this note is extended as those tasks land.
