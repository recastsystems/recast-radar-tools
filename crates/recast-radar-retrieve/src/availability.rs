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
    use recast_radar_core::{GateRange, MomentGrid, MomentStorage, Radial};

    fn gate_range(gates: usize) -> GateRange {
        GateRange {
            first_gate_m: 2125,
            gate_spacing_m: 250,
            gate_count: gates,
        }
    }

    fn grid(moment: MomentType, rows: usize, gates: usize) -> MomentGrid {
        MomentGrid {
            moment,
            gate_range: gate_range(gates),
            scale: 1.0,
            offset: 0.0,
            nodata: None,
            range_folded: None,
            radial_indices: (0..rows).collect(),
            storage: MomentStorage::F32(vec![0.0; rows * gates]),
        }
    }

    fn cut(rows: usize, moments: &[MomentType]) -> ElevationCut {
        let mut cut = ElevationCut::new(0.5, Some(1));
        for row in 0..rows {
            cut.radials.push(Radial {
                azimuth_deg: row as f32 * 360.0 / rows.max(1) as f32,
                elevation_deg: 0.5,
                time_offset_ms: row as i32,
                gate_range: gate_range(8),
                nyquist_velocity_mps: Some(25.0),
                radial_status: None,
            });
        }
        for moment in moments {
            cut.moments
                .insert(moment.clone(), grid(moment.clone(), rows, 8));
        }
        cut
    }

    #[test]
    fn threshold_relaxes_for_short_sweeps() {
        assert_eq!(displayable_radial_threshold(720), MIN_DISPLAYABLE_RADIALS);
        assert_eq!(displayable_radial_threshold(40), 20);
        assert_eq!(displayable_radial_threshold(1), 1);
        assert_eq!(displayable_radial_threshold(0), 1);
    }

    #[test]
    fn derive_on_demand_admits_a_cut_the_presence_gate_rejects() {
        // A dual-pol surveillance sweep: REF + PHI + RHO present, no REFC.
        let sweep = cut(
            720,
            &[
                MomentType::Reflectivity,
                MomentType::DifferentialPhase,
                MomentType::CorrelationCoefficient,
            ],
        );
        let refc = MomentType::Unknown(DerivedSweepProduct::CorrectedReflectivity.id().to_owned());
        assert_eq!(refc, MomentType::Unknown("REFC".to_owned()));

        // Not there yet ...
        assert!(!cut_has_moment_source(&sweep, &refc));
        // ... but derivable, so a picker must offer this tilt.
        assert!(cut_can_materialize_moment(&sweep, &refc));
        assert!(cut_has_advanced_product_sources(
            &sweep,
            DerivedSweepProduct::CorrectedReflectivity
        ));
    }

    #[test]
    fn derive_on_demand_never_admits_kdp() {
        // KDP's moment type is the NATIVE SpecificDifferentialPhase, so it can
        // never reach the Unknown-gated derive-on-demand arm. A cut carrying
        // PHI could compute KDP, but selecting "KDP" still requires real KDP.
        let sweep = cut(720, &[MomentType::DifferentialPhase]);
        assert_eq!(
            DerivedSweepProduct::Kdp.moment_type(),
            MomentType::SpecificDifferentialPhase
        );
        assert!(advanced_derived_product_for_moment(&MomentType::SpecificDifferentialPhase).is_none());
        assert!(!cut_can_materialize_moment(
            &sweep,
            &MomentType::SpecificDifferentialPhase
        ));
    }

    #[test]
    fn native_moments_route_straight_through_the_presence_gate() {
        let sweep = cut(720, &[MomentType::Reflectivity]);
        for moment in [
            MomentType::Reflectivity,
            MomentType::Velocity,
            MomentType::SpectrumWidth,
            MomentType::DifferentialReflectivity,
            MomentType::CorrelationCoefficient,
            MomentType::DifferentialPhase,
            MomentType::SpecificDifferentialPhase,
        ] {
            assert_eq!(
                cut_can_materialize_moment(&sweep, &moment),
                cut_has_moment_source(&sweep, &moment),
                "{moment:?} must not take the derive-on-demand arm"
            );
        }
    }

    #[test]
    fn a_partial_sweep_carries_no_sources() {
        // 720 radials declared, but the grid only filled 10 rows.
        let mut sweep = cut(720, &[]);
        sweep.moments.insert(
            MomentType::Reflectivity,
            grid(MomentType::Reflectivity, 10, 8),
        );
        assert!(!cut_has_moment_source(&sweep, &MomentType::Reflectivity));
        assert!(!cut_has_advanced_product_sources(
            &sweep,
            DerivedSweepProduct::HailSignature
        ));
    }

    #[test]
    fn unknown_names_that_match_nothing_are_not_derivable() {
        let sweep = cut(720, &[MomentType::Reflectivity]);
        let bogus = MomentType::Unknown("NOT_A_PRODUCT".to_owned());
        assert!(advanced_derived_product_for_moment(&bogus).is_none());
        assert!(!cut_can_materialize_moment(&sweep, &bogus));
    }

    #[test]
    fn product_id_lookup_is_case_insensitive() {
        assert_eq!(
            advanced_derived_product_for_moment(&MomentType::Unknown("refc".to_owned())),
            Some(DerivedSweepProduct::CorrectedReflectivity)
        );
    }
}
