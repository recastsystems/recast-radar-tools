//! Radar gate filters, polar smoothing and display interpolation.
//!
//! Every function works on the FM301 model of `recast-radar-core`: a
//! [`Sweep`](recast_radar_core::Sweep) supplies the ray coordinates and the
//! range coordinate, a [`recast_radar_core::Field`] the values in its native
//! gate geometry. Outputs are new physical (`F32`) fields.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod gate_filter;
mod interpolate;
mod smooth;

pub use gate_filter::apply_reflectivity_gate_filter;
pub use interpolate::{
    INTERP_MAX_AZIMUTH_HALF_WIDTH_DEG, INTERP_MAX_FACTOR, INTERP_MAX_GATES, INTERP_MAX_GRID_BYTES,
    INTERP_ROW_LIMIT, INTERP_TARGET_AZIMUTH_DEG, INTERP_TARGET_GATE_SPACING_M, UpsampleFactors,
    UpsampledSweep, upsample_factors, upsample_field,
};
pub use smooth::smooth_field;

use recast_radar_core::{Field, FieldAttrs, FieldData, FloatCoding};

/// Interpolation policy per moment family (docs/xsection-3d-spec.md):
/// reflectivity/ZDR blend linearly; CC must not blend through the melting
/// layer (Giangrande, Krause & Ryzhkov 2008: the rho_hv minimum is the
/// signature — blending fabricates intermediate values), so any bracket
/// below 0.97 falls back to nearest-gate; velocity guards against blending
/// across strong shear or residual aliasing.
#[derive(Clone, Copy, PartialEq)]
pub enum InterpPolicy {
    LinearAngle,
    CcGuard,
    VelocityGuard,
}

/// A physical `F32` field (NaN = no data) of `nrays × ngates` that keeps
/// `base`'s name, class, CF name and units and gate mapping.
pub(crate) fn physical_field_like(
    base: &Field,
    nrays: u32,
    ngates: u32,
    values: Vec<f32>,
) -> Field {
    debug_assert_eq!(values.len(), nrays as usize * ngates as usize);
    Field {
        name: base.name.clone(),
        quantity: base.quantity,
        polarization: base.polarization,
        attrs: FieldAttrs {
            standard_name: base.attrs.standard_name.clone(),
            long_name: base.attrs.long_name.clone(),
            units: base.attrs.units.clone(),
            ..FieldAttrs::default()
        },
        nrays,
        ngates,
        gates: base.gates,
        data: FieldData::F32 {
            values,
            coding: FloatCoding::default(),
        },
        absent_rows: Vec::new(),
    }
}
