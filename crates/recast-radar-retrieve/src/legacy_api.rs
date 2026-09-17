//! Legacy-model signatures kept while other crates migrate to the FM301
//! model (`docs/design/fm301-model.md` section 13.3). Only this module names
//! legacy items; it is deleted with the shim.
//!
//! Every public function that took legacy types keeps its old name and
//! signature here. Where the FM301 function kept the legacy name
//! (`derive_volume_in_place`, `derive_product`,
//! `volume_has_advanced_product_sources`, `detect_rotation_sites*`,
//! `rotation_features_per_tilt*`, `compute_vwp`) it shadows the wrapper at
//! the crate root, and the legacy form is reachable as `legacy_api::<name>`
//! only; no un-migrated crate calls those. Each wrapper converts its legacy
//! input to the FM301 model with `recast_radar_correct::legacy_api::convert`,
//! calls the FM301 function and converts the result back, so both paths
//! share one implementation and produce identical values. The availability
//! predicates, which only count rows, keep their legacy form directly.
//!
//! Derived-product ids: a legacy `MomentType::Unknown("REF_TEX")` converts
//! to the product's canonical FM301 name (`DBZH_TEX`, design note 8.3) and
//! back, so `skipped_existing` and `overwrite_existing` behave as before.

#![allow(deprecated)]

use std::collections::HashSet;

use recast_radar_core::{
    ElevationCut, Field, FieldName, MomentGrid, MomentType, RadarVolume, Sweep, SweepMode, Volume,
};
use recast_radar_correct::legacy_api::convert;

use crate::{
    CappiInterpolation, DerivationConfig, DerivationReport, DerivedSweepProduct,
    PolarVelocityField, RotationSite, SweepDerivationReport, VwpConfig, VwpError, VwpProfile,
};

/// Legacy name of [`SweepDerivationReport`].
pub type CutDerivationReport = SweepDerivationReport;

/// Legacy moment <-> FM301 name mapping with the derived-product ids.
pub mod naming {
    use super::*;

    fn product_by_legacy_id(id: &str) -> Option<DerivedSweepProduct> {
        DerivedSweepProduct::ALL
            .iter()
            .copied()
            .find(|product| product.id() == id)
    }

    /// The field name of a legacy moment: a derived-product id becomes the
    /// product's canonical name, anything else follows the 5.4 table.
    pub fn field_name(moment: &MomentType) -> FieldName {
        match moment {
            MomentType::Unknown(id) => product_by_legacy_id(id)
                .map_or_else(|| convert::field_name(moment), |p| p.canonical_field_name()),
            _ => convert::field_name(moment),
        }
    }

    /// The legacy moment of a field name (the inverse of [`field_name`]).
    pub fn moment_of(name: &FieldName) -> MomentType {
        DerivedSweepProduct::ALL
            .iter()
            .copied()
            .find(|product| product.canonical_field_name() == *name)
            .map_or_else(|| convert::moment_of(name), |p| p.moment_type())
    }
}

fn all(_: &MomentType) -> bool {
    true
}

fn sweep_for_cut(cut: &ElevationCut) -> Sweep {
    convert::sweep_for_cut(
        cut,
        0,
        SweepMode::AzimuthSurveillance,
        &all,
        &naming::field_name,
    )
}

/// A converted volume with the moments `wanted` selects.
fn volume_with(volume: &RadarVolume, wanted: &dyn Fn(&MomentType) -> bool) -> Volume {
    convert::volume_with_naming(volume, wanted, &naming::field_name)
}

