//! Storm tracking over time: cell identification and tracking, rotation
//! tracks, max-value swaths, and temporal grid combinations.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod cells;
mod swath;
mod temporal;
mod tracking;
pub mod tracks;

pub use cells::{StormCell, identify_storm_cells};
pub use swath::{SwathAggregation, base_tilt_cut, max_value_swath};
pub use temporal::{
    accumulate_rate_grids, difference_grid, exceedance_duration_grid, exceedance_probability_grid,
    maximum_swath_grid, mean_grid, minimum_swath_grid, trend_grid,
};
pub use tracking::{StormTrack, StormTracker, TIME_GATE_S};
