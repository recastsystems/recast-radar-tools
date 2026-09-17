//! Radar product derivation and registry support.
//!
//! The engine has deliberately separate layers:
//! - [`sweep`] derives products that live on one elevation cut and can therefore
//!   be inserted into `ElevationCut::moments`.
//! - [`volume`] derives products that require the vertical column from multiple
//!   elevation cuts.
//! - temporal products (combining already co-registered grids from multiple
//!   volumes) live in `recast_radar_track`.
//!
//! Keeping these layers separate prevents a volume product such as composite
//! reflectivity or VIL from being mislabeled as a native single-sweep moment.
//!
//! Velocity retrievals sit alongside them: azimuthal shear and radial
//! divergence, rotation (MDA-style) detection, GBVTD tropical-cyclone
//! circulation, the VAD wind profile ([`compute_vwp`]) and damaging-wind
//! products ([`wind`]).
//!
//! [`availability`] sits alongside them and answers the question a UI asks
//! *before* deriving anything: which elevation cuts can show a given moment,
//! counting both the moments a cut already carries and the ones [`sweep`]
//! could derive from them on demand.

mod availability;
mod detect;
mod gbvtd;
mod shear;
mod sweep;
mod volume;
mod vwp;
pub mod wind;

pub use availability::{
    MIN_DISPLAYABLE_RADIALS, advanced_derived_product_for_moment, cut_can_materialize_moment,
    cut_has_advanced_product_sources, cut_has_moment_source, displayable_radial_threshold,
    volume_has_advanced_product_sources,
};
pub use detect::{
    RotationSite, RotationStrength, detect_rotation_sites, detect_rotation_sites_from_dealiased,
    rotation_features_per_tilt, rotation_features_per_tilt_from_dealiased,
    rotation_velocity_cut_indices,
};
pub use gbvtd::{
    PolarVelocityField, RingFit, TcCirculation, find_center_and_retrieve, retrieve_axisymmetric,
};
pub use shear::{
    azimuthal_shear_grid, azimuthal_shear_grid_from_dealiased, radial_divergence_grid,
    radial_divergence_grid_from_dealiased,
};
pub use sweep::{
    AttenuationConfig, CutDerivationReport, DerivationConfig, DerivationReport,
    DerivedSweepProduct, DiagnosticConfig, KdpConfig, MeteoMaskConfig, QpeConfig, RadarBand,
    TextureConfig, derive_cut_in_place, derive_product, derive_volume_in_place,
};
pub use volume::{
    CappiInterpolation, cappi_grid, column_max_grid, column_mean_grid, column_min_grid,
    echo_base_grid, echo_depth_grid, echo_top_height_grid, height_of_max_reflectivity_grid,
    low_level_composite_reflectivity_grid,
};
pub use vwp::{
    VwpCandidateDiagnostics, VwpConfig, VwpError, VwpLevel, VwpLevelOutcome, VwpProfile,
    VwpQuality, VwpRejectedLevel, VwpRejectionReason, VwpWindLevel, compute_vwp,
};
pub use wind::{
    gust_proxy_grid, gust_proxy_grid_from_dealiased, marc_grid, marc_grid_from_dealiased,
};

use recast_radar_core::{MomentType, ProductId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductDescriptor {
    pub id: ProductId,
    pub display_name: &'static str,
    pub source: ProductSource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProductSource {
    BaseMoment(MomentType),
    Derived,
}

pub fn base_products() -> Vec<ProductDescriptor> {
    [
        (MomentType::Reflectivity, "Base Reflectivity"),
        (MomentType::Velocity, "Base Velocity"),
        (MomentType::SpectrumWidth, "Spectrum Width"),
        (
            MomentType::DifferentialReflectivity,
            "Differential Reflectivity",
        ),
        (
            MomentType::CorrelationCoefficient,
            "Correlation Coefficient",
        ),
        (MomentType::DifferentialPhase, "Differential Phase"),
        (
            MomentType::SpecificDifferentialPhase,
            "Specific Differential Phase",
        ),
    ]
    .into_iter()
    .map(|(moment, display_name)| ProductDescriptor {
        id: ProductId::from(moment.clone()),
        display_name,
        source: ProductSource::BaseMoment(moment),
    })
    .collect()
}

/// Sweep-local products implemented by this crate.
pub fn derived_products() -> Vec<ProductDescriptor> {
    DerivedSweepProduct::ALL
        .iter()
        .copied()
        .map(|product| ProductDescriptor {
            id: ProductId(product.id().to_owned()),
            display_name: product.display_name(),
            source: ProductSource::Derived,
        })
        .collect()
}

/// Volume products already implemented by BowEcho or provided by [`volume`].
///
/// The first eight IDs match the volume products in `recast_radar_map` (volumetric);
/// the remaining products are implemented in this crate.
pub fn volume_products() -> Vec<ProductDescriptor> {
    [
        ("CREF", "Composite Reflectivity"),
        ("ET", "Echo Tops"),
        ("VIL", "Vertically Integrated Liquid"),
        ("VILD", "VIL Density"),
        ("SHI", "Severe Hail Index"),
        ("MESH", "Maximum Estimated Size of Hail"),
        ("POSH", "Probability of Severe Hail"),
        ("POH", "Probability of Hail"),
        ("CAPPI", "Constant Altitude PPI"),
        ("LLCREF", "Low-Level Composite Reflectivity"),
        ("EBASE", "Echo Base"),
        ("EDEPTH", "Echo Depth"),
        ("HMAX", "Height of Maximum Reflectivity"),
        ("CMAX", "Column Maximum"),
        ("CMIN", "Column Minimum"),
        ("CMEAN", "Column Mean"),
    ]
    .into_iter()
    .map(|(id, display_name)| ProductDescriptor {
        id: ProductId(id.to_owned()),
        display_name,
        source: ProductSource::Derived,
    })
    .collect()
}

/// Temporal/grid-combination products implemented in `recast_radar_track`.
pub fn temporal_products() -> Vec<ProductDescriptor> {
    [
        ("DIFF", "Volume-to-Volume Difference"),
        ("TREND", "Volume-to-Volume Trend"),
        ("SWATH_MAX", "Maximum Swath"),
        ("SWATH_MIN", "Minimum Swath"),
        ("ACCUM", "Rate Accumulation"),
        ("DURATION", "Threshold Exceedance Duration"),
        ("PROB", "Threshold Exceedance Probability"),
    ]
    .into_iter()
    .map(|(id, display_name)| ProductDescriptor {
        id: ProductId(id.to_owned()),
        display_name,
        source: ProductSource::Derived,
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_includes_reflectivity_and_kdp() {
        let base = base_products();
        assert!(base.iter().any(|product| product.id.0 == "REF"));
        assert!(base.iter().any(|product| product.id.0 == "KDP"));
    }

    #[test]
    fn derived_ids_are_unique() {
        let mut ids = derived_products()
            .into_iter()
            .map(|product| product.id.0)
            .collect::<Vec<_>>();
        let original_len = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), original_len);
    }

    #[test]
    fn registry_includes_specific_differential_phase() {
        assert!(base_products().iter().any(|product| product.id.0 == "KDP"));
    }
}
