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
    use crate::test_support::{add_f32_field, sweep_with_rows};

    fn sweep(rows: usize, names: &[FieldName]) -> Sweep {
        let mut sweep = sweep_with_rows(rows, 0.5, Some(25.0));
        for name in names {
            add_f32_field(
                &mut sweep,
                name.clone(),
                2125.0,
                250.0,
                8,
                vec![0.0; rows * 8],
            );
        }
        sweep
    }

    #[test]
    fn threshold_relaxes_for_short_sweeps() {
        assert_eq!(displayable_radial_threshold(720), MIN_DISPLAYABLE_RADIALS);
        assert_eq!(displayable_radial_threshold(40), 20);
        assert_eq!(displayable_radial_threshold(1), 1);
        assert_eq!(displayable_radial_threshold(0), 1);
    }

    #[test]
    fn derive_on_demand_admits_a_sweep_the_presence_gate_rejects() {
        // A dual-pol surveillance sweep: DBZH + PHIDP + RHOHV present, no
        // DBZH_CORR.
        let sweep = sweep(720, &[FieldName::Dbzh, FieldName::Phidp, FieldName::Rhohv]);
        let refc = DerivedSweepProduct::CorrectedReflectivity.field_name_in(&sweep);
        assert_eq!(refc, FieldName::parse("DBZH_CORR"));

        // Not there yet ...
        assert!(!sweep_has_field_source(&sweep, &refc));
        // ... but derivable, so a picker must offer this sweep.
        assert!(sweep_can_materialize_field(&sweep, &refc));
        assert!(sweep_has_advanced_product_sources(
            &sweep,
            DerivedSweepProduct::CorrectedReflectivity
        ));
    }

    #[test]
    fn derive_on_demand_never_admits_kdp() {
        // KDP's output name is the NATIVE `KDP`, so it can never reach the
        // derive-on-demand arm. A sweep carrying PHIDP could compute KDP, but
        // selecting "KDP" still requires real KDP.
        let sweep = sweep(720, &[FieldName::Phidp]);
        assert_eq!(
            DerivedSweepProduct::Kdp.field_name_in(&sweep),
            FieldName::Kdp
        );
        assert!(advanced_derived_product_for_name(&FieldName::Kdp).is_none());
        assert!(!sweep_can_materialize_field(&sweep, &FieldName::Kdp));
    }

    #[test]
    fn native_fields_route_straight_through_the_presence_gate() {
        let sweep = sweep(720, &[FieldName::Dbzh]);
        for name in [
            FieldName::Dbzh,
            FieldName::Vradh,
            FieldName::Wradh,
            FieldName::Zdr,
            FieldName::Rhohv,
            FieldName::Phidp,
            FieldName::Kdp,
        ] {
            assert_eq!(
                sweep_can_materialize_field(&sweep, &name),
                sweep_has_field_source(&sweep, &name),
                "{name:?} must not take the derive-on-demand arm"
            );
        }
    }

    #[test]
    fn a_partial_sweep_carries_no_sources() {
        // 720 rays declared, but the field only filled 10 rows.
        let mut sweep = sweep(720, &[]);
        add_f32_field(
            &mut sweep,
            FieldName::Dbzh,
            2125.0,
            250.0,
            8,
            vec![0.0; 10 * 8],
        );
        assert!(!sweep_has_field_source(&sweep, &FieldName::Dbzh));
        assert!(!sweep_has_advanced_product_sources(
            &sweep,
            DerivedSweepProduct::HailSignature
        ));
        // Sealing pads the field with absent rows; still not displayable.
        sweep.seal().unwrap();
        assert!(!sweep_has_field_source(&sweep, &FieldName::Dbzh));
    }

    #[test]
    fn unknown_names_that_match_nothing_are_not_derivable() {
        let sweep = sweep(720, &[FieldName::Dbzh]);
        let bogus = FieldName::parse("NOT_A_PRODUCT");
        assert!(advanced_derived_product_for_name(&bogus).is_none());
        assert!(!sweep_can_materialize_field(&sweep, &bogus));
    }

    #[test]
    fn product_name_lookup_is_case_insensitive() {
        assert_eq!(
            advanced_derived_product_for_name(&FieldName::parse("dbzh_corr")),
            Some(DerivedSweepProduct::CorrectedReflectivity)
        );
    }
}