/// Caller-provided grids indexed like `volume.cuts` as detached fields of
/// the converted volume's sweeps (each on its sweep's rays and range).
fn detached_fields(converted: &mut Volume, grids: &[Option<&MomentGrid>]) -> Vec<Option<Field>> {
    // Two passes: attaching a geometry may refine or extend a sweep's range,
    // which remaps every field, so the fields are built once every geometry
    // is attached.
    let radials: Vec<Vec<usize>> = converted
        .sweeps
        .iter()
        .map(|sweep| (0..sweep.nrays()).collect())
        .collect();
    for (index, grid) in grids.iter().enumerate() {
        if let (Some(grid), Some(sweep)) = (grid, converted.sweeps.get_mut(index)) {
            let _ = convert::field_for_sweep(sweep, &radials[index], grid, &naming::field_name);
        }
    }
    grids
        .iter()
        .enumerate()
        .map(|(index, grid)| {
            let grid = (*grid)?;
            let sweep = converted.sweeps.get_mut(index)?;
            convert::field_for_sweep(sweep, &radials[index], grid, &naming::field_name)
        })
        .collect()
}

/// Lowest-elevation sweep of `volume` carrying a field named `name`.
fn base_sweep_index(volume: &Volume, name: &FieldName) -> Option<usize> {
    volume
        .sweeps
        .iter()
        .enumerate()
        .filter(|(_, sweep)| sweep.field(name).is_some())
        .min_by(|a, b| a.1.fixed_angle_deg.total_cmp(&b.1.fixed_angle_deg))
        .map(|(index, _)| index)
}

fn moment_is(moment: MomentType) -> impl Fn(&MomentType) -> bool {
    move |candidate| *candidate == moment
}

// ---------------------------------------------------------------- sweep --

impl DerivedSweepProduct {
    /// The legacy moment key of the product: the native
    /// `SpecificDifferentialPhase` for KDP, `Unknown(id)` otherwise.
    #[cfg_attr(
        recast_legacy_deprecation,
        deprecated(note = "FM301 migration: use DerivedSweepProduct::field_name_in")
    )]
    pub fn moment_type(self) -> MomentType {
        if self == Self::Kdp {
            MomentType::SpecificDifferentialPhase
        } else {
            MomentType::Unknown(self.id().to_owned())
        }
    }
}

/// Derive configured products for one cut and insert them into its moment
/// map. See [`crate::derive_sweep_in_place`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_retrieve::derive_sweep_in_place")
)]
pub fn derive_cut_in_place(
    cut: &mut ElevationCut,
    config: &DerivationConfig,
) -> CutDerivationReport {
    let mut sweep = sweep_for_cut(cut);
    let report = crate::derive_sweep_in_place(&mut sweep, config);
    let inserted: HashSet<&str> = report.inserted.iter().map(String::as_str).collect();
    let outputs: Vec<(FieldName, MomentType)> = DerivedSweepProduct::ALL
        .iter()
        .filter(|product| inserted.contains(product.id()))
        .map(|product| (product.field_name_in(&sweep), product.moment_type()))
        .collect();
    for (name, moment) in outputs {
        if let Some(index) = sweep.field_index(&name) {
            let field = sweep.fields.remove(index);
            let grid = convert::grid_from_field(field, moment.clone(), &sweep);
            cut.moments.insert(moment, grid);
        }
    }
    report
}

/// Derive configured products on every cut. See
/// [`crate::derive_volume_in_place`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_retrieve::derive_volume_in_place")
)]
pub fn derive_volume_in_place(
    volume: &mut RadarVolume,
    config: &DerivationConfig,
) -> DerivationReport {
    let mut report = DerivationReport::default();
    for (cut_index, cut) in volume.cuts.iter_mut().enumerate() {
        let cut_report = derive_cut_in_place(cut, config);
        report.sweeps_processed += 1;
        report
            .inserted
            .extend(cut_report.inserted.into_iter().map(|id| (cut_index, id)));
        report.skipped_existing.extend(
            cut_report
                .skipped_existing
                .into_iter()
                .map(|id| (cut_index, id)),
        );
        report
            .unavailable
            .extend(cut_report.unavailable.into_iter().map(|id| (cut_index, id)));
    }
    report
}

