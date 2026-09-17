//! Radar geometry products: volume column walks (composites, echo tops, VIL,
//! hail), cross sections, volume box resampling, and native RHI panels.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod rhi;
mod volumetric;

pub use rhi::{
    cut_looks_like_rhi, rhi_coverage_range_m, rhi_coverage_top_m, rhi_fixed_azimuth_deg,
    rhi_section,
};
pub use volumetric::{
    CrossSection, CrossSectionSmoothing, ECHO_TOP_THRESHOLD_DBZ, HailGrids, InterpPolicy,
    MeshCalibration, VolumeDealiasCache, composite_reflectivity_grid, echo_top_grid, hail_grids,
    mehs_grid, moment_cross_section, moment_cross_section_with_smoothing, poh_grid,
    reflectivity_cross_section, reflectivity_cross_section_with_smoothing, velocity_cross_section,
    velocity_cross_section_cached, velocity_cross_section_cached_with_smoothing, vil_density_grid,
    vil_grid, volume_box_resample, volume_box_resample_moment,
};
