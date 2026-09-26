//! Radar geometry products: volume column walks (composites, echo tops, VIL,
//! hail), cross sections, volume box resampling, native RHI panels, and
//! Cartesian gridding of one or more volumes ([`grid_from_volumes`], the
//! algorithm of Py-ART's `grid_from_radars`).
//!
//! Every function works on the FM301 model of `recast-radar-core`
//! (`docs/design/fm301-model.md`): a [`recast_radar_core::Volume`] of
//! [`recast_radar_core::Sweep`]s whose [`recast_radar_core::Field`]s carry
//! the values in their native gate geometry. Column products return physical `F32` fields on the base
//! sweep's rays and native gates, named by product id.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod grid;
mod rhi;
mod volumetric;

pub use grid::{
    CartesianGrid, GridError, GridField, GridOptions, GridOrigin, GridSpec, GridWeighting,
    MAX_GRID_CELLS, RadiusOfInfluence, grid_from_volumes,
};
pub use rhi::{
    rhi_coverage_range, rhi_coverage_top, rhi_fixed_azimuth, rhi_panel, sweep_looks_like_rhi,
};
pub use volumetric::{
    CrossSection, CrossSectionSmoothing, ECHO_TOP_THRESHOLD_DBZ, HailFields, InterpPolicy,
    MeshCalibration, VolumeDealiasCache, box_resample, box_resample_field, composite_reflectivity,
    echo_top, field_section, field_section_with_smoothing, hail, mehs, poh, reflectivity_section,
    reflectivity_section_with_smoothing, velocity_section, velocity_section_cached,
    velocity_section_cached_with_smoothing, vil, vil_density,
};