/// Derive one product without mutating the caller's cut. See
/// [`crate::derive_product`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_retrieve::derive_product")
)]
pub fn derive_product(
    cut: &ElevationCut,
    product: DerivedSweepProduct,
    config: &DerivationConfig,
) -> Option<MomentGrid> {
    let sweep = sweep_for_cut(cut);
    let field = crate::derive_product(&sweep, product, config)?;
    Some(convert::grid_from_field(
        field,
        product.moment_type(),
        &sweep,
    ))
}

// --------------------------------------------------------- availability --

/// True when `cut` already carries `moment` with enough radials to draw.
/// See [`crate::sweep_has_field_source`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_retrieve::sweep_has_field_source")
)]
pub fn cut_has_moment_source(cut: &ElevationCut, moment: &MomentType) -> bool {
    cut.moments.get(moment).is_some_and(|grid| {
        grid.radial_count() >= crate::displayable_radial_threshold(cut.radials.len())
    })
}

fn cut_has_kdp_source(cut: &ElevationCut) -> bool {
    cut_has_moment_source(cut, &MomentType::SpecificDifferentialPhase)
        || cut_has_moment_source(cut, &MomentType::DifferentialPhase)
}

/// True when `cut` carries the input moments `derive_cut_in_place` needs to
/// produce `product`. See [`crate::sweep_has_advanced_product_sources`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(
        note = "FM301 migration: use recast_radar_retrieve::sweep_has_advanced_product_sources"
    )
)]
pub fn cut_has_advanced_product_sources(cut: &ElevationCut, product: DerivedSweepProduct) -> bool {
    use DerivedSweepProduct as Product;
    let has = |moment: MomentType| cut_has_moment_source(cut, &moment);
    match product {
        Product::Kdp => has(MomentType::DifferentialPhase),
        Product::FilteredDifferentialPhase | Product::KdpUncertainty => {
            has(MomentType::DifferentialPhase)
        }
        Product::SpecificAttenuation
        | Product::PathIntegratedAttenuation
        | Product::SpecificDifferentialAttenuation
        | Product::PathIntegratedDifferentialAttenuation
        | Product::RainRateKdp
        | Product::KdpTexture => cut_has_kdp_source(cut),
        Product::CorrectedReflectivity => has(MomentType::Reflectivity) && cut_has_kdp_source(cut),
        Product::CorrectedDifferentialReflectivity => {
            has(MomentType::DifferentialReflectivity) && cut_has_kdp_source(cut)
        }
        Product::RainRateReflectivity
        | Product::LiquidWaterContent
        | Product::HailKineticEnergy
        | Product::ReflectivityTexture
        | Product::ReflectivityRangeGradient => has(MomentType::Reflectivity),
        Product::RainRateHybrid => has(MomentType::Reflectivity) || cut_has_kdp_source(cut),
        Product::CircularDepolarizationRatio => {
            has(MomentType::DifferentialReflectivity) && has(MomentType::CorrelationCoefficient)
        }
        Product::LogCorrelationRatio | Product::CorrelationCoefficientTexture => {
            has(MomentType::CorrelationCoefficient)
        }
        Product::VelocityTexture | Product::VelocityRangeGradient => has(MomentType::Velocity),
        Product::SpectrumWidthTexture => has(MomentType::SpectrumWidth),
        Product::DifferentialReflectivityTexture => has(MomentType::DifferentialReflectivity),
        Product::DifferentialPhaseTexture => has(MomentType::DifferentialPhase),
        Product::MeteorologicalQuality | Product::MeteorologicalGateMask => {
            has(MomentType::CorrelationCoefficient) || has(MomentType::Reflectivity)
        }
        Product::TdsConfidence => {
            has(MomentType::Reflectivity) && has(MomentType::CorrelationCoefficient)
        }
        Product::HailSignature => has(MomentType::Reflectivity),
        Product::TurbulenceProxy => has(MomentType::SpectrumWidth) || has(MomentType::Velocity),
    }
}

