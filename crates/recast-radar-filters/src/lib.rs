//! Radar gate filters, polar smoothing and display interpolation.
//!
//! Every function works on the FM301 model of `recast-radar-core`: a
//! [`Sweep`](recast_radar_core::Sweep) supplies the ray coordinates and the
//! range coordinate, a [`recast_radar_core::Field`] the values in its native
//! gate geometry. Outputs are new physical (`F32`) fields.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
#![cfg_attr(recast_legacy_deprecation, deny(deprecated))]

mod gate_filter;
mod interpolate;
pub mod legacy_api;
mod smooth;

pub use gate_filter::apply_reflectivity_gate_filter;
pub use interpolate::{
    INTERP_MAX_AZIMUTH_HALF_WIDTH_DEG, INTERP_MAX_FACTOR, INTERP_MAX_GATES, INTERP_MAX_GRID_BYTES,
    INTERP_ROW_LIMIT, INTERP_TARGET_AZIMUTH_DEG, INTERP_TARGET_GATE_SPACING_M, UpsampleFactors,
    UpsampledSweep, upsample_factors, upsample_field,
};
pub use smooth::smooth_field;

// Legacy signatures, kept until the FM301 shim is removed
// (docs/design/fm301-model.md section 13.3).
#[allow(deprecated)]
pub use legacy_api::*;

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

#[cfg(test)]
pub(crate) mod test_support {
    use recast_radar_core::{
        Field, FieldData, FieldName, FloatCoding, RayVariables, Sweep, SweepMode,
    };

    /// A sealed PPI sweep with uniform gates (first centre `first_center_m`,
    /// `spacing_m`) and one physical field per `(name, values)` entry.
    pub(crate) fn sweep_with(
        azimuths: &[f32],
        first_center_m: f64,
        spacing_m: f64,
        gates: usize,
        fields: Vec<(FieldName, Vec<f32>)>,
        nyquist_mps: Option<f32>,
    ) -> Sweep {
        let mut sweep = Sweep::new(0, SweepMode::AzimuthSurveillance, 0.5);
        for (ray, azimuth) in azimuths.iter().enumerate() {
            sweep.push_ray(ray as f64, *azimuth, 0.5);
        }
        sweep.ray_vars = RayVariables {
            nyquist_velocity_mps: nyquist_mps.map(|n| vec![n; azimuths.len()]),
            ..RayVariables::default()
        };
        for (name, values) in fields {
            let gates_mapping = sweep
                .attach_geometry(first_center_m, spacing_m, gates as u32)
                .unwrap();
            let field = Field::new(
                name,
                gates_mapping,
                gates as u32,
                FieldData::F32 {
                    values,
                    coding: FloatCoding::default(),
                },
            );
            sweep.add_field(field).unwrap();
        }
        sweep.seal().unwrap();
        sweep
    }
}
