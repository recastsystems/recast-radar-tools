//! Temporal grid combinations over real co-registered sweeps.
//!
//! Input: the eleven NOXP 0.5 deg sector sweeps of 2009-05-25 20:35:29-20:51:27Z
//! with identical geometry (171 rays, 1002 x 150 m gates, about 90 s apart) from
//! the Zenodo VORTEX2 archive `dorade-noxp-20090525-sweeps-tgz`. Expected
//! values: `testdata/golden/track/temporal.json`, written by
//! `tools/track_golden.py` (section `temporal`) with numpy from the DORADE
//! walker's reflectivity and the SSWB sweep start times (float32 arithmetic
//! where the Rust code uses f32).

// Test code panics on purpose: the workspace's unwrap/expect lints guard library code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{
    array, as_f64, as_i64, as_str, as_usize, assert_grid_matches, cell, dorade, golden,
    noxp_sweeps, sweep_bytes,
};
use recast_radar_core::{MomentGrid, MomentType, RadarVolume};
use recast_radar_testdata::require_file;
use recast_radar_track::{
    accumulate_rate_grids, difference_grid, exceedance_duration_grid, exceedance_probability_grid,
    maximum_swath_grid, mean_grid, minimum_swath_grid, trend_grid,
};

/// The eleven equal-geometry sweeps named by the golden, in time order.
fn frames(expected: &serde_json::Value, archive: &std::path::Path) -> Vec<RadarVolume> {
    let sweeps = noxp_sweeps(archive);
    let frames: Vec<RadarVolume> = array(&expected["members"])
        .iter()
        .map(|member| {
            let name = as_str(member);
            dorade(name, sweep_bytes(&sweeps, name))
        })
        .collect();
    let starts = array(&expected["start_unix"]);
    assert_eq!(frames.len(), starts.len());
    for (frame, start) in frames.iter().zip(starts) {
        assert_eq!(
            frame.volume_time.timestamp(),
            as_i64(start),
            "sweep start time"
        );
        assert_eq!(frame.cuts.len(), 1);
        assert_eq!(frame.cuts[0].radials.len(), as_usize(&expected["rays"]));
        let grid = reflectivity(frame);
        assert_eq!(
            grid.gate_range.gate_count,
            as_usize(&expected["gate_count"])
        );
        assert_eq!(
            i64::from(grid.gate_range.first_gate_m),
            as_i64(&expected["first_gate_m"])
        );
        assert_eq!(
            i64::from(grid.gate_range.gate_spacing_m),
            as_i64(&expected["gate_spacing_m"])
        );
    }
    let valid = array(&expected["valid_gates"]);
    for (frame, count) in frames.iter().zip(valid) {
        let grid = reflectivity(frame);
        let finite = (0..grid.radial_count())
            .flat_map(|row| (0..grid.gate_range.gate_count).map(move |gate| (row, gate)))
            .filter(|&(row, gate)| cell(grid, row, gate).is_some())
            .count();
        assert_eq!(finite, as_usize(count), "valid reflectivity gates");
    }
    frames
}

fn reflectivity(frame: &RadarVolume) -> &MomentGrid {
    &frame.cuts[0].moments[&MomentType::Reflectivity]
}

fn seconds(frame: &RadarVolume) -> f64 {
    frame.volume_time.timestamp() as f64
}

fn output(name: &str) -> MomentType {
    MomentType::Unknown(name.to_owned())
}

/// Difference and hourly trend between the 20:36:59 and 20:35:29Z sweeps: the
/// numpy per-gate difference where both sweeps have data, empty elsewhere; the
/// trend is the difference over 90 s scaled to an hour. Non-positive or
/// non-finite elapsed times give no trend, and the 170-ray sweep before them is
/// a geometry mismatch that gives no difference at all.
#[test]
fn difference_and_trend() {
    let golden = golden("temporal.json");
    let expected = &golden["noxp"];
    let archive = require_file!(as_str(&expected["archive"]));
    let frames = frames(expected, &archive);
    let summary = &expected["difference"];
    let newer = reflectivity(&frames[as_usize(&summary["newer"])]);
    let older = reflectivity(&frames[as_usize(&summary["older"])]);
    let difference = difference_grid(newer, older, output("DIFF")).expect("same geometry");
    assert_eq!(difference.moment, output("DIFF"));
    assert_eq!(difference.gate_range, newer.gate_range);
    assert_eq!(difference.radial_indices, newer.radial_indices);
    assert_grid_matches(&difference, summary, "difference");

    let summary = &expected["trend"];
    let elapsed = as_f64(&summary["elapsed_s"]);
    assert_eq!(seconds(&frames[1]) - seconds(&frames[0]), elapsed);
    let trend = trend_grid(newer, older, elapsed, output("TREND")).expect("positive elapsed");
    assert_grid_matches(&trend, summary, "trend");
    assert!(trend_grid(newer, older, 0.0, output("TREND")).is_none());
    assert!(trend_grid(newer, older, -elapsed, output("TREND")).is_none());
    assert!(trend_grid(newer, older, f64::NAN, output("TREND")).is_none());

    // The 20:33:47Z sweep has 170 rays: its radial indices differ, so it cannot
    // be combined with the 171-ray sweeps.
    let sweeps = noxp_sweeps(&archive);
    let short = dorade(
        "swp.1090525203347.NOXPRVP.0.0.5_PPI_v1",
        sweep_bytes(&sweeps, "swp.1090525203347.NOXPRVP.0.0.5_PPI_v1"),
    );
    assert_eq!(short.cuts[0].radials.len(), 170);
    assert!(difference_grid(older, reflectivity(&short), output("DIFF")).is_none());
    assert!(trend_grid(older, reflectivity(&short), 100.0, output("TREND")).is_none());
}

