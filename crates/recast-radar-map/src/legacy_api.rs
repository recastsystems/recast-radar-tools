//! Legacy-model signatures kept while other crates migrate to the FM301
//! model (`docs/design/fm301-model.md` section 13.3). Only this module names
//! legacy items; it is deleted with the shim.
//!
//! Every public function that took legacy types keeps its old name and
//! signature here (the render crate's examples still call
//! `composite_reflectivity_grid`, `echo_top_grid`, `vil_grid`,
//! `vil_density_grid`, `mehs_grid` and `reflectivity_cross_section`). Each
//! wrapper converts its legacy input to the FM301 model with
//! `recast_radar_correct::legacy_api::convert`, calls the FM301 function and
//! converts the result back, so both paths share one implementation and
//! produce identical values. Column products come back as F32 grids keyed
//! `Reflectivity` on the base tilt's geometry, as before.

#![allow(deprecated)]

use recast_radar_core::{ElevationCut, Field, MomentGrid, MomentType, RadarVolume, Sweep, Volume};
use recast_radar_correct::legacy_api::convert;

use crate::{CrossSection, CrossSectionSmoothing, HailFields, InterpPolicy, MeshCalibration};

fn reflectivity_only(moment: &MomentType) -> bool {
    *moment == MomentType::Reflectivity
}

fn velocity_only(moment: &MomentType) -> bool {
    *moment == MomentType::Velocity
}

/// A column product converted back to a legacy grid keyed `Reflectivity` on
/// the lowest reflectivity cut's geometry.
fn reflectivity_grid(
    volume: &RadarVolume,
    product: impl Fn(&Volume) -> Option<Field>,
) -> Option<MomentGrid> {
    let converted = convert::volume_with(volume, reflectivity_only);
    let field = product(&converted)?;
    let base = converted
        .sweeps
        .iter()
        .filter(|sweep| sweep.field(&recast_radar_core::FieldName::Dbzh).is_some())
        .min_by(|a, b| a.fixed_angle_deg.total_cmp(&b.fixed_angle_deg))?;
    Some(convert::grid_from_field(
        field,
        MomentType::Reflectivity,
        base,
    ))
}

/// Composite (column-max) reflectivity. See [`crate::composite_reflectivity`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_map::composite_reflectivity")
)]
pub fn composite_reflectivity_grid(volume: &RadarVolume) -> Option<MomentGrid> {
    reflectivity_grid(volume, crate::composite_reflectivity)
}

/// Echo-top height. See [`crate::echo_top`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_map::echo_top")
)]
pub fn echo_top_grid(volume: &RadarVolume, threshold_dbz: f32) -> Option<MomentGrid> {
    reflectivity_grid(volume, |converted| {
        crate::echo_top(converted, threshold_dbz)
    })
}

/// Vertically Integrated Liquid. See [`crate::vil`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_map::vil")
)]
pub fn vil_grid(volume: &RadarVolume) -> Option<MomentGrid> {
    reflectivity_grid(volume, crate::vil)
}

/// VIL density. See [`crate::vil_density`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_map::vil_density")
)]
pub fn vil_density_grid(volume: &RadarVolume) -> Option<MomentGrid> {
    reflectivity_grid(volume, crate::vil_density)
}

/// Witt-calibrated MESH. See [`crate::mehs`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_map::mehs")
)]
pub fn mehs_grid(
    volume: &RadarVolume,
    freezing_level_m: f32,
    minus20c_level_m: f32,
) -> Option<MomentGrid> {
    reflectivity_grid(volume, |converted| {
        crate::mehs(converted, freezing_level_m, minus20c_level_m)
    })
}

/// Probability of hail. See [`crate::poh`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_map::poh")
)]
pub fn poh_grid(volume: &RadarVolume, freezing_level_m: f32) -> Option<MomentGrid> {
    reflectivity_grid(volume, |converted| crate::poh(converted, freezing_level_m))
}

/// SHI + MESH + POSH. See [`crate::hail`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_map::HailFields")
)]
pub struct HailGrids {
    pub shi: MomentGrid,
    pub mesh_mm: MomentGrid,
    pub posh_pct: MomentGrid,
}

/// SHI + MESH + POSH in one column walk. See [`crate::hail`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_map::hail")
)]
pub fn hail_grids(
    volume: &RadarVolume,
    freezing_level_m: f32,
    minus20c_level_m: f32,
    calibration: MeshCalibration,
) -> Option<HailGrids> {
    let converted = convert::volume_with(volume, reflectivity_only);
    let HailFields {
        shi,
        mesh_mm,
        posh_pct,
    } = crate::hail(&converted, freezing_level_m, minus20c_level_m, calibration)?;
    let base = converted
        .sweeps
        .iter()
        .filter(|sweep| sweep.field(&recast_radar_core::FieldName::Dbzh).is_some())
        .min_by(|a, b| a.fixed_angle_deg.total_cmp(&b.fixed_angle_deg))?;
    let grid = |field: Field| convert::grid_from_field(field, MomentType::Reflectivity, base);
    Some(HailGrids {
        shi: grid(shi),
        mesh_mm: grid(mesh_mm),
        posh_pct: grid(posh_pct),
    })
}

