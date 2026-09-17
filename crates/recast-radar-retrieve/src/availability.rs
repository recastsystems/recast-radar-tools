//! Product availability: can this sweep show this field?
//!
//! Two distinct questions live here, and keeping them distinct matters:
//!
//! * [`sweep_has_field_source`] — is the field *already present* on the
//!   sweep, with enough rays to be worth drawing? This is the render-time
//!   gate.
//! * [`sweep_can_materialize_field`] — is the field present **or** could this
//!   crate derive it on demand from the fields the sweep does carry? This is
//!   the enumeration-time gate: it admits the sweeps a UI should offer, given
//!   that selecting one will run the derivation first.
//!
//! The predicates were previously private to the `bowecho` binary while the
//! browser/WASM toolbox kept a hand-transcribed copy. Both now call this
//! module, so the two can no longer drift.
//!
//! Note that [`DerivedSweepProduct::Kdp`] is deliberately *not* reachable
//! through the derive-on-demand arm: its output name is the native
//! [`FieldName::Kdp`], which [`advanced_derived_product_for_name`] never
//! maps to a product. KDP is therefore only ever admitted when the source
//! data actually carries it.

use crate::DerivedSweepProduct;
use recast_radar_core::{Field, FieldName, Quantity, Sweep, Volume};

/// Ray count a sweep must reach before a field on it is considered worth
/// drawing (a partial sweep is not a picture).
pub const MIN_DISPLAYABLE_RADIALS: usize = 180;

/// Ray-count floor for a sweep: a sweep that legitimately carries only a
/// handful of rays is not held to the full [`MIN_DISPLAYABLE_RADIALS`].
pub fn displayable_radial_threshold(sweep_rays: usize) -> usize {
    MIN_DISPLAYABLE_RADIALS.min((sweep_rays / 2).max(1))
}

/// Rays a field provides: its rows minus the absent ones.
fn provided_rows(field: &Field) -> usize {
    (field.nrays as usize).saturating_sub(field.absent_rows.len())
}

fn field_is_displayable(sweep: &Sweep, field: &Field) -> bool {
    provided_rows(field) >= displayable_radial_threshold(sweep.nrays())
}

/// True when `sweep` already carries a field named `name` with enough rays
/// to draw.
pub fn sweep_has_field_source(sweep: &Sweep, name: &FieldName) -> bool {
    sweep
        .field(name)
        .is_some_and(|field| field_is_displayable(sweep, field))
}

/// True when `sweep` carries a displayable field of `quantity`
/// ([`Sweep::find`]).
fn sweep_has_quantity_source(sweep: &Sweep, quantity: Quantity) -> bool {
    sweep
        .find(quantity)
        .is_some_and(|field| field_is_displayable(sweep, field))
}

fn sweep_has_kdp_source(sweep: &Sweep) -> bool {
    sweep_has_quantity_source(sweep, Quantity::SpecificDifferentialPhase)
        || sweep_has_quantity_source(sweep, Quantity::DifferentialPhase)
}

/// True when `sweep` carries the input fields [`crate::derive_sweep_in_place`]
/// needs to produce `product` on this sweep.
pub fn sweep_has_advanced_product_sources(sweep: &Sweep, product: DerivedSweepProduct) -> bool {
    use DerivedSweepProduct as Product;
    use Quantity as Q;
    let has = |quantity| sweep_has_quantity_source(sweep, quantity);
    match product {
        Product::Kdp => has(Q::DifferentialPhase),
        Product::FilteredDifferentialPhase | Product::KdpUncertainty => has(Q::DifferentialPhase),
        Product::SpecificAttenuation
        | Product::PathIntegratedAttenuation
        | Product::SpecificDifferentialAttenuation
        | Product::PathIntegratedDifferentialAttenuation
        | Product::RainRateKdp
        | Product::KdpTexture => sweep_has_kdp_source(sweep),
        Product::CorrectedReflectivity => has(Q::Reflectivity) && sweep_has_kdp_source(sweep),
        Product::CorrectedDifferentialReflectivity => {
            has(Q::DifferentialReflectivity) && sweep_has_kdp_source(sweep)
        }
        Product::RainRateReflectivity
        | Product::LiquidWaterContent
        | Product::HailKineticEnergy
        | Product::ReflectivityTexture
        | Product::ReflectivityRangeGradient => has(Q::Reflectivity),
        Product::RainRateHybrid => has(Q::Reflectivity) || sweep_has_kdp_source(sweep),
        Product::CircularDepolarizationRatio => {
            has(Q::DifferentialReflectivity) && has(Q::CorrelationCoefficient)
        }
        Product::LogCorrelationRatio | Product::CorrelationCoefficientTexture => {
            has(Q::CorrelationCoefficient)
        }
        Product::VelocityTexture | Product::VelocityRangeGradient => has(Q::RadialVelocity),
        Product::SpectrumWidthTexture => has(Q::SpectrumWidth),
        Product::DifferentialReflectivityTexture => has(Q::DifferentialReflectivity),
        Product::DifferentialPhaseTexture => has(Q::DifferentialPhase),
        Product::MeteorologicalQuality | Product::MeteorologicalGateMask => {
            has(Q::CorrelationCoefficient) || has(Q::Reflectivity)
        }
        Product::TdsConfidence => has(Q::Reflectivity) && has(Q::CorrelationCoefficient),
        Product::HailSignature => has(Q::Reflectivity),
        Product::TurbulenceProxy => has(Q::SpectrumWidth) || has(Q::RadialVelocity),
    }
}

/// True when any sweep in `volume` could produce `product`.
pub fn volume_has_advanced_product_sources(volume: &Volume, product: DerivedSweepProduct) -> bool {
    volume
        .sweeps
        .iter()
        .any(|sweep| sweep_has_advanced_product_sources(sweep, product))
}

/// The sweep product a dataset variable name refers to, if any
/// ([`DerivedSweepProduct::for_name`]). Native names, `KDP` included, give
/// `None`.
pub fn advanced_derived_product_for_name(name: &FieldName) -> Option<DerivedSweepProduct> {
    DerivedSweepProduct::for_name(name)
}

/// True when a field named `name` can be shown on `sweep` — either because
/// the sweep already carries it, or because this crate can derive it from
/// what the sweep carries.
///
/// This is the predicate a product/tilt picker should enumerate with. The
/// render path should use [`sweep_has_field_source`] instead, after the
/// derivation has actually run and added the field.
pub fn sweep_can_materialize_field(sweep: &Sweep, name: &FieldName) -> bool {
    sweep_has_field_source(sweep, name)
        || advanced_derived_product_for_name(name)
            .is_some_and(|product| sweep_has_advanced_product_sources(sweep, product))
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
    fn product_name_lookup_is_case_insensitive() {
        assert_eq!(
            advanced_derived_product_for_name(&FieldName::parse("dbzh_corr")),
            Some(DerivedSweepProduct::CorrectedReflectivity)
        );
    }
}
