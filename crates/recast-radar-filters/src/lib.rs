//! Radar gate filters, polar smoothing and display interpolation.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod gate_filter;
mod interpolate;
mod smooth;

pub use gate_filter::apply_reflectivity_gate_filter;
pub use interpolate::{
    INTERP_MAX_AZIMUTH_HALF_WIDTH_DEG, INTERP_MAX_FACTOR, INTERP_MAX_GATES, INTERP_MAX_GRID_BYTES,
    INTERP_ROW_LIMIT, INTERP_TARGET_AZIMUTH_DEG, INTERP_TARGET_GATE_SPACING_M, InterpolatedGrid,
    UpsampleFactors, upsample_factors, upsample_moment_grid,
};
pub use smooth::smooth_moment_grid;

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