/// True when any cut in `volume` could produce `product`. See
/// [`crate::volume_has_advanced_product_sources`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(
        note = "FM301 migration: use recast_radar_retrieve::volume_has_advanced_product_sources"
    )
)]
pub fn volume_has_advanced_product_sources(
    volume: &RadarVolume,
    product: DerivedSweepProduct,
) -> bool {
    volume
        .cuts
        .iter()
        .any(|cut| cut_has_advanced_product_sources(cut, product))
}

/// The sweep product a non-native moment name refers to, if any. See
/// [`crate::advanced_derived_product_for_name`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(
        note = "FM301 migration: use recast_radar_retrieve::advanced_derived_product_for_name"
    )
)]
pub fn advanced_derived_product_for_moment(moment: &MomentType) -> Option<DerivedSweepProduct> {
    let MomentType::Unknown(name) = moment else {
        return None;
    };
    DerivedSweepProduct::ALL
        .iter()
        .copied()
        .find(|product| product.id().eq_ignore_ascii_case(name))
}

/// True when `moment` can be shown on `cut`. See
/// [`crate::sweep_can_materialize_field`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_retrieve::sweep_can_materialize_field")
)]
pub fn cut_can_materialize_moment(cut: &ElevationCut, moment: &MomentType) -> bool {
    cut_has_moment_source(cut, moment)
        || advanced_derived_product_for_moment(moment)
            .is_some_and(|product| cut_has_advanced_product_sources(cut, product))
}

// --------------------------------------------------------------- detect --

fn detector_moments(moment: &MomentType) -> bool {
    matches!(
        moment,
        MomentType::Velocity | MomentType::Reflectivity | MomentType::CorrelationCoefficient
    )
}

/// Ordered, bounded cut set consumed by rotation detection. See
/// [`crate::rotation_velocity_sweep_indices`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(
        note = "FM301 migration: use recast_radar_retrieve::rotation_velocity_sweep_indices"
    )
)]
pub fn rotation_velocity_cut_indices(volume: &RadarVolume) -> Vec<usize> {
    crate::rotation_velocity_sweep_indices(&volume_with(volume, &moment_is(MomentType::Velocity)))
}

/// Detect vertically-continuous rotation in a volume. See
/// [`crate::detect_rotation_sites`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_retrieve::detect_rotation_sites")
)]
pub fn detect_rotation_sites(volume: &RadarVolume) -> Vec<RotationSite> {
    crate::detect_rotation_sites(&volume_with(volume, &detector_moments))
}

/// Detect rotation from caller-provided dealiased grids indexed like
/// `volume.cuts`. See [`crate::detect_rotation_sites_from_dealiased`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(
        note = "FM301 migration: use recast_radar_retrieve::detect_rotation_sites_from_dealiased"
    )
)]
pub fn detect_rotation_sites_from_dealiased(
    volume: &RadarVolume,
    dealiased_velocity: &[Option<&MomentGrid>],
) -> Vec<RotationSite> {
    let mut converted = volume_with(volume, &detector_moments);
    let fields = detached_fields(&mut converted, dealiased_velocity);
    let borrowed: Vec<Option<&Field>> = fields.iter().map(Option::as_ref).collect();
    crate::detect_rotation_sites_from_dealiased(&converted, &borrowed)
}

/// Per-tilt rotation diagnostics. See [`crate::rotation_features_per_tilt`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_retrieve::rotation_features_per_tilt")
)]
pub fn rotation_features_per_tilt(volume: &RadarVolume) -> Vec<(f32, usize, u8)> {
    crate::rotation_features_per_tilt(&volume_with(volume, &detector_moments))
}

/// Per-tilt rotation diagnostics from caller-provided dealiased grids. See
/// [`crate::rotation_features_per_tilt_from_dealiased`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(
        note = "FM301 migration: use recast_radar_retrieve::rotation_features_per_tilt_from_dealiased"
    )
)]
pub fn rotation_features_per_tilt_from_dealiased(
    volume: &RadarVolume,
    dealiased_velocity: &[Option<&MomentGrid>],
) -> Vec<(f32, usize, u8)> {
    let mut converted = volume_with(volume, &detector_moments);
    let fields = detached_fields(&mut converted, dealiased_velocity);
    let borrowed: Vec<Option<&Field>> = fields.iter().map(Option::as_ref).collect();
    crate::rotation_features_per_tilt_from_dealiased(&converted, &borrowed)
}