/// Reflectivity cross-section. See [`crate::reflectivity_section`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_map::reflectivity_section")
)]
pub fn reflectivity_cross_section(
    volume: &RadarVolume,
    start_km: (f32, f32),
    end_km: (f32, f32),
    width: usize,
    height: usize,
    top_m: f32,
) -> Option<CrossSection> {
    crate::reflectivity_section(
        &convert::volume_with(volume, reflectivity_only),
        start_km,
        end_km,
        width,
        height,
        top_m,
    )
}

/// Reflectivity cross-section with a smoothing choice. See
/// [`crate::reflectivity_section_with_smoothing`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(
        note = "FM301 migration: use recast_radar_map::reflectivity_section_with_smoothing"
    )
)]
pub fn reflectivity_cross_section_with_smoothing(
    volume: &RadarVolume,
    start_km: (f32, f32),
    end_km: (f32, f32),
    width: usize,
    height: usize,
    top_m: f32,
    smoothing: CrossSectionSmoothing,
) -> Option<CrossSection> {
    crate::reflectivity_section_with_smoothing(
        &convert::volume_with(volume, reflectivity_only),
        start_km,
        end_km,
        width,
        height,
        top_m,
        smoothing,
    )
}

/// Box resample of the reflectivity volume. See [`crate::box_resample`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_map::box_resample")
)]
pub fn volume_box_resample(
    volume: &RadarVolume,
    center_east_km: f32,
    center_north_km: f32,
    half_km: f32,
    n: usize,
    nz: usize,
    top_m: f32,
) -> Option<Vec<f32>> {
    crate::box_resample(
        &convert::volume_with(volume, reflectivity_only),
        center_east_km,
        center_north_km,
        half_km,
        n,
        nz,
        top_m,
    )
}

/// Box resample of one moment. See [`crate::box_resample_field`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_map::box_resample_field")
)]
#[allow(clippy::too_many_arguments)]
pub fn volume_box_resample_moment(
    volume: &RadarVolume,
    moment: &MomentType,
    policy: InterpPolicy,
    center_east_km: f32,
    center_north_km: f32,
    half_km: f32,
    n: usize,
    nz: usize,
    top_m: f32,
) -> Option<Vec<f32>> {
    let wanted = moment.clone();
    crate::box_resample_field(
        &convert::volume_with(volume, |candidate| *candidate == wanted),
        &convert::field_name(moment),
        policy,
        center_east_km,
        center_north_km,
        half_km,
        n,
        nz,
        top_m,
    )
}

/// Single-moment cross-section. See [`crate::field_section`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_map::field_section")
)]
#[allow(clippy::too_many_arguments)]
pub fn moment_cross_section(
    volume: &RadarVolume,
    moment: MomentType,
    policy: InterpPolicy,
    start_km: (f32, f32),
    end_km: (f32, f32),
    width: usize,
    height: usize,
    top_m: f32,
) -> Option<CrossSection> {
    moment_cross_section_with_smoothing(
        volume,
        moment,
        policy,
        start_km,
        end_km,
        width,
        height,
        top_m,
        CrossSectionSmoothing::Smoothed,
    )
}

/// Single-moment cross-section with a smoothing choice. See
/// [`crate::field_section_with_smoothing`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_map::field_section_with_smoothing")
)]
#[allow(clippy::too_many_arguments)]
pub fn moment_cross_section_with_smoothing(
    volume: &RadarVolume,
    moment: MomentType,
    policy: InterpPolicy,
    start_km: (f32, f32),
    end_km: (f32, f32),
    width: usize,
    height: usize,
    top_m: f32,
    smoothing: CrossSectionSmoothing,
) -> Option<CrossSection> {
    let wanted = moment.clone();
    crate::field_section_with_smoothing(
        &convert::volume_with(volume, |candidate| *candidate == wanted),
        &convert::field_name(&moment),
        policy,
        start_km,
        end_km,
        width,
        height,
        top_m,
        smoothing,
    )
}

/// Dealiased-velocity cross-section. See [`crate::velocity_section`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_map::velocity_section")
)]
pub fn velocity_cross_section(
    volume: &RadarVolume,
    start_km: (f32, f32),
    end_km: (f32, f32),
    width: usize,
    height: usize,
    top_m: f32,
) -> Option<CrossSection> {
    crate::velocity_section(
        &convert::volume_with(volume, velocity_only),
        start_km,
        end_km,
        width,
        height,
        top_m,
    )
}