/// Trapezoid integration of the first four sweeps (reflectivity standing in
/// for a rate, negatives clamped to zero) between their SSWB start times: the
/// numpy trapezoid sum per gate over the windows where both ends have data,
/// empty where no window had both. Fewer than two frames or non-increasing
/// times give nothing.
#[test]
fn rate_accumulation_uses_trapezoids() {
    let golden = golden("temporal.json");
    let expected = &golden["noxp"];
    let archive = require_file!(as_str(&expected["archive"]));
    let frames = frames(expected, &archive);
    let summary = &expected["accumulation"];
    let count = as_usize(&summary["frames"]);
    let timed: Vec<(&MomentGrid, f64)> = frames[..count]
        .iter()
        .map(|frame| (reflectivity(frame), seconds(frame)))
        .collect();
    let accumulation = accumulate_rate_grids(&timed, output("ACCUM")).expect("four frames");
    assert_grid_matches(&accumulation, summary, "accumulation");
    assert!(accumulate_rate_grids(&timed[..1], output("ACCUM")).is_none());
    let mut reversed = timed.clone();
    reversed.reverse();
    assert!(accumulate_rate_grids(&reversed, output("ACCUM")).is_none());
    let mut stalled = timed.clone();
    stalled[1].1 = stalled[0].1;
    assert!(accumulate_rate_grids(&stalled, output("ACCUM")).is_none());
}

/// Fraction of the eleven sweeps at or above 40 dBZ per gate, over the sweeps
/// that have data there: gates missing in some sweeps are scored on the
/// remaining ones (the golden's partial samples carry the valid and exceeding
/// counts), and gates with no data anywhere stay empty.
#[test]
fn probability_ignores_missing_values() {
    let golden = golden("temporal.json");
    let expected = &golden["noxp"];
    let archive = require_file!(as_str(&expected["archive"]));
    let frames = frames(expected, &archive);
    let summary = &expected["probability"];
    let grids: Vec<&MomentGrid> = frames.iter().map(reflectivity).collect();
    let threshold = as_f64(&summary["threshold_dbz"]) as f32;
    let probability =
        exceedance_probability_grid(&grids, threshold, output("PROB")).expect("same geometry");
    assert_grid_matches(&probability, summary, "probability");
    let mut full = 0usize;
    let mut zero = 0usize;
    for row in 0..probability.radial_count() {
        for gate in 0..probability.gate_range.gate_count {
            match cell(&probability, row, gate) {
                Some(100.0) => full += 1,
                Some(0.0) => zero += 1,
                _ => {}
            }
        }
    }
    assert_eq!(full, as_usize(&summary["count_100"]));
    assert_eq!(zero, as_usize(&summary["count_0"]));
    assert!(as_usize(&summary["partial_count"]) > 100);
    for sample in array(&summary["partial_samples"]) {
        let sample = array(sample);
        let (row, gate) = (as_usize(&sample[0]), as_usize(&sample[1]));
        let (valid, exceeded) = (as_usize(&sample[3]), as_usize(&sample[4]));
        assert!(valid < grids.len() && exceeded > 0);
        let present = grids
            .iter()
            .filter(|grid| cell(grid, row, gate).is_some())
            .count();
        assert_eq!(present, valid, "sweeps with data at ({row}, {gate})");
        let actual = cell(&probability, row, gate).expect("scored");
        let expected = 100.0 * exceeded as f32 / valid as f32;
        assert!(
            (actual - expected).abs() < 1e-4,
            "({row}, {gate}): {actual} != {exceeded}/{valid} of the sweeps with data"
        );
    }
    assert!(exceedance_probability_grid(&[], threshold, output("PROB")).is_none());
}

/// Maximum, minimum and mean over the eleven sweeps, and minutes at or above
/// 40 dBZ over the first four (full credit when both ends of a window exceed,
/// half when one does), against the numpy references.
#[test]
fn maximum_minimum_mean_and_duration_match_the_reference() {
    let golden = golden("temporal.json");
    let expected = &golden["noxp"];
    let archive = require_file!(as_str(&expected["archive"]));
    let frames = frames(expected, &archive);
    let grids: Vec<&MomentGrid> = frames.iter().map(reflectivity).collect();
    let maximum = maximum_swath_grid(&grids, output("MAX")).expect("same geometry");
    assert_grid_matches(&maximum, &expected["maximum"], "maximum");
    let minimum = minimum_swath_grid(&grids, output("MIN")).expect("same geometry");
    assert_grid_matches(&minimum, &expected["minimum"], "minimum");
    let mean = mean_grid(&grids, output("MEAN")).expect("same geometry");
    assert_grid_matches(&mean, &expected["mean"], "mean");

    let summary = &expected["duration"];
    let count = as_usize(&summary["frames"]);
    let timed: Vec<(&MomentGrid, f64)> = frames[..count]
        .iter()
        .map(|frame| (reflectivity(frame), seconds(frame)))
        .collect();
    let threshold = as_f64(&summary["threshold_dbz"]) as f32;
    let duration = exceedance_duration_grid(&timed, threshold, output("DUR")).expect("four frames");
    assert_grid_matches(&duration, summary, "duration");
    assert!(exceedance_duration_grid(&timed[..1], threshold, output("DUR")).is_none());
}
