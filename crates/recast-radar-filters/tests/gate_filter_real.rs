//! Reflectivity gate filter on real volumes.
//!
//! Expected values: `testdata/golden/filters/gate_filter.json`, written by
//! `tools/filters_map_golden.py gate_filter` with Py-ART 2.2.5
//! (`GateFilter.exclude_below('reflectivity', threshold)` applied to Py-ART's
//! velocity field) and MetPy 1.7.1.

mod common;

use common::{array, as_f64, as_usize, assert_close, cell, golden, row_stats};
use recast_radar_core::Quantity;
use recast_radar_filters::apply_reflectivity_gate_filter;

/// KTLX 2024-03-15 0.48 deg Doppler cut (sweep 2 of the split cut: REF, VEL
/// and SW on the same radials): for each threshold, the velocity gates that
/// survive per radial and their sum equal Py-ART's gate filter.
#[test]
fn keeps_velocity_only_where_reflectivity_clears_the_threshold() {
    let expected = &golden("filters/gate_filter.json")["doppler_cut"];
    let path = recast_radar_testdata::require_file!("l2-ktlx-20240315-000217-trim");
    let volume = common::level2(&path);
    let cut = &volume.sweeps[as_usize(&expected["sweep"])];
    let velocity = cut.find(Quantity::RadialVelocity).expect("VEL");
    assert_eq!(velocity.shape().0, as_usize(&expected["rays"]));
    let valid_velocity: usize = row_stats(velocity).iter().map(|(count, _)| count).sum();
    assert_eq!(
        valid_velocity,
        as_usize(&expected["velocity_valid_total"]),
        "decoded velocity gates"
    );

    let cases = array(&expected["thresholds"]);
    let mut kept_totals = Vec::new();
    for case in cases {
        let threshold = as_f64(&case["threshold_dbz"]) as f32;
        let filtered = apply_reflectivity_gate_filter(cut, velocity, threshold);
        assert_eq!(filtered.shape(), velocity.shape());
        assert_eq!(filtered.gates, velocity.gates);
        assert_eq!(filtered.absent_rows, velocity.absent_rows);

        // A kept gate carries the unfiltered velocity unchanged.
        for row in 0..filtered.shape().0 {
            for gate in 0..filtered.shape().1 {
                if let Some(value) = cell(&filtered, row, gate) {
                    assert_eq!(
                        Some(value),
                        cell(velocity, row, gate),
                        "row {row} gate {gate}"
                    );
                }
            }
        }

        let stats = row_stats(&filtered);
        let kept_per_ray = array(&case["kept_per_ray"]);
        let kept_sum_per_ray = array(&case["kept_sum_per_ray"]);
        for (row, (count, sum)) in stats.iter().enumerate() {
            assert_eq!(
                *count,
                as_usize(&kept_per_ray[row]),
                "threshold {threshold} dBZ, ray {row}: kept gates"
            );
            assert_close(
                *sum,
                as_f64(&kept_sum_per_ray[row]),
                1e-6,
                &format!("threshold {threshold} dBZ, ray {row}: kept velocity sum"),
            );
        }
        let kept: usize = stats.iter().map(|(count, _)| count).sum();
        assert_eq!(kept, as_usize(&case["kept_total"]));
        kept_totals.push(kept);
    }
    // The thresholds bite on this sweep: 88294 velocity gates, 59271 with
    // reflectivity >= 0 dBZ, 6082 at 10 dBZ and 1313 at 20 dBZ.
    assert!(kept_totals.windows(2).all(|pair| pair[0] > pair[1]));
    assert!(kept_totals[0] < valid_velocity);
}

/// Cuts without a reflectivity moment blank every gate: the Message 1 Doppler
/// sweep of the 1999 KTLX split cut (MetPy: VEL and SW only) and every sweep
/// of the JMA radial-velocity (N6) product.
#[test]
fn no_reflectivity_moment_blanks_everything() {
    let expected = golden("filters/gate_filter.json");

    let legacy = &expected["velocity_only_cut"];
    let path = recast_radar_testdata::require_file!("l2-ktlx-19990504-002218-trim");
    let volume = common::level2(&path);
    let cut = &volume.sweeps[as_usize(&legacy["sweep"])];
    assert!(cut.find(Quantity::Reflectivity).is_none());
    let velocity = cut.find(Quantity::RadialVelocity).expect("VEL");
    let valid: usize = row_stats(velocity).iter().map(|(count, _)| count).sum();
    assert_eq!(valid, as_usize(&legacy["velocity_valid_total"]));
    let filtered = apply_reflectivity_gate_filter(cut, velocity, -30.0);
    assert!(row_stats(&filtered).iter().all(|(count, _)| *count == 0));

    let jma = &expected["velocity_only_volume"];
    let path = recast_radar_testdata::require_file!("jma-n6-20191012-090000-rs47773");
    let volume = common::jma(&path);
    assert_eq!(volume.sweeps.len(), as_usize(&jma["sweeps"]));
    let mut valid_total = 0;
    for (index, cut) in volume.sweeps.iter().enumerate() {
        assert!(cut.find(Quantity::Reflectivity).is_none());
        let velocity = cut.find(Quantity::RadialVelocity).expect("VEL");
        valid_total += row_stats(velocity)
            .iter()
            .map(|(count, _)| count)
            .sum::<usize>();
        let filtered = apply_reflectivity_gate_filter(cut, velocity, -30.0);
        assert_eq!(filtered.shape(), velocity.shape());
        assert_eq!(filtered.gates, velocity.gates);
        assert!(
            row_stats(&filtered).iter().all(|(count, _)| *count == 0),
            "sweep {index} kept velocity without reflectivity"
        );
    }
    assert_eq!(valid_total, as_usize(&jma["velocity_valid_total"]));
}
