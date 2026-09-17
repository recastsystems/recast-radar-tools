//! Legacy-model signatures kept while other crates migrate to the FM301
//! model (`docs/design/fm301-model.md` section 13.3). Only this module names
//! legacy items; it is deleted with the shim.
//!
//! Every public function that took legacy types keeps its old name and
//! signature here (the render crate's examples still call `base_tilt_cut`
//! and `max_value_swath`). Where the FM301 function kept the legacy name
//! (`identify_storm_cells`, `low_level_azshear_cartesian[_from_dealiased]`,
//! `detect_tds_gates`) it shadows the wrapper at the crate root, and the
//! legacy form is reachable as `legacy_api::<name>` only; no un-migrated
//! crate calls those. Each wrapper converts its legacy input to the FM301
//! model with `recast_radar_correct::legacy_api::convert`, calls the FM301
//! function and converts the result back, so both paths share one
//! implementation and produce identical values.

#![allow(deprecated)]

use recast_radar_core::{
    ElevationCut, Field, GateRange, MomentGrid, MomentType, RadarVolume, Radial, Volume,
};
use recast_radar_correct::legacy_api::convert;
use recast_radar_retrieve::RotationSite;

use crate::tracks::{TdsGate, TracksGridSpec};
use crate::{StormCell, SwathAggregation};

fn moment_is(moment: MomentType) -> impl Fn(&MomentType) -> bool {
    move |candidate| *candidate == moment
}

fn reflectivity_only(moment: &MomentType) -> bool {
    *moment == MomentType::Reflectivity
}

fn velocity_and_reflectivity(moment: &MomentType) -> bool {
    matches!(moment, MomentType::Velocity | MomentType::Reflectivity)
}

fn dual_pol_low_level(moment: &MomentType) -> bool {
    matches!(
        moment,
        MomentType::Reflectivity | MomentType::CorrelationCoefficient
    )
}

/// Caller-provided grids indexed like `volume.cuts` as detached fields of
/// the converted volume's sweeps (each on its sweep's rays and range).
fn detached_fields(converted: &mut Volume, grids: &[Option<&MomentGrid>]) -> Vec<Option<Field>> {
    let radials: Vec<Vec<usize>> = converted
        .sweeps
        .iter()
        .map(|sweep| (0..sweep.nrays()).collect())
        .collect();
    // Attaching a geometry may refine or extend a sweep's range, which
    // remaps every field, so the fields are built once every geometry is
    // attached.
    for (index, grid) in grids.iter().enumerate() {
        if let (Some(grid), Some(sweep)) = (grid, converted.sweeps.get_mut(index)) {
            let _ = convert::field_for_sweep(sweep, &radials[index], grid, &convert::field_name);
        }
    }
    grids
        .iter()
        .enumerate()
        .map(|(index, grid)| {
            let grid = (*grid)?;
            let sweep = converted.sweeps.get_mut(index)?;
            convert::field_for_sweep(sweep, &radials[index], grid, &convert::field_name)
        })
        .collect()
}

// ---------------------------------------------------------------- cells --

/// Identify storm cells on the volume's composite reflectivity. See
/// [`crate::identify_storm_cells`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_track::identify_storm_cells")
)]
pub fn identify_storm_cells(volume: &RadarVolume) -> Vec<StormCell> {
    crate::identify_storm_cells(&convert::volume_with(volume, reflectivity_only))
}

// ---------------------------------------------------------------- swath --

/// Lowest-elevation cut of `volume` that carries `moment` with decoded
/// rows. See [`crate::base_tilt_sweep`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_track::base_tilt_sweep")
)]
pub fn base_tilt_cut(volume: &RadarVolume, moment: &MomentType) -> Option<usize> {
    let converted = convert::volume_with(volume, moment_is(moment.clone()));
    crate::base_tilt_sweep(&converted, &convert::field_name(moment))
}