/// Per-legacy-volume memo of every tilt's dealiased velocity. See
/// [`crate::VolumeDealiasCache`].
///
/// The memo is keyed by the legacy volume's address; the converted FM301
/// volume it holds is what the sections sample.
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_map::VolumeDealiasCache")
)]
#[derive(Default)]
pub struct LegacyVolumeDealiasCache {
    volume_ptr: usize,
    converted: Option<Volume>,
    cache: crate::VolumeDealiasCache,
}

impl LegacyVolumeDealiasCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Convert `volume` unless the memo already holds it.
    fn ensure(&mut self, volume: &RadarVolume) {
        let ptr = volume as *const RadarVolume as usize;
        if ptr != self.volume_ptr || self.converted.is_none() {
            self.volume_ptr = ptr;
            self.converted = Some(convert::volume_with(volume, velocity_only));
            self.cache = crate::VolumeDealiasCache::new();
        }
    }
}

/// Velocity cross-section with a caller-held dealias memo. See
/// [`crate::velocity_section_cached`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_map::velocity_section_cached")
)]
pub fn velocity_cross_section_cached(
    volume: &RadarVolume,
    cache: &mut LegacyVolumeDealiasCache,
    start_km: (f32, f32),
    end_km: (f32, f32),
    width: usize,
    height: usize,
    top_m: f32,
) -> Option<CrossSection> {
    velocity_cross_section_cached_with_smoothing(
        volume,
        cache,
        start_km,
        end_km,
        width,
        height,
        top_m,
        CrossSectionSmoothing::Smoothed,
    )
}

/// Velocity cross-section with a memo and a smoothing choice. See
/// [`crate::velocity_section_cached_with_smoothing`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(
        note = "FM301 migration: use recast_radar_map::velocity_section_cached_with_smoothing"
    )
)]
#[allow(clippy::too_many_arguments)]
pub fn velocity_cross_section_cached_with_smoothing(
    volume: &RadarVolume,
    cache: &mut LegacyVolumeDealiasCache,
    start_km: (f32, f32),
    end_km: (f32, f32),
    width: usize,
    height: usize,
    top_m: f32,
    smoothing: CrossSectionSmoothing,
) -> Option<CrossSection> {
    cache.ensure(volume);
    let LegacyVolumeDealiasCache {
        converted, cache, ..
    } = cache;
    crate::velocity_section_cached_with_smoothing(
        converted.as_ref()?,
        cache,
        start_km,
        end_km,
        width,
        height,
        top_m,
        smoothing,
    )
}

/// RHI heuristic on a cut. See [`crate::sweep_looks_like_rhi`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_map::sweep_looks_like_rhi")
)]
pub fn cut_looks_like_rhi(cut: &ElevationCut) -> bool {
    crate::sweep_looks_like_rhi(&sweep_of(cut, None))
}

/// Circular-mean azimuth of a cut. See [`crate::rhi_fixed_azimuth`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_map::rhi_fixed_azimuth")
)]
pub fn rhi_fixed_azimuth_deg(cut: &ElevationCut) -> f32 {
    crate::rhi_fixed_azimuth(&sweep_of(cut, None))
}

/// Highest beam height of an RHI cut. See [`crate::rhi_coverage_top`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_map::rhi_coverage_top")
)]
pub fn rhi_coverage_top_m(cut: &ElevationCut, grid: &MomentGrid) -> f32 {
    let sweep = sweep_of(cut, Some(grid));
    crate::rhi_coverage_top(&sweep, &sweep.fields[0])
}

/// Furthest ground range of an RHI cut. See [`crate::rhi_coverage_range`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_map::rhi_coverage_range")
)]
pub fn rhi_coverage_range_m(cut: &ElevationCut, grid: &MomentGrid) -> f32 {
    let sweep = sweep_of(cut, Some(grid));
    crate::rhi_coverage_range(&sweep, &sweep.fields[0])
}

/// Native RHI panel. See [`crate::rhi_panel`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_map::rhi_panel")
)]
pub fn rhi_section(
    cut: &ElevationCut,
    grid: &MomentGrid,
    width: usize,
    height: usize,
    top_m: f32,
    max_range_m: f32,
) -> Option<CrossSection> {
    let sweep = sweep_of(cut, Some(grid));
    crate::rhi_panel(&sweep, &sweep.fields[0], width, height, top_m, max_range_m)
}

/// The cut's radials as rays (all of them without a grid; the grid's rows
/// with one, as the grid's only field).
fn sweep_of(cut: &ElevationCut, grid: Option<&MomentGrid>) -> Sweep {
    match grid {
        Some(grid) => convert::sweep_for_grid(cut, grid),
        None => convert::sweep_for_cut(
            cut,
            0,
            recast_radar_core::SweepMode::AzimuthSurveillance,
            &|_| false,
            &convert::field_name,
        ),
    }
}