// ---------------------------------------------------------------- gbvtd --

impl PolarVelocityField {
    /// Build the polar field from a dealiased velocity moment grid. See
    /// [`PolarVelocityField::from_dealiased_velocity`].
    #[cfg_attr(
        recast_legacy_deprecation,
        deprecated(note = "FM301 migration: use PolarVelocityField::from_dealiased_velocity")
    )]
    pub fn from_dealiased_velocity_grid(cut: &ElevationCut, grid: &MomentGrid) -> Self {
        let sweep = convert::sweep_for_grid(cut, grid);
        Self::from_dealiased_velocity(&sweep, &sweep.fields[0])
    }
}

// ---------------------------------------------------------------- shear --

fn shear_grid(
    cut: &ElevationCut,
    velocity: &MomentGrid,
    derive: impl Fn(&Sweep, &Field) -> Field,
) -> MomentGrid {
    let sweep = convert::sweep_for_grid(cut, velocity);
    let field = derive(&sweep, &sweep.fields[0]);
    convert::grid_like(field, MomentType::Velocity, velocity, None)
}

/// Azimuthal shear of a raw velocity grid. See [`crate::azimuthal_shear`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_retrieve::azimuthal_shear")
)]
pub fn azimuthal_shear_grid(cut: &ElevationCut, velocity: &MomentGrid) -> MomentGrid {
    shear_grid(cut, velocity, crate::azimuthal_shear)
}

/// Azimuthal shear of a dealiased velocity grid. See
/// [`crate::azimuthal_shear_from_dealiased`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(
        note = "FM301 migration: use recast_radar_retrieve::azimuthal_shear_from_dealiased"
    )
)]
pub fn azimuthal_shear_grid_from_dealiased(
    cut: &ElevationCut,
    dealiased_velocity: &MomentGrid,
) -> MomentGrid {
    shear_grid(
        cut,
        dealiased_velocity,
        crate::azimuthal_shear_from_dealiased,
    )
}

/// Radial divergence of a raw velocity grid. See [`crate::radial_divergence`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_retrieve::radial_divergence")
)]
pub fn radial_divergence_grid(cut: &ElevationCut, velocity: &MomentGrid) -> MomentGrid {
    shear_grid(cut, velocity, crate::radial_divergence)
}

/// Radial divergence of a dealiased velocity grid. See
/// [`crate::radial_divergence_from_dealiased`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(
        note = "FM301 migration: use recast_radar_retrieve::radial_divergence_from_dealiased"
    )
)]
pub fn radial_divergence_grid_from_dealiased(
    cut: &ElevationCut,
    dealiased_velocity: &MomentGrid,
) -> MomentGrid {
    shear_grid(
        cut,
        dealiased_velocity,
        crate::radial_divergence_from_dealiased,
    )
}

// --------------------------------------------------------------- volume --

/// A volume product of the moment `moment` converted back to a legacy grid
/// keyed `output`.
fn volume_product(
    volume: &RadarVolume,
    moment: &MomentType,
    output: MomentType,
    product: impl Fn(&Volume, &FieldName) -> Option<Field>,
) -> Option<MomentGrid> {
    let converted = volume_with(volume, &moment_is(moment.clone()));
    let name = naming::field_name(moment);
    let field = product(&converted, &name)?;
    let base = base_sweep_index(&converted, &name)?;
    Some(convert::grid_from_field(
        field,
        output,
        &converted.sweeps[base],
    ))
}