/// Build the per-gate max-value swath over `frames` for `moment`. See
/// [`crate::value_swath`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_track::value_swath")
)]
pub fn max_value_swath(
    frames: &[&RadarVolume],
    moment: MomentType,
    aggregation: SwathAggregation,
) -> Option<RadarVolume> {
    let converted: Vec<Volume> = frames
        .iter()
        .map(|frame| convert::volume_with(frame, moment_is(moment.clone())))
        .collect();
    let borrowed: Vec<&Volume> = converted.iter().collect();
    let swath = crate::value_swath(&borrowed, &convert::field_name(&moment), aggregation)?;
    let newest = frames
        .iter()
        .max_by_key(|volume| volume.volume_time)
        .copied()?;

    let sweep = swath.sweeps.into_iter().next()?;
    let field = sweep.fields.first()?.clone();
    let grid = convert::grid_from_field(field, moment.clone(), &sweep);
    let mut cut = ElevationCut::new(sweep.fixed_angle_deg, Some(1));
    cut.radials = sweep
        .rays
        .azimuth_deg
        .iter()
        .map(|&azimuth_deg| Radial {
            azimuth_deg,
            elevation_deg: sweep.fixed_angle_deg,
            time_offset_ms: 0,
            gate_range: GateRange {
                first_gate_m: grid.gate_range.first_gate_m,
                gate_spacing_m: grid.gate_range.gate_spacing_m,
                gate_count: grid.gate_range.gate_count,
            },
            nyquist_velocity_mps: None,
            radial_status: None,
        })
        .collect();
    cut.moments.insert(moment, grid);

    let mut volume = RadarVolume::new(newest.site.clone(), newest.volume_time);
    volume.cuts.push(cut);
    Some(volume)
}

// ------------------------------------------------------------- temporal --

/// Legacy geometry test: identical gate range and radial indices.
fn geometry_matches(left: &MomentGrid, right: &MomentGrid) -> bool {
    left.gate_range == right.gate_range && left.radial_indices == right.radial_indices
}

fn binary(
    newer: &MomentGrid,
    older: &MomentGrid,
    output_moment: MomentType,
    op: impl Fn(&Field, &Field, recast_radar_core::FieldName) -> Option<Field>,
) -> Option<MomentGrid> {
    if !geometry_matches(newer, older) {
        return None;
    }
    let output = convert::field_name(&output_moment);
    let field = op(
        &convert::field_for_grid(newer),
        &convert::field_for_grid(older),
        output,
    )?;
    Some(convert::grid_like(field, output_moment, newer, None))
}

fn many(
    grids: &[&MomentGrid],
    output_moment: MomentType,
    op: impl Fn(&[&Field], recast_radar_core::FieldName) -> Option<Field>,
) -> Option<MomentGrid> {
    let first = *grids.first()?;
    if grids.iter().any(|grid| !geometry_matches(first, grid)) {
        return None;
    }
    let fields: Vec<Field> = grids
        .iter()
        .map(|grid| convert::field_for_grid(grid))
        .collect();
    let borrowed: Vec<&Field> = fields.iter().collect();
    let field = op(&borrowed, convert::field_name(&output_moment))?;
    Some(convert::grid_like(field, output_moment, first, None))
}

fn timed(
    frames: &[(&MomentGrid, f64)],
    output_moment: MomentType,
    op: impl Fn(&[(&Field, f64)], recast_radar_core::FieldName) -> Option<Field>,
) -> Option<MomentGrid> {
    let (first, _) = *frames.first()?;
    if frames
        .iter()
        .any(|(grid, _)| !geometry_matches(first, grid))
    {
        return None;
    }
    let fields: Vec<(Field, f64)> = frames
        .iter()
        .map(|(grid, time)| (convert::field_for_grid(grid), *time))
        .collect();
    let borrowed: Vec<(&Field, f64)> = fields.iter().map(|(field, time)| (field, *time)).collect();
    let field = op(&borrowed, convert::field_name(&output_moment))?;
    Some(convert::grid_like(field, output_moment, first, None))
}

/// See [`crate::difference`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_track::difference")
)]
pub fn difference_grid(
    newer: &MomentGrid,
    older: &MomentGrid,
    output_moment: MomentType,
) -> Option<MomentGrid> {
    binary(newer, older, output_moment, crate::difference)
}

/// See [`crate::trend`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_track::trend")
)]
pub fn trend_grid(
    newer: &MomentGrid,
    older: &MomentGrid,
    elapsed_seconds: f64,
    output_moment: MomentType,
) -> Option<MomentGrid> {
    binary(newer, older, output_moment, |newer, older, output| {
        crate::trend(newer, older, elapsed_seconds, output)
    })
}

