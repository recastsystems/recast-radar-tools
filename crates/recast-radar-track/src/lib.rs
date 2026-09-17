//! Storm tracking over time: cell identification and tracking, rotation
//! tracks, max-value swaths, and temporal grid combinations.
//!
//! Every function works on the FM301 model of `recast-radar-core`
//! (`docs/design/fm301-model.md`): [`Volume`](recast_radar_core::Volume)s of
//! [`Sweep`](recast_radar_core::Sweep)s whose
//! [`recast_radar_core::Field`]s carry the values in their native gate
//! geometry.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod cells;
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
