//! Radar product derivation and registry support.
//!
//! Every function works on the FM301 model of `recast-radar-core`
//! (`docs/design/fm301-model.md`): a [`Sweep`](recast_radar_core::Sweep)
//! supplies the rays and the range coordinate, its
//! [`Field`](recast_radar_core::Field)s the values in their native gate
//! geometry, a [`Volume`](recast_radar_core::Volume) the sweeps. Inputs are
//! found by [`Quantity`](recast_radar_core::Quantity), so the spelling of a
//! source's reflectivity (DBZH, DBZ, DBZHC) does not matter; outputs are
//! physical `F32` fields named per the design note's section 8.3.
//!
//! The engine has deliberately separate layers:
//! - `sweep` ([`derive_sweep_in_place`]) derives products that live on one
//!   sweep and can therefore be added to its fields.
//! - `volume` ([`cappi`], [`column_max`], ...) derives products that require
//!   the vertical column from multiple sweeps.
//! - temporal products (combining already co-registered fields from multiple
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
//! `availability` ([`sweep_can_materialize_field`]) sits alongside them and
//! answers the question a UI asks *before* deriving anything: which sweeps
//! can show a given field, counting both the fields a sweep already carries
//! and the ones the derivation could produce from them on demand.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod availability;
mod detect;
mod gbvtd;
mod shear;
mod sweep;
mod volume;
mod vwp;
pub mod wind;

pub use availability::{
    MIN_DISPLAYABLE_RADIALS, advanced_derived_product_for_name, displayable_radial_threshold,
    sweep_can_materialize_field, sweep_has_advanced_product_sources, sweep_has_field_source,
    volume_has_advanced_product_sources,
};
pub use detect::{
    RotationSite, RotationStrength, detect_rotation_sites, detect_rotation_sites_from_dealiased,
    rotation_features_per_tilt, rotation_features_per_tilt_from_dealiased,
    rotation_velocity_sweep_indices,
};
pub use gbvtd::{
    PolarVelocityField, RingFit, TcCirculation, find_center_and_retrieve, retrieve_axisymmetric,
};
pub use shear::{
    azimuthal_shear, azimuthal_shear_from_dealiased, radial_divergence,
    radial_divergence_from_dealiased,
};
pub use sweep::{
    AttenuationConfig, DerivationConfig, DerivationReport, DerivedSweepProduct, DiagnosticConfig,
    KdpConfig, MeteoMaskConfig, QpeConfig, RadarBand, SweepDerivationReport, TextureConfig,
    derive_product, derive_sweep_in_place, derive_volume_in_place,
};
pub use volume::{
    CappiInterpolation, cappi, column_max, column_mean, column_min, echo_base, echo_depth,
    echo_top_height, height_of_max_reflectivity, low_level_composite_reflectivity,
};
pub use vwp::{
    VwpCandidateDiagnostics, VwpConfig, VwpError, VwpLevel, VwpLevelOutcome, VwpProfile,
    VwpQuality, VwpRejectedLevel, VwpRejectionReason, VwpWindLevel, compute_vwp,
};
pub use wind::{gust_proxy, gust_proxy_from_dealiased, marc, marc_from_dealiased};

use recast_radar_core::FieldName;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductDescriptor {
    /// The dataset variable name the product is stored under (a volume or
    /// temporal product's id).
    pub id: FieldName,
    pub display_name: &'static str,
    pub source: ProductSource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProductSource {
    /// A native field, by its FM301 name.
    BaseField(FieldName),
    Derived,
}

pub fn base_products() -> Vec<ProductDescriptor> {
    [
        (FieldName::Dbzh, "Base Reflectivity"),
        (FieldName::Vradh, "Base Velocity"),
        (FieldName::Wradh, "Spectrum Width"),
        (FieldName::Zdr, "Differential Reflectivity"),
        (FieldName::Rhohv, "Correlation Coefficient"),
        (FieldName::Phidp, "Differential Phase"),
        (FieldName::Kdp, "Specific Differential Phase"),
    ]
    .into_iter()
    .map(|(name, display_name)| ProductDescriptor {
        id: name.clone(),
        display_name,
        source: ProductSource::BaseField(name),
    })
    .collect()
}

/// Sweep-local products implemented by this crate, under their canonical
/// names ([`DerivedSweepProduct::canonical_field_name`]).
pub fn derived_products() -> Vec<ProductDescriptor> {
    DerivedSweepProduct::ALL
        .iter()
        .copied()
        .map(|product| ProductDescriptor {
            id: product.canonical_field_name(),
            display_name: product.display_name(),
            source: ProductSource::Derived,
        })
        .collect()
}

/// Volume products provided by `recast_radar_map` (the first eight ids) and
/// by this crate ([`cappi`], [`column_max`], [`echo_base`], ...).
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
        id: FieldName::parse(id),
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
        id: FieldName::parse(id),
        display_name,
        source: ProductSource::Derived,
    })
    .collect()
}

/// Level II decoding for real-file tests.
#[cfg(test)]
pub(crate) mod test_decode {
    use std::path::Path;

    use recast_radar_core::Volume;

    pub(crate) fn decode_level2(path: &Path) -> Result<Volume, String> {
        recast_radar_io_nexrad::read_volume_from_path(path).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_includes_reflectivity_and_kdp() {
        let base = base_products();
        assert!(base.iter().any(|product| product.id == FieldName::Dbzh));
        assert!(base.iter().any(|product| product.id == FieldName::Kdp));
    }

    #[test]
    fn derived_ids_are_unique() {
        let mut ids = derived_products()
            .into_iter()
            .map(|product| product.id)
            .collect::<Vec<_>>();
        let original_len = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), original_len);
    }

    #[test]
    fn registry_includes_specific_differential_phase() {
        assert!(
            base_products()
                .iter()
                .any(|product| product.id == FieldName::Kdp)
        );
    }
}
