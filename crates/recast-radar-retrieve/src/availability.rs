//! Product availability: can this elevation cut show this moment?
//!
//! Two distinct questions live here, and keeping them distinct matters:
//!
//! * [`cut_has_moment_source`] — is the moment *already present* on the cut,
//!   with enough radials to be worth drawing? This is the render-time gate.
//! * [`cut_can_materialize_moment`] — is the moment present **or** could this
//!   crate derive it on demand from the moments the cut does carry? This is the
//!   enumeration-time gate: it admits the tilts a UI should offer, given that
//!   selecting one will run the derivation first.
//!
//! The predicates were previously private to the `bowecho` binary while the
//! browser/WASM toolbox kept a hand-transcribed copy. Both now call this
//! module, so the two can no longer drift.
//!
//! Note that [`DerivedSweepProduct::Kdp`] is deliberately *not* reachable
//! through the derive-on-demand arm: its [`DerivedSweepProduct::moment_type`]
//! is the native [`MomentType::SpecificDifferentialPhase`], not an
//! `Unknown` id, and [`advanced_derived_product_for_moment`] only matches
//! `Unknown`. KDP is therefore only ever admitted when the source data
//! actually carries it.

use crate::DerivedSweepProduct;
use recast_radar_core::{ElevationCut, MomentType, RadarVolume};

/// Radial count an elevation cut must reach before a moment on it is
/// considered worth drawing (a partial sweep is not a picture).
pub const MIN_DISPLAYABLE_RADIALS: usize = 180;

/// Radial-count floor for a cut: a sweep that legitimately carries only a
/// handful of radials is not held to the full [`MIN_DISPLAYABLE_RADIALS`].
pub fn displayable_radial_threshold(cut_radials: usize) -> usize {
    MIN_DISPLAYABLE_RADIALS.min((cut_radials / 2).max(1))
}

/// True when `cut` already carries `moment` with enough radials to draw.
pub fn cut_has_moment_source(cut: &ElevationCut, moment: &MomentType) -> bool {
    cut.moments
        .get(moment)
        .is_some_and(|grid| grid.radial_count() >= displayable_radial_threshold(cut.radials.len()))
}

fn cut_has_kdp_source(cut: &ElevationCut) -> bool {
    cut_has_moment_source(cut, &MomentType::SpecificDifferentialPhase)
        || cut_has_moment_source(cut, &MomentType::DifferentialPhase)
}

/// True when `cut` carries the input moments [`crate::derive_cut_in_place`]
/// needs to produce `product` on this sweep.
pub fn cut_has_advanced_product_sources(cut: &ElevationCut, product: DerivedSweepProduct) -> bool {
    use DerivedSweepProduct as Product;
    match product {
        Product::Kdp => cut_has_moment_source(cut, &MomentType::DifferentialPhase),
        Product::FilteredDifferentialPhase | Product::KdpUncertainty => {
            cut_has_moment_source(cut, &MomentType::DifferentialPhase)
        }
        Product::SpecificAttenuation
        | Product::PathIntegratedAttenuation
        | Product::SpecificDifferentialAttenuation
        | Product::PathIntegratedDifferentialAttenuation
        | Product::RainRateKdp
        | Product::KdpTexture => cut_has_kdp_source(cut),
        Product::CorrectedReflectivity => {
            cut_has_moment_source(cut, &MomentType::Reflectivity) && cut_has_kdp_source(cut)
        }
        Product::CorrectedDifferentialReflectivity => {
            cut_has_moment_source(cut, &MomentType::DifferentialReflectivity)
                && cut_has_kdp_source(cut)
        }
        Product::RainRateReflectivity
        | Product::LiquidWaterContent
        | Product::HailKineticEnergy
        | Product::ReflectivityTexture
        | Product::ReflectivityRangeGradient => {
            cut_has_moment_source(cut, &MomentType::Reflectivity)
        }
        Product::RainRateHybrid => {
            cut_has_moment_source(cut, &MomentType::Reflectivity) || cut_has_kdp_source(cut)
        }
        Product::CircularDepolarizationRatio => {
            cut_has_moment_source(cut, &MomentType::DifferentialReflectivity)
                && cut_has_moment_source(cut, &MomentType::CorrelationCoefficient)
        }
        Product::LogCorrelationRatio | Product::CorrelationCoefficientTexture => {
            cut_has_moment_source(cut, &MomentType::CorrelationCoefficient)
        }
        Product::VelocityTexture | Product::VelocityRangeGradient => {
            cut_has_moment_source(cut, &MomentType::Velocity)
        }
        Product::SpectrumWidthTexture => cut_has_moment_source(cut, &MomentType::SpectrumWidth),
        Product::DifferentialReflectivityTexture => {
            cut_has_moment_source(cut, &MomentType::DifferentialReflectivity)
        }
        Product::DifferentialPhaseTexture => {
            cut_has_moment_source(cut, &MomentType::DifferentialPhase)
        }
        Product::MeteorologicalQuality | Product::MeteorologicalGateMask => {
            cut_has_moment_source(cut, &MomentType::CorrelationCoefficient)
                || cut_has_moment_source(cut, &MomentType::Reflectivity)
        }
        Product::TdsConfidence => {
            cut_has_moment_source(cut, &MomentType::Reflectivity)
                && cut_has_moment_source(cut, &MomentType::CorrelationCoefficient)
        }
        Product::HailSignature => cut_has_moment_source(cut, &MomentType::Reflectivity),
        Product::TurbulenceProxy => {
            cut_has_moment_source(cut, &MomentType::SpectrumWidth)
                || cut_has_moment_source(cut, &MomentType::Velocity)
        }
    }
}

/// True when any cut in `volume` could produce `product`.
pub fn volume_has_advanced_product_sources(
    volume: &RadarVolume,
    product: DerivedSweepProduct,
) -> bool {
    volume
        .cuts
        .iter()
        .any(|cut| cut_has_advanced_product_sources(cut, product))
}

/// The sweep product a non-native moment name refers to, if any.
///
/// Returns `None` for every native [`MomentType`]: only
/// [`MomentType::Unknown`] carries a derived-product id.
pub fn advanced_derived_product_for_moment(moment: &MomentType) -> Option<DerivedSweepProduct> {
    let MomentType::Unknown(name) = moment else {
        return None;
    };
    DerivedSweepProduct::ALL
        .iter()
        .copied()
        .find(|product| product.id().eq_ignore_ascii_case(name))
}

/// True when `moment` can be shown on `cut` — either because the cut already
/// carries it, or because this crate can derive it from what the cut carries.
///
/// This is the predicate a product/tilt picker should enumerate with. The
/// render path should use [`cut_has_moment_source`] instead, after the
/// derivation has actually run and inserted the moment.
pub fn cut_can_materialize_moment(cut: &ElevationCut, moment: &MomentType) -> bool {
    cut_has_moment_source(cut, moment)
        || advanced_derived_product_for_moment(moment)
            .is_some_and(|product| cut_has_advanced_product_sources(cut, product))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_relaxes_for_short_sweeps() {
        assert_eq!(displayable_radial_threshold(720), MIN_DISPLAYABLE_RADIALS);
        assert_eq!(displayable_radial_threshold(40), 20);
        assert_eq!(displayable_radial_threshold(1), 1);
        assert_eq!(displayable_radial_threshold(0), 1);
    }

    #[test]
    fn product_id_lookup_is_case_insensitive() {
        assert_eq!(
            advanced_derived_product_for_moment(&MomentType::Unknown("refc".to_owned())),
            Some(DerivedSweepProduct::CorrectedReflectivity)
        );
    }
}