/// See [`crate::maximum_swath`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_track::maximum_swath")
)]
pub fn maximum_swath_grid(grids: &[&MomentGrid], output_moment: MomentType) -> Option<MomentGrid> {
    many(grids, output_moment, crate::maximum_swath)
}

/// See [`crate::minimum_swath`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_track::minimum_swath")
)]
pub fn minimum_swath_grid(grids: &[&MomentGrid], output_moment: MomentType) -> Option<MomentGrid> {
    many(grids, output_moment, crate::minimum_swath)
}

/// See [`crate::mean`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_track::mean")
)]
pub fn mean_grid(grids: &[&MomentGrid], output_moment: MomentType) -> Option<MomentGrid> {
    many(grids, output_moment, crate::mean)
}

/// See [`crate::accumulate_rates`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_track::accumulate_rates")
)]
pub fn accumulate_rate_grids(
    frames: &[(&MomentGrid, f64)],
    output_moment: MomentType,
) -> Option<MomentGrid> {
    timed(frames, output_moment, crate::accumulate_rates)
}

/// See [`crate::exceedance_duration`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_track::exceedance_duration")
)]
pub fn exceedance_duration_grid(
    frames: &[(&MomentGrid, f64)],
    threshold: f32,
    output_moment: MomentType,
) -> Option<MomentGrid> {
    timed(frames, output_moment, |frames, output| {
        crate::exceedance_duration(frames, threshold, output)
    })
}

/// See [`crate::exceedance_probability`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_track::exceedance_probability")
)]
pub fn exceedance_probability_grid(
    grids: &[&MomentGrid],
    threshold: f32,
    output_moment: MomentType,
) -> Option<MomentGrid> {
    many(grids, output_moment, |grids, output| {
        crate::exceedance_probability(grids, threshold, output)
    })
}

// --------------------------------------------------------------- tracks --

/// Ordered, bounded cut set consumed by the low-level track composite. See
/// [`crate::tracks::low_level_azshear_sweep_indices`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(
        note = "FM301 migration: use recast_radar_track::tracks::low_level_azshear_sweep_indices"
    )
)]
pub fn low_level_azshear_cut_indices(volume: &RadarVolume) -> Vec<usize> {
    crate::tracks::low_level_azshear_sweep_indices(&convert::volume_with(
        volume,
        moment_is(MomentType::Velocity),
    ))
}

/// Low-level azimuthal shear on a Cartesian grid. See
/// [`crate::tracks::low_level_azshear_cartesian`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(
        note = "FM301 migration: use recast_radar_track::tracks::low_level_azshear_cartesian"
    )
)]
pub fn low_level_azshear_cartesian(volume: &RadarVolume, spec: &TracksGridSpec) -> Vec<f32> {
    crate::tracks::low_level_azshear_cartesian(
        &convert::volume_with(volume, velocity_and_reflectivity),
        spec,
    )
}

/// Low-level azimuthal shear from caller-provided dealiased grids indexed
/// like `volume.cuts`. See
/// [`crate::tracks::low_level_azshear_cartesian_from_dealiased`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(
        note = "FM301 migration: use recast_radar_track::tracks::low_level_azshear_cartesian_from_dealiased"
    )
)]
pub fn low_level_azshear_cartesian_from_dealiased(
    volume: &RadarVolume,
    dealiased_velocity: &[Option<&MomentGrid>],
    spec: &TracksGridSpec,
) -> Vec<f32> {
    let mut converted = convert::volume_with(volume, velocity_and_reflectivity);
    let fields = detached_fields(&mut converted, dealiased_velocity);
    let borrowed: Vec<Option<&Field>> = fields.iter().map(Option::as_ref).collect();
    crate::tracks::low_level_azshear_cartesian_from_dealiased(&converted, &borrowed, spec)
}

/// Per-gate TDS flags on the lowest dual-pol tilt. See
/// [`crate::tracks::detect_tds_gates`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_track::tracks::detect_tds_gates")
)]
pub fn detect_tds_gates(volume: &RadarVolume, sites: &[RotationSite]) -> Vec<TdsGate> {
    crate::tracks::detect_tds_gates(&convert::volume_with(volume, dual_pol_low_level), sites)
}
