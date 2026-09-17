# F.3 (algo): migration of the algorithm crates to the FM301 model

> **Historical record (status 2026-09-17).** This note describes the
> `fm301-algo` sub-branch as it was merged (`1c68329`). The compatibility shim
> it relies on (`core::legacy`, each crate's `legacy_api` and `legacy_bridge`
> modules, the `recast_legacy_deprecation` gate and the probes named below)
> was removed at `b811f2d`, so the commands and files it cites no longer
> exist on `fm301`; they remain in the history before that commit. Since the
> verifier fixes of 2026-09-17 the products take a sweep's tilt elevation
> from `Sweep::tilt_elevation_deg` (a Level II sweep's first-ray elevation,
> as on `main`), and the bench checksums are the import values again. The
> current state is in `fm301-model.md`, section Status.

Companion to `fm301-model.md` (sections 13.3 and 13.4). Branch `fm301-algo`,
from `fm301` @ `e4df5bd` (F.2) with `main` merged at `349c8e4`. Crates:
`recast-radar-filters`, `recast-radar-correct`, `recast-radar-retrieve`,
`recast-radar-map`, `recast-radar-track`. At the time the shim in core stayed;
the io and render crates were migrated by `fm301-io` and `fm301-render`.

## What changed

Every public function works on `Volume` / `Sweep` / `Field`. The algorithms
are unchanged: only their inputs and outputs moved.

- Inputs are found by `Quantity` (`Sweep::find`), so DBZH, DBZ and DBZHC all
  serve as reflectivity; volume-level products that need one name across
  sweeps (`cappi`, `column_max`, `field_section`, `value_swath`) take a
  `&FieldName`.
- Samplers work by ray and physical range. Every field of a sweep is on the
  sweep's rays, so the legacy `radial_indices` cross-referencing between
  grids of one cut is gone. Gate geometry comes from
  `Field::native_geometry(&sweep.range)` in f64.
- Outputs are physical `F32` fields on the base field's rays, native gates
  (`GateMapping` kept) and absent rows. Dealiased velocity keeps its `u16`
  packing (`(raw - 32768) / 10`, raw 0 = `_FillValue`) under the name
  `VRADDH`.
- Sentinels resolve through the field's coding (`Field::value`), so
  `valid_range` and `_Undetect` of natively decoded fields are honoured. Note
  `Field::value` returns `None` for a stored NaN where the legacy
  `scaled_value` returned `Some(NaN)`; the algorithms already treated both as
  "no value".

### Names of derived fields (design note 8.3)

`DerivedSweepProduct::field_name_in(sweep)`: fixed ids (`KDP`, `KDP_SD`,
`AH`, `PIA`, `ADP`, `PIDA`, `RR`, `RR_Z`, `RR_KDP`, `LWC`, `HKE`, `CDR`,
`MET_QI`, `MET_MASK`, `TDS_SCORE`, `HAIL_SCORE`, `TURB`) and `<BASE>_<SUFFIX>`
with `BASE` the input field's own name: `_TEX`, `_GRAD_R`, `_CORR`,
`_CLEAN` (PHIDP), `_LOG` (RHOHV). `DerivedSweepProduct::for_name` is the
inverse; `KDP` never maps to a derived product (the derive-on-demand rule of
`availability`). Other products: `AZSHEAR`, `DIVSHEAR`, `MARC`, `GUST`,
`CAPPI_<NAME>_<h>KM`, `CMAX_<NAME>`, `CMIN_<NAME>`, `CMEAN_<NAME>`, `LLCREF`,
`EBASE`, `ET`, `EDEPTH`, `HMAX` (retrieve); `CREF`, `ET`, `VIL`, `VILD`,
`SHI`, `MESH`, `POSH`, `POH` (map); temporal products keep the caller's
name.

### Public names