/// Constant-altitude PPI. See [`crate::cappi`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_retrieve::cappi")
)]
pub fn cappi_grid(
    volume: &RadarVolume,
    moment: MomentType,
    height_m: f32,
    interpolation: CappiInterpolation,
) -> Option<MomentGrid> {
    let id = format!("CAPPI_{}_{:.1}KM", moment.short_name(), height_m / 1000.0);
    volume_product(
        volume,
        &moment,
        MomentType::Unknown(id),
        |converted, name| crate::cappi(converted, name, height_m, interpolation),
    )
}

/// Column maximum. See [`crate::column_max`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_retrieve::column_max")
)]
pub fn column_max_grid(volume: &RadarVolume, moment: MomentType) -> Option<MomentGrid> {
    let id = format!("CMAX_{}", moment.short_name());
    volume_product(volume, &moment, MomentType::Unknown(id), crate::column_max)
}

/// Column minimum. See [`crate::column_min`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_retrieve::column_min")
)]
pub fn column_min_grid(volume: &RadarVolume, moment: MomentType) -> Option<MomentGrid> {
    let id = format!("CMIN_{}", moment.short_name());
    volume_product(volume, &moment, MomentType::Unknown(id), crate::column_min)
}

/// Column mean. See [`crate::column_mean`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_retrieve::column_mean")
)]
pub fn column_mean_grid(volume: &RadarVolume, moment: MomentType) -> Option<MomentGrid> {
    let id = format!("CMEAN_{}", moment.short_name());
    volume_product(volume, &moment, MomentType::Unknown(id), crate::column_mean)
}

fn reflectivity_product(
    volume: &RadarVolume,
    id: &str,
    product: impl Fn(&Volume) -> Option<Field>,
) -> Option<MomentGrid> {
    volume_product(
        volume,
        &MomentType::Reflectivity,
        MomentType::Unknown(id.to_owned()),
        |converted, _| product(converted),
    )
}

/// Low-level composite reflectivity. See
/// [`crate::low_level_composite_reflectivity`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(
        note = "FM301 migration: use recast_radar_retrieve::low_level_composite_reflectivity"
    )
)]
pub fn low_level_composite_reflectivity_grid(
    volume: &RadarVolume,
    maximum_height_m: f32,
) -> Option<MomentGrid> {
    reflectivity_product(volume, "LLCREF", |converted| {
        crate::low_level_composite_reflectivity(converted, maximum_height_m)
    })
}

/// Echo base. See [`crate::echo_base`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_retrieve::echo_base")
)]
pub fn echo_base_grid(volume: &RadarVolume, threshold_dbz: f32) -> Option<MomentGrid> {
    reflectivity_product(volume, "EBASE", |converted| {
        crate::echo_base(converted, threshold_dbz)
    })
}

/// Echo top height. See [`crate::echo_top_height`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_retrieve::echo_top_height")
)]
pub fn echo_top_height_grid(volume: &RadarVolume, threshold_dbz: f32) -> Option<MomentGrid> {
    reflectivity_product(volume, "ET", |converted| {
        crate::echo_top_height(converted, threshold_dbz)
    })
}

/// Echo depth. See [`crate::echo_depth`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_retrieve::echo_depth")
)]
pub fn echo_depth_grid(volume: &RadarVolume, threshold_dbz: f32) -> Option<MomentGrid> {
    reflectivity_product(volume, "EDEPTH", |converted| {
        crate::echo_depth(converted, threshold_dbz)
    })
}

/// Height of maximum reflectivity. See [`crate::height_of_max_reflectivity`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_retrieve::height_of_max_reflectivity")
)]
pub fn height_of_max_reflectivity_grid(volume: &RadarVolume) -> Option<MomentGrid> {
    reflectivity_product(volume, "HMAX", crate::height_of_max_reflectivity)
}

// ------------------------------------------------------------------ vwp --

