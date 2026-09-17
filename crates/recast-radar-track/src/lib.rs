//! Storm tracking over time: cell identification and tracking, rotation
//! tracks, max-value swaths, and temporal grid combinations.
//!
//! Every function works on the FM301 model of `recast-radar-core`
//! (`docs/design/fm301-model.md`): [`Volume`](recast_radar_core::Volume)s of
//! [`Sweep`](recast_radar_core::Sweep)s whose
//! [`Field`](recast_radar_core::Field)s carry the values in their native
//! gate geometry.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
#![cfg_attr(recast_legacy_deprecation, deny(deprecated))]

mod cells;
pub mod legacy_api;
mod swath;
mod temporal;
mod tracking;
pub mod tracks;

pub use cells::{StormCell, identify_storm_cells};
pub use swath::{SwathAggregation, base_tilt_sweep, value_swath};
pub use temporal::{
    accumulate_rates, difference, exceedance_duration, exceedance_probability, maximum_swath, mean,
    minimum_swath, trend,
};
pub use tracking::{StormTrack, StormTracker, TIME_GATE_S};

// Legacy signatures, kept until the FM301 shim is removed
// (docs/design/fm301-model.md section 13.3).
#[allow(deprecated)]
pub use legacy_api::*;

use recast_radar_core::{Field, FieldData, FieldName, FloatCoding, Quantity};

/// A physical `F32` field named `name` on `base`'s rays, native gates and
/// absent rows (NaN = no data).
pub(crate) fn physical_field_like(base: &Field, name: FieldName, values: Vec<f32>) -> Field {
    debug_assert_eq!(values.len(), base.nrays as usize * base.ngates as usize);
    let (quantity, polarization) = Quantity::classify(name.as_str(), None);
    Field {
        name,
        quantity,
        polarization,
        attrs: Default::default(),
        nrays: base.nrays,
        ngates: base.ngates,
        gates: base.gates,
        data: FieldData::F32 {
            values,
            coding: FloatCoding::default(),
        },
        absent_rows: base.absent_rows.clone(),
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use recast_radar_core::{
        Field, FieldData, FieldName, FloatCoding, RayVariables, Sweep, SweepMode, Volume,
    };

    /// An unsealed PPI sweep at `elevation_deg` of `rows` evenly spaced rays
    /// with one physical `F32` field of `values` on uniform gates.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sweep_with_f32_field(
        elevation_deg: f32,
        rows: usize,
        nyquist_mps: Option<f32>,
        name: FieldName,
        first_center_m: f64,
        spacing_m: f64,
        gates: usize,
        values: Vec<f32>,
    ) -> Sweep {
        let mut sweep = Sweep::new(0, SweepMode::AzimuthSurveillance, elevation_deg);
        for row in 0..rows {
            sweep.push_ray(
                row as f64,
                row as f32 * (360.0 / rows.max(1) as f32),
                elevation_deg,
            );
        }
        sweep.ray_vars = RayVariables {
            nyquist_velocity_mps: nyquist_mps.map(|nyquist| vec![nyquist; rows]),
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