| Legacy (kept as wrapper) | FM301 |
|---|---|
| `filters::smooth_moment_grid` | `smooth_field` |
| `filters::apply_reflectivity_gate_filter(cut, grid, ..)` | `apply_reflectivity_gate_filter(sweep, field, ..)` (shadows) |
| `filters::upsample_moment_grid` -> `InterpolatedGrid` | `upsample_field` -> `UpsampledSweep` (a sweep of its own with `parent_rays`) |
| `correct::dealias_velocity_grid[_with_reference]` | `dealias_velocity[_with_reference]` |
| `correct::dealias_velocity_grid_pyart_region` | `dealias_velocity_pyart_region` |
| `correct::fit_range_band_reference` | `range_band_reference` |
| `correct::dealias_volume_v4`, `dealias_velocity_grid_v4` | `dealias_volume`, `dealias_velocity_v4` |
| `correct::project_environmental_winds` | `project_environmental_winds_onto` |
| `V4VolumeSolution::tilt_grid`, `into_tilt_grid` | `tilt_field`, `into_tilt_field` |
| `correct::{radial_azimuths, copy_scaled_velocity_row, dealias_skipped_no_nyquist}` | same names (shadow) |
| `retrieve::derive_cut_in_place` -> `CutDerivationReport` | `derive_sweep_in_place` -> `SweepDerivationReport` |
| `retrieve::{derive_volume_in_place, derive_product}` | same names (shadow) |
| `retrieve::cut_has_moment_source`, `cut_can_materialize_moment`, `advanced_derived_product_for_moment`, `cut_has_advanced_product_sources` | `sweep_has_field_source`, `sweep_can_materialize_field`, `advanced_derived_product_for_name`, `sweep_has_advanced_product_sources` |
| `retrieve::{azimuthal_shear_grid, radial_divergence_grid}[_from_dealiased]` | `{azimuthal_shear, radial_divergence}[_from_dealiased]` |
| `retrieve::{marc_grid, gust_proxy_grid}[_from_dealiased]` | `{marc, gust_proxy}[_from_dealiased]` |
| `retrieve::{cappi_grid, column_*_grid, echo_*_grid, height_of_max_reflectivity_grid, low_level_composite_reflectivity_grid}` | without `_grid` |
| `retrieve::rotation_velocity_cut_indices` | `rotation_velocity_sweep_indices` |
| `retrieve::{detect_rotation_sites, rotation_features_per_tilt}[_from_dealiased]`, `compute_vwp` | same names (shadow) |
| `PolarVelocityField::from_dealiased_velocity(cut, grid)` | `from_dealiased_velocity(sweep, field)`; legacy `from_dealiased_velocity_grid` |
| `map::{composite_reflectivity, echo_top, vil, vil_density, mehs, poh}_grid`, `hail_grids` -> `HailGrids` | without `_grid`; `hail` -> `HailFields` |
| `map::reflectivity_cross_section[_with_smoothing]` | `reflectivity_section[_with_smoothing]` |
| `map::moment_cross_section[_with_smoothing]` | `field_section[_with_smoothing]` |
| `map::velocity_cross_section[_cached[_with_smoothing]]`, `VolumeDealiasCache` | `velocity_section[...]`; legacy memo `LegacyVolumeDealiasCache` |
| `map::volume_box_resample[_moment]` | `box_resample[_field]` |
| `map::{cut_looks_like_rhi, rhi_fixed_azimuth_deg, rhi_coverage_top_m, rhi_coverage_range_m, rhi_section}` | `sweep_looks_like_rhi`, `rhi_fixed_azimuth`, `rhi_coverage_top`, `rhi_coverage_range`, `rhi_panel` |
| `track::base_tilt_cut`, `max_value_swath` | `base_tilt_sweep`, `value_swath` |
| `track::*_grid` temporal functions | without `_grid` (`difference`, `trend`, `maximum_swath`, ...) |
| `track::tracks::low_level_azshear_cut_indices` | `low_level_azshear_sweep_indices` |
| `track::{identify_storm_cells, tracks::{low_level_azshear_cartesian[_from_dealiased], detect_tds_gates}}` | same names (shadow) |

"Shadow": the FM301 function kept the legacy name; the explicit `pub use`
wins over `pub use legacy_api::*`, so the legacy form is reachable only as
`legacy_api::<name>`. That was done only where no un-migrated crate calls
the legacy form. Names the render crate still calls
(`composite_reflectivity_grid`, `echo_top_grid`, `vil_grid`,
`vil_density_grid`, `mehs_grid`, `reflectivity_cross_section`,
`azimuthal_shear_grid`, `radial_divergence_grid`, `base_tilt_cut`,
`max_value_swath`, `dealias_velocity_grid`, `smooth_moment_grid`) keep the
crate-root slot, which is why the FM301 versions of those got new names.
`TemporalPrior::Volume` holds a `Volume`; nothing outside `correct`
constructed it. `VwpError::UnsupportedScanMode` carries a `SweepMode`.

### Legacy wrappers

Each crate's `legacy_api` keeps every old signature as a thin wrapper:
legacy -> FM301 -> algorithm -> legacy. The conversion lives in
`recast_radar_correct::legacy_api::convert` (shared by map, retrieve and
track) and differs from `recast_radar_core::legacy::volume_from_legacy` on
purpose:

- It converts one grid (or the moments a wrapper needs) instead of cloning
  a whole cut, so the bench's velocity render pays one grid clone, not six.
- It reads `GateRange::first_gate_m` as the range of gate 0 for every
  decoder, which is what the legacy algorithms did, so every range an
  algorithm computes is the value it computed before (design note 6.6 moves
  ODIM and CfRadial gates by half a gate; that is the io crates' migration,
  not this one).