/// Compute a wind profile from caller-dealiased velocity grids indexed like
/// `volume.cuts`. See [`crate::compute_vwp`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_retrieve::compute_vwp")
)]
pub fn compute_vwp(
    volume: &RadarVolume,
    dealiased_velocity: &[Option<&MomentGrid>],
    config: VwpConfig,
) -> Result<VwpProfile, VwpError> {
    let mut converted = volume_with(volume, &|_| false);
    let fields = if dealiased_velocity.len() == converted.sweeps.len() {
        detached_fields(&mut converted, dealiased_velocity)
    } else {
        vec![None; dealiased_velocity.len()]
    };
    let borrowed: Vec<Option<&Field>> = fields.iter().map(Option::as_ref).collect();
    crate::compute_vwp(&converted, &borrowed, config)
}

// ----------------------------------------------------------------- wind --

fn wind_moments(moment: &MomentType) -> bool {
    matches!(moment, MomentType::Velocity | MomentType::Reflectivity)
}

/// The first sweep (in order) with a velocity field.
fn first_velocity_sweep(volume: &Volume) -> Option<usize> {
    volume
        .sweeps
        .iter()
        .position(|sweep| sweep.field(&FieldName::Vradh).is_some())
}

/// MARC composite. See [`crate::marc`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_retrieve::marc")
)]
pub fn marc_grid(volume: &RadarVolume) -> Option<MomentGrid> {
    let converted = volume_with(volume, &moment_is(MomentType::Velocity));
    let field = crate::marc(&converted)?;
    let base = first_velocity_sweep(&converted)?;
    Some(convert::grid_from_field(
        field,
        MomentType::Velocity,
        &converted.sweeps[base],
    ))
}

/// MARC composite from caller-provided dealiased grids indexed like
/// `volume.cuts`. See [`crate::marc_from_dealiased`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_retrieve::marc_from_dealiased")
)]
pub fn marc_grid_from_dealiased(
    volume: &RadarVolume,
    dealiased_velocity: &[Option<&MomentGrid>],
) -> Option<MomentGrid> {
    let mut converted = volume_with(volume, &moment_is(MomentType::Velocity));
    let fields = detached_fields(&mut converted, dealiased_velocity);
    let borrowed: Vec<Option<&Field>> = fields.iter().map(Option::as_ref).collect();
    let field = crate::marc_from_dealiased(&converted, &borrowed)?;
    let base = first_velocity_sweep(&converted)?;
    let template = dealiased_velocity.get(base).copied().flatten()?;
    Some(convert::grid_like(
        field,
        MomentType::Velocity,
        template,
        Some(&converted.sweeps[base]),
    ))
}

/// Low-level gust proxy. See [`crate::gust_proxy`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_retrieve::gust_proxy")
)]
pub fn gust_proxy_grid(volume: &RadarVolume) -> Option<MomentGrid> {
    let converted = volume_with(volume, &wind_moments);
    let field = crate::gust_proxy(&converted)?;
    let base = first_velocity_sweep(&converted)?;
    Some(convert::grid_from_field(
        field,
        MomentType::Velocity,
        &converted.sweeps[base],
    ))
}

/// Low-level gust proxy from one caller-provided dealiased grid of cut
/// `cut_index`. See [`crate::gust_proxy_from_dealiased`].
#[cfg_attr(
    recast_legacy_deprecation,
    deprecated(note = "FM301 migration: use recast_radar_retrieve::gust_proxy_from_dealiased")
)]
pub fn gust_proxy_grid_from_dealiased(
    volume: &RadarVolume,
    cut_index: usize,
    dealiased: &MomentGrid,
) -> Option<MomentGrid> {
    let mut converted = volume_with(volume, &wind_moments);
    let mut grids = vec![None; converted.sweeps.len()];
    *grids.get_mut(cut_index)? = Some(dealiased);
    let fields = detached_fields(&mut converted, &grids);
    let field = fields.get(cut_index)?.as_ref()?;
    let output = crate::gust_proxy_from_dealiased(&converted, cut_index, field)?;
    Some(convert::grid_like(
        output,
        MomentType::Velocity,
        dealiased,
        Some(&converted.sweeps[cut_index]),
    ))
}
