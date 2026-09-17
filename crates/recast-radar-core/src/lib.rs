//! Core data model for recast-radar-tools.
//!
//! - [`model`]: the WMO FM301 / CfRadial 2 data model (`Volume`, `Sweep`,
//!   `Field`) with compact raw storage and lazy scaling
//!   (`docs/design/fm301-model.md`).
//! - [`fm301`]: the FM301 group view over a [`model::Volume`], the conformance
//!   surface for xradar `DataTree` and CfRadial 2 output.
//! - [`legacy`]: the pre-FM301 model (`RadarVolume`, `ElevationCut`,
//!   `MomentGrid`, ...) and exact conversions to and from the FM301 model. Its
//!   items stay re-exported at their old paths until FM301 migration task F.3
//!   removes them.
//! - Beam geometry, refractivity and bounded decompression helpers shared by
//!   every crate.

pub mod bounded_read;
mod field_names;
pub mod fm301;
pub mod legacy;
pub mod model;
mod refractivity;

// Every legacy model name at its old path, so un-migrated code compiles
// unchanged (docs/design/fm301-model.md section 13.2).
#[allow(deprecated)]
pub use field_names::canonical_moment;
#[allow(deprecated)]
pub use legacy::{
    CUT_ELEVATION_MATCH_TOLERANCE_DEG, ElevationCut, GateRange, MergeReport, MomentGrid,
    MomentGridError, MomentRow, MomentStorage, MomentType, ProductId, RadarSite, RadarVolume,
    Radial, RadialStatus, RayInstrumentMetadata, RayInstrumentMetadataAlignmentError,
    ScanLegMetadata, ScanMode, VcpInfo, VolumeMetadata, merge_radar_volumes,
};
pub use model::{
    ArrayBuf, AttrValue, ExtraVariable, Field, FieldAttrs, FieldData, FieldName, FloatCoding, Gate,
    GateMapping, IntCoding, LinearTransform, Polarization, Quantity, RangeCoord, RayVariables,
    Rays, Scalar, SourceFormat, Sweep, SweepMode, Volume,
};
pub use refractivity::{
    EARTH_DUCTING_GRADIENT_N_PER_KM, PropagationRegime, RefractedBeamError, RefractedBeamPoint,
    RefractedBeamTrace, RefractivityLevel, RefractivityProfile, RefractivityProfileError,
    STANDARD_REFRACTIVITY_GRADIENT_N_PER_KM, propagation_regime, radio_refractivity_n_units,
    trace_refracted_beam,
};

/// Earth's mean radius (m).
pub const EARTH_RADIUS_M: f64 = 6_371_000.0;
/// Effective Earth radius under the standard "4/3 Earth" refraction model
/// (Bean & Dutton 1968; the standard-atmosphere refractivity gradient).
pub const EFFECTIVE_EARTH_RADIUS_M: f64 = EARTH_RADIUS_M * 4.0 / 3.0;

/// Center height of the radar beam **above the antenna**, in metres, under the
/// 4/3-Earth-radius effective-radius approximation for atmospheric refraction.
///
/// Doviak & Zrnić (1993), *Doppler Radar and Weather Observations* (2nd ed.),
/// eq. 2.28b: `h = sqrt(r² + aₑ² + 2·r·aₑ·sin θ) − aₑ`, with `aₑ = 4/3·a`.
///
/// `slant_range_m` is range along the beam; `elevation_deg` the antenna
/// elevation angle. Add the antenna's MSL altitude to get beam MSL height.
pub fn beam_height_above_radar_m(slant_range_m: f64, elevation_deg: f64) -> f64 {
    let ae = EFFECTIVE_EARTH_RADIUS_M;
    let r = slant_range_m;
    let theta = elevation_deg.to_radians();
    (r * r + ae * ae + 2.0 * r * ae * theta.sin()).sqrt() - ae
}

/// Great-circle (ground) distance from the radar to the gate, in metres, under
/// the same 4/3-Earth model. Doviak & Zrnić (1993) eq. 2.28c.
pub fn beam_ground_range_m(slant_range_m: f64, elevation_deg: f64) -> f64 {
    let ae = EFFECTIVE_EARTH_RADIUS_M;
    let r = slant_range_m;
    let theta = elevation_deg.to_radians();
    let h = beam_height_above_radar_m(r, elevation_deg);
    ae * ((r * theta.cos()) / (ae + h)).asin()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beam_height_matches_four_thirds_earth_reference() {
        // At 0° elevation, h ≈ r²/(2·aₑ): 100 km -> ~588 m.
        let h0 = beam_height_above_radar_m(100_000.0, 0.0);
        assert!((h0 - 588.6).abs() < 3.0, "0° 100km height was {h0}");

        // At 0.5° elevation, add ~r·sin(0.5°) ≈ 873 m -> ~1461 m.
        let h05 = beam_height_above_radar_m(100_000.0, 0.5);
        assert!((h05 - 1461.0).abs() < 5.0, "0.5° 100km height was {h05}");

        // Origin and monotonicity in range.
        assert!(beam_height_above_radar_m(0.0, 0.5).abs() < 1.0);
        assert!(
            beam_height_above_radar_m(200_000.0, 0.5) > beam_height_above_radar_m(100_000.0, 0.5)
        );
    }

    #[test]
    fn ground_range_close_to_slant_range_at_low_tilt() {
        // At low elevation the ground range is only slightly less than slant range.
        let s = beam_ground_range_m(100_000.0, 0.5);
        assert!(s > 99_000.0 && s < 100_000.0, "ground range was {s}");
    }
}