- Rows are placed by radial index; rays a grid lacks become absent rows;
  caller-provided grids (`_from_dealiased` slices) become detached fields on
  the converted sweeps. Outputs go back through `grid_like` (with a
  template) or `grid_from_field` (rows the field provides, in ray order).
- Retrieve's wrappers map legacy derived ids to the canonical FM301 names
  (`REF_TEX` <-> `DBZH_TEX`, `REFC` <-> `DBZH_CORR`, `PHIF` <-> `PHIDP_CLEAN`,
  `L_RHO` <-> `RHOHV_LOG`, `RATE*` <-> `RR*`) and back, so
  `skipped_existing` and `overwrite_existing` behave as before.
- The availability predicates (row counts only) keep their legacy form
  directly rather than converting grids for a boolean.

A legacy cut whose moments cannot share one range coordinate (non-multiple
spacings, or edges off the finer lattice) has no FM301 form; the wrapper
leaves such a moment out (the product is then unavailable). No decoded
corpus file has one (design note 17, round-trip evidence); the synthetic
`rho_qc_is_aligned_by_physical_range` fixture that did was given a
physically consistent geometry (RHO first centre 125 m on 500 m gates beside
PHI on 250 m gates).

Tests and examples that decode real files through the un-migrated io
crates use a private `legacy_bridge` around
`recast_radar_core::legacy::volume_from_legacy` (design note 13.3). Map's
DOW8 RHI test now derives its coverage expectation from the sweep's own
range, because the bridge places CfRadial gate centres at the file's `range`
values, half a gate past the legacy start-of-gate reading (6.6); its
pixel-exact spot check still passes.

## Evidence (acceptance, design note 13.4)

- **Behaviour identical.** `crates/recast-radar-track/examples/fm301_algo_golden.rs`
  (at `ce3e6b3`; deleted with the shim)
  hashes every public product of the five crates through the legacy API
  over seven real volumes (KTLX 2024-03-15 and 2013-05-20 Level II, KLIX
  2005 Message 1, dkrom and iesha ODIM, DOW8 RHI and IRENE CfRadial; `CORPUS`
  names the bench corpus directory). 1520 of 1520 lines are identical before
  (the un-migrated tree at `7ebb3b0`; the later `main` merge `349c8e4` did
  not touch these crates) and after the migration. The probe maps the
  renamed Debug names
  (`SweepDerivationReport`, `sweep_index`, `velocity_sweep_count`) back
  before hashing; values are untouched. Deleted with the shim.
- **Checksums.** `cargo run --release -p recast-radar-bench -- <file> --iters 1`
  prints the three checksums of `docs/baselines/import-checksums.txt`
  (0xc04a5e2dfecc4c1f, 0xd5080047ae5dfeb5, 0x19e3735f42cdca4b; the KTLX
  values were re-recorded at the shim removal and restored by the verifier
  fixes of 2026-09-17, see that file); the
  velocity render goes through `dealias_velocity_grid` -> `dealias_velocity`.
- **Single-core decode** (`RAYON_NUM_THREADS=1`, KTLX20240315_000217_V06,
  `--iters 10`, three interleaved rounds, release builds of `349c8e4` and
  the migrated tree): decode mean 376.3 / 347.8 / 347.1 ms before, 358.4 /
  350.3 / 350.2 ms after (best 308.1 ms both). Nothing in this stream touches
  a decode path. Velocity raster (with the dealias wrapper): 192.1 / 192.5 /
  180.6 ms before, 190.5 / 191.8 / 200.0 ms after (best 149.1 vs 148.6 ms).
- **Gate.** `CARGO_TARGET_DIR=target/legacy-gate RUSTFLAGS="--cfg recast_legacy_deprecation" cargo check -p <crate> --all-targets`
  passes for all five crates with no warning from any of their targets;
  every lib, test and example target declares
  `#![cfg_attr(recast_legacy_deprecation, deny(deprecated))]` (the probe,
  which exercises the legacy API on purpose, allows it). The companion grep
  finds legacy names only in `legacy_api` and `legacy_bridge` modules.
- **Tests.** `cargo test --workspace`: 1222 passed, 0 failed. Per crate:
  filters 16, correct 38 (one added), retrieve 52 (50 before; a
  helper-only test replaced by three new ones), map 16, track 27.
- Clippy (all targets) and `cargo doc --no-deps` with `-D warnings` are clean
  for the five crates.

## Left for the merge and shim removal

- `recast_radar_core::bounded_read` gained legacy-typed helpers from stream
  D after F.2; under the cfg it warns (core, not these crates).
- The render crate's examples and the facade's README examples still use
  the legacy names through the wrappers; `fm301-render` migrates them.
- At shim removal: delete each `legacy_api` and every `legacy_bridge`, the
  probe, and the `deny(deprecated)` attributes (design note 13.4).
