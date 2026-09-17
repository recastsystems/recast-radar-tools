//! Radar geometry products: volume column walks (composites, echo tops, VIL,
//! hail), cross sections, volume box resampling, and native RHI panels.
//!
//! Every function works on the FM301 model of `recast-radar-core`
//! (`docs/design/fm301-model.md`): a [`recast_radar_core::Volume`] of
//! [`recast_radar_core::Sweep`]s whose [`recast_radar_core::Field`]s carry
//! the values in their native gate geometry. Column products return physical `F32` fields on the base
//! sweep's rays and native gates, named by product id.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
#![cfg_attr(recast_legacy_deprecation, deny(deprecated))]

pub mod legacy_api;
mod rhi;
mod volumetric;

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

// Legacy signatures, kept until the FM301 shim is removed
// (docs/design/fm301-model.md section 13.3).
#[allow(deprecated)]
pub use legacy_api::*;

#[cfg(test)]
pub(crate) mod test_support {
    use recast_radar_core::{
        Field, FieldData, FieldName, FloatCoding, RayVariables, Sweep, SweepMode, Volume,
    };

    /// An unsealed sweep in `mode` at `fixed_angle_deg` with the given
    /// `(azimuth, elevation)` rays, a per-ray Nyquist velocity when given,
    /// and one physical `F32` field of `values` with native geometry
    /// (`first_center_m`, `spacing_m`, `gates`).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sweep_with_rays(
        mode: SweepMode,
        fixed_angle_deg: f32,
        rays: &[(f32, f32)],
        nyquist_mps: Option<f32>,
        name: FieldName,
        first_center_m: f64,
        spacing_m: f64,
        gates: usize,
        values: Vec<f32>,
    ) -> Sweep {
        let mut sweep = Sweep::new(0, mode, fixed_angle_deg);
        for (row, (azimuth, elevation)) in rays.iter().enumerate() {
            sweep.push_ray(row as f64, *azimuth, *elevation);
        }
        sweep.ray_vars = RayVariables {
            nyquist_velocity_mps: nyquist_mps.map(|nyquist| vec![nyquist; rays.len()]),
            ..RayVariables::default()
        };
        let mapping = sweep
            .attach_geometry(first_center_m, spacing_m, gates as u32)
            .expect("aligned geometry");
        let field = Field::new(
            name,
            mapping,
            gates as u32,
            FieldData::F32 {
                values,
                coding: FloatCoding::default(),
            },
        );
        sweep.add_field(field).expect("unique field name");
        sweep
    }

    /// A PPI sweep at `elevation_deg` of `az_count` evenly spaced rays with
    /// one physical field.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sweep_with_field(
        elevation_deg: f32,
        az_count: usize,
        nyquist_mps: Option<f32>,
        name: FieldName,
        first_center_m: f64,
        spacing_m: f64,
        gates: usize,
        values: Vec<f32>,
    ) -> Sweep {
        let rays: Vec<(f32, f32)> = (0..az_count)
            .map(|k| (k as f32 * (360.0 / az_count as f32), elevation_deg))
            .collect();
        sweep_with_rays(
            SweepMode::AzimuthSurveillance,
            elevation_deg,
            &rays,
            nyquist_mps,
            name,
            first_center_m,
            spacing_m,
            gates,
            values,
        )
    }

    /// A volume named `TEST` at the Unix epoch holding `sweeps`, numbered in
    /// order.
    pub(crate) fn volume_with(sweeps: Vec<Sweep>) -> Volume {
        let mut volume = Volume::new("TEST", chrono::DateTime::<chrono::Utc>::UNIX_EPOCH);
        volume.sweeps = sweeps;
        for (index, sweep) in volume.sweeps.iter_mut().enumerate() {
            sweep.sweep_number = index as u32;
        }
        volume
    }
}
